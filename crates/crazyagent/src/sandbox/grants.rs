use super::policy::{AccessGrant, AgentAccess, AgentPolicy, GRANT_PORTS_MAX, GRANT_SOCKETS_MAX};
use super::{OWNER_PORTS, canonical, owner_home, owner_secrets};
use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::protocol::ProjectId;
use crate::questions::{Ask, Asker, EngineSource, QuestionKind};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The most log rows one project keeps; older ones are pruned on each write.
pub const LOG_KEPT_MAX: i64 = 256;
/// The most log rows a project's Agent access view lists, newest first.
pub const LOG_LISTED_MAX: usize = 20;
/// The most access requests one project has waiting for the owner at once.
pub const REQUESTS_PENDING_MAX: i64 = 4;
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

const LOG_TABLE: &str = "CREATE TABLE IF NOT EXISTS access_grant_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    action TEXT NOT NULL CHECK(action IN ('granted','revoked','allowed','denied')),
    grant_json TEXT NOT NULL,
    by TEXT NOT NULL
)";

const REQUESTS_TABLE: &str = "CREATE TABLE IF NOT EXISTS access_requests (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project TEXT NOT NULL,
    asker TEXT NOT NULL,
    grant_json TEXT NOT NULL,
    reason TEXT NOT NULL,
    question INTEGER,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','allowed','denied')),
    created_ms INTEGER NOT NULL
)";

const GRANT_JSON_BYTES_MAX: usize = 256;
const LAUNCHD_LISTENERS: &str = "/private/tmp/com.apple.launchd.";
#[cfg(unix)]
const PER_USER_TEMP: &str = "/private/var/folders/";
#[cfg(unix)]
const IDE_FOLDERS_SCANNED_MAX: usize = 256;
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

const _: () = assert!(super::policy::GRANT_SOCKET_BYTES_MAX + 64 <= GRANT_JSON_BYTES_MAX);
const _: () = assert!(GRANT_SOCKETS_MAX + GRANT_PORTS_MAX + 1 == super::policy::GRANTS_MAX);
const _: () = assert!(REASON_BYTES_MAX < crate::questions::CONTEXT_BYTES_MAX / 2);

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

/// How an access request ended for the agent that made it: asked as an owner question,
/// or already allowed by the saved policy, so nothing was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Requested {
    Asked(u64),
    AlreadyAllowed,
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

/// Checks one grant the owner or an agent named, as a save, an Allow and each launch take
/// it: a socket as the canonical path of an existing socket with one name the user owns in
/// a folder no other user can change, never with the owner's credentials, in the engine's
/// data folder or a launchd listener, nor one of a container or VM host, a terminal or
/// editor, or a key agent; a port below the dynamic range other than the owner's fixed
/// ports, Docker's API and debugger ports, the model proxy ports and every IDE port the
/// engine recorded; the temp folder only when it is the owner's private one. Inputs: the
/// grant and the engine's data folder. Output: the grant to keep, or a plain refusal code.
pub fn checked(grant: &AccessGrant, data_root: &Path) -> Result<AccessGrant, &'static str> {
    match grant {
        AccessGrant::Socket { path } => {
            if !cfg!(target_os = "macos") {
                return Err("grant_socket_needs_macos");
            }
            let real = socket_path(grant, path)?;
            let owner = owner_home().map_err(|_| "grant_socket_secret")?;
            if owner_secrets(&owner).any(|secret| real.starts_with(&secret)) {
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
                || super::model_proxy::PROXY_PORTS.contains(port)
                || super::model_proxy::RELAY_PORTS.contains(port)
                || ide_ports(data_root).contains(port);
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
/// Inputs: the saved grants, the ports the launch keeps free and the engine's data folder.
/// Output: the grants to build in, in order.
pub fn held_now(grants: &[AccessGrant], kept: &[u16], data_root: &Path) -> Vec<AccessGrant> {
    assert!(grants.len() <= super::policy::GRANTS_MAX);
    let held: Vec<AccessGrant> = grants
        .iter()
        .filter(|grant| checked(grant, data_root).is_ok_and(|now| now == **grant))
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
fn socket_fit(real: &Path) -> Result<(), &'static str> {
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
    Ok(())
}

#[cfg(not(unix))]
fn socket_fit(_real: &Path) -> Result<(), &'static str> {
    Err("grant_socket_needs_macos")
}

/// Judges who holds a socket: it must exist as a socket owned by the user, in a folder the
/// user owns that no group or other user can write, or in a sticky folder of the user or
/// root, where no other user can replace it. Inputs: the socket's kind and owner when it
/// exists, its folder's owner and mode, and the user. Output: done or the refusal code.
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

/// The loopback ports the engine recorded for its projects' IDE servers and their proxies,
/// read from at most `IDE_FOLDERS_SCANNED_MAX` project folders. Input: the engine's data
/// folder. Output: the ports, none when there is no IDE folder.
#[cfg(unix)]
fn ide_ports(data_root: &Path) -> Vec<u16> {
    let Ok(folders) = std::fs::read_dir(data_root.join(crate::ide::FOLDER)) else {
        return Vec::new();
    };
    let ports: Vec<u16> = folders
        .take(IDE_FOLDERS_SCANNED_MAX)
        .filter_map(Result::ok)
        .filter_map(|entry| crate::ide::ports::recorded(&entry.path()))
        .flat_map(|ports| [ports.server, ports.proxy])
        .collect();
    assert!(ports.len() <= 2 * IDE_FOLDERS_SCANNED_MAX);
    ports
}

#[cfg(not(unix))]
fn ide_ports(_data_root: &Path) -> Vec<u16> {
    Vec::new()
}

/// An agent's reason as the owner's question shows it: control characters become spaces,
/// runs of space become one, and it is cut to `REASON_BYTES_MAX` bytes on a character
/// boundary. Input: the agent's text. Output: the plain reason, or `grant_reason_invalid`
/// when nothing is left.
fn plain_reason(reason: &str) -> Result<String, &'static str> {
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
/// Inputs: the grants and the engine's data folder. Output: the grants to keep, in order,
/// or the first refusal code.
pub fn checked_all(
    grants: &[AccessGrant],
    data_root: &Path,
) -> Result<Vec<AccessGrant>, &'static str> {
    let kept = grants
        .iter()
        .map(|grant| checked(grant, data_root))
        .collect::<Result<Vec<_>, _>>()?;
    let policy = AgentPolicy {
        grants: kept.clone(),
        ..AgentPolicy::default()
    };
    policy.validate()?;
    assert_eq!(kept.len(), grants.len());
    Ok(kept)
}

/// The engine's data folder for a connection on `engine.db`: the folder holding it.
/// Input: the connection. Output: the folder, or none for an in-memory database.
pub fn data_root(connection: &Connection) -> Option<PathBuf> {
    connection
        .path()
        .filter(|path| !path.is_empty())
        .map(Path::new)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

/// Logs one grant change for a project and prunes the log to [`LOG_KEPT_MAX`] rows.
/// Inputs: the connection or transaction, the project, the action (`granted`, `revoked`,
/// `allowed` or `denied`), the grant, who made it and the time in ms. Output: done, or the
/// database error.
pub fn log(
    connection: &Connection,
    project: &str,
    action: &str,
    grant: &AccessGrant,
    by: &str,
    now: i64,
) -> rusqlite::Result<()> {
    assert!(matches!(
        action,
        "granted" | "revoked" | "allowed" | "denied"
    ));
    let body = serde_json::to_string(grant)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    assert!(body.len() <= GRANT_JSON_BYTES_MAX);
    connection.execute(LOG_TABLE, [])?;
    connection.execute(
        "INSERT INTO access_grant_log(project,at_ms,action,grant_json,by) VALUES (?1,?2,?3,?4,?5)",
        params![project, now, action, body, by],
    )?;
    connection.execute(
        "DELETE FROM access_grant_log WHERE project=?1 AND id NOT IN (
             SELECT id FROM access_grant_log WHERE project=?1 ORDER BY id DESC LIMIT ?2)",
        params![project, LOG_KEPT_MAX],
    )?;
    Ok(())
}

/// Logs what an owner save changed in a project's grants: each grant only in the new list
/// as granted and each only in the old one as revoked. Inputs: the connection, the project,
/// the grants before and after, who saved and the time. Output: done, or the database error.
pub fn log_changes(
    connection: &Connection,
    project: &str,
    before: &[AccessGrant],
    after: &[AccessGrant],
    by: &str,
    now: i64,
) -> rusqlite::Result<()> {
    for grant in after.iter().filter(|grant| !before.contains(grant)) {
        log(connection, project, "granted", grant, by, now)?;
    }
    for grant in before.iter().filter(|grant| !after.contains(grant)) {
        log(connection, project, "revoked", grant, by, now)?;
    }
    Ok(())
}

/// Reads a project's newest grant log rows, at most [`LOG_LISTED_MAX`]; none when nothing
/// was ever logged. Inputs: the connection and the project. Output: the rows, newest first.
pub fn history(connection: &Connection, project: &str) -> rusqlite::Result<Vec<LogView>> {
    if !table_exists(connection, "access_grant_log")? {
        return Ok(Vec::new());
    }
    let mut statement = connection.prepare(
        "SELECT at_ms,action,grant_json,by FROM access_grant_log WHERE project=?1 ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = statement
        .query_map(params![project, LOG_LISTED_MAX as i64], |row| {
            let body: String = row.get(2)?;
            let grant = serde_json::from_str::<AccessGrant>(&body)
                .map(|grant| grant.label())
                .unwrap_or_else(|_| "An extra this version does not know".to_owned());
            Ok(LogView {
                at_ms: row.get(0)?,
                action: row.get(1)?,
                grant,
                by: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert!(rows.len() <= LOG_LISTED_MAX);
    Ok(rows)
}

/// Removes a project's grant log and requests when the project is removed. Inputs: the
/// transaction and the project. Output: done, or the database error.
pub fn forget_project(connection: &Connection, project: &str) -> rusqlite::Result<()> {
    connection.execute(LOG_TABLE, [])?;
    connection.execute(REQUESTS_TABLE, [])?;
    connection.execute("DELETE FROM access_grant_log WHERE project=?1", [project])?;
    connection.execute("DELETE FROM access_requests WHERE project=?1", [project])?;
    Ok(())
}

/// Turns an agent's access request into a real owner question (F08): the grant is checked
/// as a save checks it, one the policy already holds asks nothing, and otherwise the engine
/// raises a question with the options [`ALLOW`] and [`DONT_ALLOW`], the words written by the
/// engine from the checked grant and the agent's reason only as quoted context. Inputs: the
/// engine connection, the project, who asked, the grant and the reason. Output: the
/// outcome, or a plain refusal code.
pub fn request(
    db: &Connection,
    project: &ProjectId,
    asker: &Asker,
    grant: &AccessGrant,
    reason: &str,
) -> Result<Requested, String> {
    let reason = plain_reason(reason)?;
    let data = data_root(db).ok_or("grant_requests_unavailable")?;
    let grant = checked(grant, &data)?;
    let policy =
        super::policy::load(db, project.as_str()).map_err(|_| "grant_requests_unavailable")?;
    let full_reach = policy.access == AgentAccess::Full && grant != AccessGrant::TempFolder;
    if full_reach || policy.grants.contains(&grant) {
        return Ok(Requested::AlreadyAllowed);
    }
    let mut grown = policy.clone();
    grown.grants.push(grant.clone());
    if grown.validate().is_err() {
        return Err("grant_full".into());
    }
    let body = serde_json::to_string(&grant).map_err(|_| "grant_requests_unavailable")?;
    db.execute(REQUESTS_TABLE, [])
        .map_err(|_| "grant_requests_unavailable")?;
    let waiting: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM access_requests WHERE project=?1 AND status='pending'",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(|_| "grant_requests_unavailable")?;
    if waiting >= REQUESTS_PENDING_MAX {
        return Err("grant_requests_full".into());
    }
    db.execute(
        "INSERT INTO access_requests(project,asker,grant_json,reason,created_ms) VALUES (?1,?2,?3,?4,?5)",
        params![project.as_str(), asker.name(), body, &reason, crate::team::now_ms()],
    )
    .map_err(|_| "grant_requests_unavailable")?;
    let id = db.last_insert_rowid();
    assert!(id > 0);
    let ask = Ask {
        question: format!(
            "{} asks for one extra: {}. Allow it?",
            capital(&crate::questions::asker_label(&asker.name())),
            grant.label()
        ),
        kind: QuestionKind::Single,
        options: vec![ALLOW.to_owned(), DONT_ALLOW.to_owned()],
        recommended: Some(DONT_ALLOW.to_owned()),
        context: Some(format!(
            "{} Its reason, in its own words: {}. Allow only if you trust it. Allowing adds it to this project's Agent access, where you can remove it; running agents restart at their next pause to use it.",
            meaning(&grant),
            serde_json::to_string(&reason).map_err(|_| "grant_requests_unavailable")?
        )),
    };
    let source = EngineSource::Access(id.to_string());
    match crate::questions::raise(db, project, &source, &ask) {
        Ok(question) => {
            db.execute(
                "UPDATE access_requests SET question=?2 WHERE id=?1",
                params![id, i64::try_from(question).unwrap_or(i64::MAX)],
            )
            .map_err(|_| "grant_requests_unavailable")?;
            Ok(Requested::Asked(question))
        }
        Err(problem) => {
            let _ = db.execute("DELETE FROM access_requests WHERE id=?1", [id]);
            Err(problem.0)
        }
    }
}

/// Settles the access request behind an engine question the owner answered or dismissed,
/// in the caller's transaction: an Allow adds the grant to the project's policy with its own
/// versioned owner write when it still passes its check and fits, and every outcome is
/// logged. A question of another asker is left alone. Inputs: the transaction, the
/// project, the question's asker, whether the owner allowed it and the time. Output: done,
/// or the store error.
pub(crate) fn decide(
    tx: &Transaction<'_>,
    project: &ProjectId,
    asker: &str,
    allowed: bool,
    now: i64,
) -> Result<(), EngineError> {
    let Some(id) = request_id(asker) else {
        return Ok(());
    };
    let unavailable = |_| failure("grant_requests_unavailable");
    tx.execute(REQUESTS_TABLE, []).map_err(unavailable)?;
    let row: Option<(String, Option<i64>)> = tx
        .query_row(
            "SELECT grant_json,question FROM access_requests WHERE id=?1 AND project=?2 AND status='pending'",
            params![id, project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((body, question)) = row else {
        return Ok(());
    };
    let by = question.map_or_else(|| "owner".to_owned(), |q| format!("question {q}"));
    let grant: AccessGrant =
        serde_json::from_str(&body).map_err(|_| failure("grant_requests_unavailable"))?;
    let data = data_root(tx).ok_or_else(|| failure("grant_requests_unavailable"))?;
    let kept = if allowed {
        checked(&grant, &data).ok()
    } else {
        None
    };
    let mut policy = super::policy::load(tx, project.as_str()).map_err(unavailable)?;
    let granted = match kept {
        Some(kept) if policy.grants.contains(&kept) => true,
        Some(kept) => {
            policy.grants.push(kept);
            if policy.validate().is_ok() {
                let item = crate::storage::versioned::agent_policy_item(project);
                let current = crate::storage::versioned::current(tx, &item)?.version;
                if crate::storage::versioned::owner_write(tx, &item, current)?.is_err() {
                    return Err(failure(crate::storage::versioned::STALE_CODE));
                }
                super::policy::save(tx, project.as_str(), &policy).map_err(unavailable)?;
                true
            } else {
                false
            }
        }
        None => false,
    };
    let (status, action) = if granted {
        ("allowed", "allowed")
    } else {
        ("denied", "denied")
    };
    tx.execute(
        "UPDATE access_requests SET status=?2 WHERE id=?1",
        params![id, status],
    )
    .map_err(unavailable)?;
    log(tx, project.as_str(), action, &grant, &by, now).map_err(unavailable)?;
    Ok(())
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

fn capital(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn table_exists(connection: &Connection, name: &str) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [name],
        |row| row.get(0),
    )
}

#[cfg(all(test, unix))]
mod tests;
