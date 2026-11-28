use crate::paths::{canonical, owner_home, owner_secrets};
use crate::policy::{AccessGrant, AgentPolicy, GRANT_PORTS_MAX, GRANT_SOCKETS_MAX};
use crate::ports::{OWNER_PORTS, PROXY_PORTS, RELAY_PORTS};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[cfg(unix)]
const PER_USER_TEMP: &str = "/private/var/folders/";

/// The engine's parts the grant checks reach, passed in so this crate names no engine
/// module. The engine passes its one value; rows pass stand-ins.
pub struct GrantHooks {
    /// The folders and files in the owner's home that belong to a harness, which no socket
    /// grant may sit inside. No input. Output: the names.
    pub harness_folders: fn() -> Vec<&'static str>,
    /// The loopback ports the engine recorded for its projects' IDE servers and their
    /// proxies, which no port grant may take. Input: the engine's data folder. Output: the
    /// ports, none when there is no IDE folder.
    pub ide_ports: fn(&Path) -> Vec<u16>,
}

/// One row of a project's grant log as Agent access lists it: when, what happened, the
/// grant in the owner's words and who did it (`owner`, or `question <id>` for an Allow).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogView {
    pub at_ms: i64,
    pub action: String,
    pub grant: String,
    pub by: String,
}

/// The engine's own temp folder when it is the owner's private one: on macOS a folder
/// under `/private/var/folders/`, owned by the engine's user and closed to every other
/// user. A shared temp folder such as `/tmp` is never handed to agents. Takes nothing.
/// Returns the canonical folder, or `temp_folder_shared` or `temp_folder_needs_macos`.
pub fn owner_temp_folder() -> Result<PathBuf, &'static str> {
    if !cfg!(target_os = "macos") {
        return Err("temp_folder_needs_macos");
    }
    private_temp(&std::env::temp_dir())
}

#[cfg(unix)]
fn private_temp(folder: &Path) -> Result<PathBuf, &'static str> {
    use std::os::unix::fs::MetadataExt;
    let real = canonical(folder).map_err(|_| "temp_folder_shared")?;
    let meta = std::fs::symlink_metadata(&real).map_err(|_| "temp_folder_shared")?;
    let mine = meta.uid() == nix::unistd::getuid().as_raw();
    let private = meta.mode() & 0o077 == 0;
    let per_user = real
        .to_str()
        .is_some_and(|text| text.starts_with(PER_USER_TEMP));
    if !meta.is_dir() || !mine || !private || !per_user {
        return Err("temp_folder_shared");
    }
    assert!(real.is_absolute());
    Ok(real)
}

#[cfg(not(unix))]
fn private_temp(_folder: &Path) -> Result<PathBuf, &'static str> {
    Err("temp_folder_needs_macos")
}

/// The plain sentence for a grant refusal code, the same in the desktop app and the tool
/// reply. Input: the code. Output: the sentence.
pub fn refusal_text(code: &str) -> &'static str {
    match code {
        "temp_folder_shared" => {
            "Your temp folder is shared with other users on this computer, so it is never given to agents."
        }
        "temp_folder_needs_macos" => {
            "Your temp folder can be given to agents only on macOS in this version."
        }
        "grant_socket_needs_macos" => "A socket can be allowed only on macOS in this version.",
        "grant_socket_not_plain" => {
            "The socket path must be a full path of letters, digits, slashes, dots, dashes and underscores, at most 103 bytes, in a folder that exists."
        }
        "grant_socket_secret" => {
            "That socket sits with your keys or credentials, so it is never allowed."
        }
        "grant_socket_engine" => {
            "That socket belongs to CrazyAgent itself, so it is never allowed."
        }
        "grant_socket_agent" => {
            "That socket controls a terminal, an editor or an SSH connection (tmux, screen, dtach, Zellij, kitty, WezTerm, Alacritty, iTerm2, Emacs, Neovim, Kakoune, VS Code, Cursor and the like) or hands out your SSH keys, which can run anything as you, so it is never allowed."
        }
        "grant_socket_host" => {
            "That socket controls containers or virtual machines (Docker, containerd, Podman, Colima, Lima, OrbStack and the like), which can reach your whole computer, so it is never allowed."
        }
        "grant_socket_keys" => {
            "That socket belongs to a key or password agent (gpg-agent, 1Password, SSH or another agent), so it is never allowed."
        }
        "grant_socket_owner" => {
            "That socket belongs to the system or another user, has a second name, or sits in a folder other users can change, so it is never allowed."
        }
        "grant_socket_refused" => {
            "That socket cannot be allowed. A socket can be allowed only when it is there now, belongs to the owner with one name, sits in a folder only the owner can change, and is no socket of CrazyAgent itself, of keys or credentials, of containers or virtual machines, or of a terminal, editor, SSH connection or key agent. Check the path and that the program making it runs, then ask again."
        }
        "grant_socket_missing" => {
            "There is no socket at that path now. Start the program that makes it, then allow it again."
        }
        "grant_port_reserved" => {
            "That port belongs to your sign-in, your local model or CrazyAgent itself (its IDE or model proxy), or controls Docker, a browser or a debugger (Node, Java, Python or Go), so it is never allowed."
        }
        "grant_port_dynamic" => {
            "Ports from 49152 up are lent by the system to CrazyAgent's IDE and other apps for a while, so they are never allowed. Pick a fixed port below 49152."
        }
        "agent_grants_invalid" => {
            "A project allows at most 2 sockets, 4 ports and your temp folder, each once."
        }
        "grant_full" => "This project already allows as many extras of that kind as it can.",
        "grant_requests_full" => {
            "Four access requests of this project are still waiting for the owner."
        }
        "grant_reason_invalid" => "Give a reason in plain words.",
        _ => "The request could not be recorded.",
    }
}

/// The longest reason an agent may give with an access request; a longer one is cut.
pub const REASON_BYTES_MAX: usize = 300;

/// The ports the system lends out by itself on macOS (IANA dynamic range): the IDE's server
/// and proxy ports and other apps' picked ports live here, so no port grant falls in it.
pub const DYNAMIC_PORTS: std::ops::RangeInclusive<u16> = 49_152..=65_535;

/// The option an owner picks to allow a request, always the first.
pub const ALLOW: &str = "Allow";

/// The option an owner picks to refuse a request.
pub const DONT_ALLOW: &str = "Don't allow";

/// The asker name prefix of the questions access requests raise.
pub const ASKER_PREFIX: &str = "engine:access:";

/// The one refusal code an agent's access request gets for every failed check of a socket path it
/// wrote plainly.
pub const SOCKET_REFUSED: &str = "grant_socket_refused";

const LAUNCHD_LISTENERS: &str = "/private/tmp/com.apple.launchd.";

const HOST_ESCAPE_WORDS: [&str; 8] = [
    "docker",
    "containerd",
    "podman",
    "colima",
    "orbstack",
    "rancher",
    "buildkit",
    "nerdctl",
];

const HOST_ESCAPE_NAMES: [&str; 4] = ["lima", "finch", "rd", "utm"];

const KEY_WORDS: [&str; 10] = [
    "gnupg",
    "1password",
    "2bua8c4s2c",
    "keychain",
    "scdaemon",
    "keyboxd",
    "dirmngr",
    "bitwarden",
    "secretive",
    "keepass",
];

const CONTROL_WORDS: [&str; 16] = [
    "tmux",
    "emacs",
    "nvim",
    "zellij",
    "kitty",
    "wezterm",
    "alacritty",
    "iterm",
    "vscode",
    "codium",
    "cursor",
    "windsurf",
    "hammerspoon",
    "kakoune",
    "dtach",
    "abduco",
];

const CONTROL_NAMES: [&str; 4] = ["vim", "screen", "screens", "uscreens"];

const CONTROL_FOLDER_NAMES: [&str; 1] = ["code"];

const CONTROL_PORTS: [u16; 7] = [2345, 2375, 2376, 5005, 5678, 9222, 9229];

const SSH_MUX_HASH_HEX: usize = 40;

const KEY_STARTS: [&str; 2] = ["gpg", "ssh"];

const KEY_NAME_WORDS: [&str; 3] = ["agent", "gpg", "ssh"];

const _: () = assert!(GRANT_SOCKETS_MAX + GRANT_PORTS_MAX + 1 == crate::policy::GRANTS_MAX);

/// How an access request ended for the agent that made it: asked as an owner question,
/// or already allowed by the saved policy, so nothing was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Requested {
    Asked(u64),
    AlreadyAllowed,
}

/// Checks one grant the owner or an agent named, as a save, an Allow and each launch take
/// it: a socket as the canonical path of an existing socket with one name the user owns in
/// a folder no other user can change, never with the owner's credentials, in the engine's
/// data folder or a launchd listener, nor one of a container or VM host, a terminal or
/// editor, or a key agent; a port below the dynamic range other than the owner's fixed
/// ports, Docker's API and debugger ports, the model proxy ports and every IDE port the
/// engine recorded; the temp folder only when it is the owner's private one. Inputs: the
/// engine's hooks for the harness folders and IDE ports, the grant and the engine's data
/// folder. Output: the grant to keep, or a plain refusal code.
pub fn checked(
    hooks: &GrantHooks,
    grant: &AccessGrant,
    data_root: &Path,
) -> Result<AccessGrant, &'static str> {
    match grant {
        AccessGrant::Socket { path } => {
            if !cfg!(target_os = "macos") {
                return Err("grant_socket_needs_macos");
            }
            let real = socket_path(grant, path)?;
            let owner = owner_home().map_err(|_| "grant_socket_secret")?;
            if owner_secrets(&owner, &(hooks.harness_folders)())
                .any(|secret| real.starts_with(&secret))
            {
                return Err("grant_socket_secret");
            }
            let data = canonical(data_root).unwrap_or_else(|_| data_root.to_path_buf());
            if real.starts_with(&data) {
                return Err("grant_socket_engine");
            }
            let text = real.to_str().ok_or("grant_socket_not_plain")?;
            if text.starts_with(LAUNCHD_LISTENERS) {
                return Err("grant_socket_agent");
            }
            if let Some(code) = escape_class(&real) {
                return Err(code);
            }
            socket_fit(&real)?;
            let kept = AccessGrant::Socket { path: real };
            assert!(kept.plain());
            Ok(kept)
        }
        AccessGrant::Port { port } => {
            let reserved = *port == 0
                || OWNER_PORTS.contains(port)
                || CONTROL_PORTS.contains(port)
                || PROXY_PORTS.contains(port)
                || RELAY_PORTS.contains(port)
                || (hooks.ide_ports)(data_root).contains(port);
            if reserved {
                return Err("grant_port_reserved");
            }
            if DYNAMIC_PORTS.contains(port) {
                return Err("grant_port_dynamic");
            }
            Ok(grant.clone())
        }
        AccessGrant::TempFolder => owner_temp_folder().map(|_| AccessGrant::TempFolder),
    }
}

/// The grants of a saved policy a launch builds into the profile now: each still passes
/// [`checked`] unchanged, so a socket that is gone, changed hands or moved behind a link is
/// left out, and no port is one the launch keeps free for the owner's model endpoints.
/// Inputs: the engine's grant hooks, the saved grants, the ports the launch keeps free and
/// the engine's data folder. Output: the grants to build in, in order.
pub fn held_now(
    hooks: &GrantHooks,
    grants: &[AccessGrant],
    kept: &[u16],
    data_root: &Path,
) -> Vec<AccessGrant> {
    assert!(grants.len() <= crate::policy::GRANTS_MAX);
    let held: Vec<AccessGrant> = grants
        .iter()
        .filter(|grant| checked(hooks, grant, data_root).is_ok_and(|now| now == **grant))
        .filter(|grant| !matches!(grant, AccessGrant::Port { port } if kept.contains(port)))
        .cloned()
        .collect();
    if held.len() < grants.len() {
        eprintln!("agent_grants_left_out count={}", grants.len() - held.len());
    }
    held
}

/// The class of host-escape or key socket a path names, judged by its folder and file
/// names, lowercased: a container or VM host, a terminal, editor or SSH connection that
/// runs commands as the owner, or a key or password agent. Input: the canonical path. Output:
/// `grant_socket_host`, `grant_socket_agent`, `grant_socket_keys` or none.
fn escape_class(real: &Path) -> Option<&'static str> {
    let words: Vec<String> = real
        .components()
        .filter_map(|part| part.as_os_str().to_str())
        .map(|part| part.trim_start_matches('.').to_ascii_lowercase())
        .collect();
    if marked(&words, &HOST_ESCAPE_NAMES, &HOST_ESCAPE_WORDS) {
        return Some("grant_socket_host");
    }
    let folder = words
        .len()
        .checked_sub(2)
        .and_then(|at| words.get(at..=at))
        .unwrap_or_default();
    let name = words.last().map(String::as_str).unwrap_or_default();
    let control = marked(&words, &CONTROL_NAMES, &CONTROL_WORDS)
        || marked(folder, &CONTROL_FOLDER_NAMES, &[])
        || hashed(name);
    if control {
        return Some("grant_socket_agent");
    }
    let keys = words.iter().any(|word| {
        KEY_WORDS.iter().any(|mark| word.contains(mark))
            || KEY_STARTS.iter().any(|start| word.starts_with(start))
    }) || KEY_NAME_WORDS.iter().any(|mark| name.contains(mark));
    keys.then_some("grant_socket_keys")
}

/// Whether a socket's file name holds a run of at least `SSH_MUX_HASH_HEX` hex digits, the
/// `%C` hash ssh names a ControlMaster socket by. Input: the file name. Output: true when
/// it does.
fn hashed(name: &str) -> bool {
    name.split(|c: char| !c.is_ascii_hexdigit())
        .any(|run| run.len() >= SSH_MUX_HASH_HEX)
}

/// Whether any lowercased path part is one of the names, alone or followed by `-`, `_` or
/// `.`, or holds one of the marks anywhere. Inputs: the parts, the names and the marks.
/// Output: true when one part matches.
fn marked(words: &[String], names: &[&str], marks: &[&str]) -> bool {
    words.iter().any(|word| {
        let named = names.iter().any(|name| {
            word == name
                || ["-", "_", "."]
                    .iter()
                    .any(|cut| word.starts_with(&format!("{name}{cut}")))
        });
        named || marks.iter().any(|mark| word.contains(mark))
    })
}

#[cfg(unix)]
pub(crate) fn socket_fit(real: &Path) -> Result<(), &'static str> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let meta = std::fs::symlink_metadata(real).ok();
    let socket = meta
        .as_ref()
        .map(|meta| (meta.file_type().is_socket(), meta.uid()));
    let parent = real
        .parent()
        .and_then(|folder| std::fs::symlink_metadata(folder).ok())
        .map(|meta| (meta.uid(), meta.mode()))
        .ok_or("grant_socket_missing")?;
    ownership(socket, parent, nix::unistd::getuid().as_raw())?;
    let links = meta.map_or(0, |meta| meta.nlink());
    if links != 1 {
        return Err("grant_socket_owner");
    }
    if kernel_processes::service::changeable_above_socket(real).is_some() {
        return Err("grant_socket_owner");
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn socket_fit(_real: &Path) -> Result<(), &'static str> {
    Err("grant_socket_needs_macos")
}

/// Judges who holds a socket: it must exist as a socket owned by the user, in a folder the
/// user owns that no group or other user can write, or in a sticky folder of the user or
/// root; `socket_fit` also reads the folders above and access lists. Inputs: the socket's
/// kind and owner when it exists, its folder's owner and mode, and the user. Output: done
/// or the refusal code.
#[cfg(unix)]
fn ownership(socket: Option<(bool, u32)>, parent: (u32, u32), me: u32) -> Result<(), &'static str> {
    let Some((true, owner)) = socket else {
        return Err("grant_socket_missing");
    };
    if owner != me {
        return Err("grant_socket_owner");
    }
    let (folder_owner, mode) = parent;
    let closed = folder_owner == me && mode & 0o022 == 0;
    let sticky = mode & 0o1000 != 0 && (folder_owner == me || folder_owner == 0);
    if !closed && !sticky {
        return Err("grant_socket_owner");
    }
    Ok(())
}

/// An agent's reason as the owner's question shows it: control characters become spaces,
/// runs of space become one, and it is cut to `REASON_BYTES_MAX` bytes on a character
/// boundary. Input: the agent's text. Output: the plain reason, or `grant_reason_invalid`
/// when nothing is left.
pub fn plain_reason(reason: &str) -> Result<String, &'static str> {
    let spaced: String = reason
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut plain = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    if plain.len() > REASON_BYTES_MAX {
        let mut end = REASON_BYTES_MAX;
        while !plain.is_char_boundary(end) {
            end -= 1;
        }
        plain.truncate(end);
    }
    if plain.is_empty() {
        return Err("grant_reason_invalid");
    }
    assert!(plain.len() <= REASON_BYTES_MAX);
    Ok(plain)
}

fn socket_path(grant: &AccessGrant, path: &Path) -> Result<PathBuf, &'static str> {
    if !grant.plain() {
        return Err("grant_socket_not_plain");
    }
    let name = path.file_name().ok_or("grant_socket_not_plain")?;
    let folder = path.parent().ok_or("grant_socket_not_plain")?;
    let real = match canonical(path) {
        Ok(real) => real,
        Err(_) => canonical(folder)
            .ok()
            .filter(|folder| folder.is_dir())
            .ok_or("grant_socket_not_plain")?
            .join(name),
    };
    let kept = AccessGrant::Socket { path: real.clone() };
    if !kept.plain() {
        return Err("grant_socket_not_plain");
    }
    Ok(real)
}

/// Checks a whole list of grants as [`checked`] does and as one policy may hold them.
/// Inputs: the engine's grant hooks, the grants and the engine's data folder. Output: the
/// grants to keep, in order, or the first refusal code.
pub fn checked_all(
    hooks: &GrantHooks,
    grants: &[AccessGrant],
    data_root: &Path,
) -> Result<Vec<AccessGrant>, &'static str> {
    let kept = grants
        .iter()
        .map(|grant| checked(hooks, grant, data_root))
        .collect::<Result<Vec<_>, _>>()?;
    let policy = AgentPolicy {
        grants: kept.clone(),
        ..AgentPolicy::default()
    };
    policy.validate()?;
    assert_eq!(kept.len(), grants.len());
    Ok(kept)
}

/// Reads the request id from an access question's asker name. Input: the stored asker.
/// Output: the id, or none for any other asker.
pub fn request_id(asker: &str) -> Option<i64> {
    asker
        .strip_prefix(ASKER_PREFIX)
        .and_then(|id| id.parse::<i64>().ok())
        .filter(|id| *id > 0)
}

/// Tells whether the owner's answer to an access question allows it: the first option
/// picked alone, whatever comment comes with it. Input: the picks. Output: the answer.
pub fn allows(picks: &[u8]) -> bool {
    picks == [0]
}

/// What one grant lets agents do, in the owner's words. Input: the grant. Output: the
/// sentence.
pub fn meaning(grant: &AccessGrant) -> &'static str {
    match grant {
        AccessGrant::Socket { .. } => {
            "Agents may connect to this socket and talk to the program behind it with your rights, but never replace or remove it. If that program can run commands or change files (a database, a debugger, an editor, a terminal or a remote shell), agents can then do anything you can, outside their sandbox."
        }
        AccessGrant::Port { .. } => {
            "Agents may connect to this port on this computer, even when their network is limited, and talk to the program behind it with your rights. If that program can run commands or change files (a database, a debugger, an editor, a terminal or a remote shell), agents can then do anything you can, outside their sandbox."
        }
        AccessGrant::TempFolder => {
            "Agents use your own temp folder as theirs: they can read and change other apps' temp files there, but no socket in it."
        }
    }
}

/// What one grant lets agents do as its access question says it: [`meaning`], and for a
/// port the line that says what this Linux cannot hold for it, when there is one
/// ([`crate::policy::linux_network_gap`]). Inputs: the grant and that line, if any.
/// Output: the sentences.
pub fn asked_meaning(grant: &AccessGrant, gap: Option<&str>) -> String {
    match (grant, gap) {
        (AccessGrant::Port { .. }, Some(gap)) => format!("{} {gap}", meaning(grant)),
        _ => meaning(grant).to_owned(),
    }
}

pub fn capital(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The identity of the socket file a checked socket grant names: its device and inode,
/// read without following a link, so the file the owner was asked about can be told from
/// one put at its path later. Input: the canonical path. Output: `device:inode`, or none
/// when it cannot be read.
#[cfg(unix)]
pub fn socket_identity(real: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(real).ok()?;
    Some(format!("{}:{}", meta.dev(), meta.ino()))
}

/// The identity of a socket file where no socket grant is kept. Input: the path, unused.
/// Output: none.
#[cfg(not(unix))]
pub fn socket_identity(_real: &Path) -> Option<String> {
    None
}

#[cfg(all(test, unix))]
mod tests;
