use crate::engine::EngineError;
use crate::sandbox::toolchains::{ToolHomes, Toolchains};
use crate::sandbox::{ProjectSandbox, SANDBOX_EXEC, SandboxNetwork, seatbelt_pattern};
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The file in a project's IDE folder that holds the Seatbelt profile of its running server,
/// written by the engine before each launch.
pub const PROFILE: &str = "server.sb";

const PROFILE_BYTES_MAX: usize = 96 * 1024;
const GRANTS_MAX: usize = 8;

/// The places one IDE server launch is given. `data` is the engine data root, denied except
/// for the grants. `root` is the project folder the IDE opens and edits, and `build` the
/// verified build, read and run only. `home` is the server's data folder and `tmp` its short
/// temp folder, both writable; unix sockets are bound only in `tmp`. `writable` are more
/// folders in the project's IDE folder, `read_only` folders there it may read but never
/// write, and `readable` the files there it may read. `locked` are folders inside `home` or
/// a writable folder that the engine writes: the server reads them but never writes, adds,
/// removes or moves anything in them, nor moves a folder between them and their grant.
pub struct Grants<'a> {
    pub data: &'a Path,
    pub root: &'a Path,
    pub build: &'a Path,
    pub home: &'a Path,
    pub tmp: &'a Path,
    pub writable: &'a [&'a Path],
    pub read_only: &'a [&'a Path],
    pub readable: &'a [&'a Path],
    pub locked: &'a [&'a Path],
}

/// Says whether the IDE server can run sandboxed here: Seatbelt on macOS only. Linux and
/// Windows qualify in IDE-12a and IDE-12b; until then no IDE server starts there.
pub fn qualified() -> bool {
    cfg!(target_os = "macos") && Path::new(SANDBOX_EXEC).is_file()
}

/// Builds the Seatbelt profile an IDE server process tree runs under. The base is the agent
/// profile for the project folder: deny by default, system reads, the project writable except
/// its git hooks, git config and context, the owner's secrets and keychain denied, the
/// project's toolchains readable, `home` and `tmp` writable and the build readable. On top,
/// the whole engine data root is denied, then only the grants are given back, then every key
/// file directly in a folder of the data root (the owner keys) and the engine database are
/// denied again, then every locked folder is denied for writing, case-insensitively as APFS
/// matches names, with its folders up to its grant kept from being moved, then agent IDE
/// config and agent git folders in the project are denied for reading ([`agent_config`]).
/// The project's own git index and its lock stay denied: the IDE stages on a private index
/// the engine carries across ([`super::git::shim_text`], SECAUDIT-IDE-SERVER-INDEX). Network:
/// listen on 127.0.0.1 at `port`, unix sockets in `tmp`, connect to an owner socket, and
/// reach what `network` allows ([`crate::sandbox::policy::ide_network`]).
/// On the open network it never binds or listens on `kept_ports`, the owner's local model
/// ports. Inputs: the grants, the server port, the network and the ports kept. Output: the
/// profile text, or `ide_sandbox_invalid` for an unsafe place.
pub fn profile(
    grants: &Grants,
    port: u16,
    network: SandboxNetwork,
    kept_ports: &[u16],
) -> Result<String, EngineError> {
    let invalid = |_| failure("ide_sandbox_invalid");
    let data = grants.data;
    let writes: Vec<&Path> = std::iter::once(grants.home)
        .chain(grants.writable.iter().copied())
        .collect();
    if port == 0
        || !plain(data)
        || crate::sandbox::canonical(data).ok().as_deref() != Some(data)
        || grants.root.starts_with(data)
        || data.starts_with(grants.root)
        || grants.tmp.starts_with(data)
        || !grants.home.starts_with(data)
        || grants.writable.len() > GRANTS_MAX
        || grants.read_only.len() > GRANTS_MAX
        || grants.readable.len() > GRANTS_MAX
        || grants.locked.len() > GRANTS_MAX
        || grants
            .writable
            .iter()
            .chain(grants.read_only.iter())
            .chain(grants.readable.iter())
            .chain(grants.locked.iter())
            .any(|path| !plain(path) || !path.starts_with(data) || *path == data)
        || grants.read_only.iter().any(|folder| {
            writes
                .iter()
                .any(|write| folder.starts_with(write) || write.starts_with(folder))
        })
    {
        return Err(failure("ide_sandbox_invalid"));
    }
    let held = grants
        .locked
        .iter()
        .map(|folder| grant(&writes, folder).map(|within| (*folder, within)))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| failure("ide_sandbox_invalid"))?;
    let homes = ToolHomes::owner().map_err(invalid)?;
    let closed = matches!(&network, SandboxNetwork::Only(remotes) if remotes.is_empty());
    let sandbox = ProjectSandbox::new(grants.root, grants.home, grants.tmp, network)
        .and_then(|sandbox| sandbox.keep_ports(kept_ports))
        .and_then(|sandbox| sandbox.allow_read(grants.build))
        .and_then(|sandbox| sandbox.allow_tools(&Toolchains::detect(grants.root, &homes)))
        .map_err(invalid)?;
    let mut text = sandbox.profile();
    let base = text.len();
    text.push_str(&format!(
        "(deny file-read* file-write*\n  (subpath \"{}\"))\n(allow file-read* file-write*",
        data.display()
    ));
    for folder in std::iter::once(&grants.home).chain(grants.writable.iter()) {
        text.push_str(&format!("\n  (subpath \"{}\")", folder.display()));
    }
    text.push(')');
    if !grants.readable.is_empty() || !grants.read_only.is_empty() {
        text.push_str("\n(allow file-read*");
        for file in grants.readable {
            text.push_str(&format!("\n  (literal \"{}\")", file.display()));
        }
        for folder in grants.read_only {
            text.push_str(&format!("\n  (subpath \"{}\")", folder.display()));
        }
        text.push(')');
    }
    text.push_str(&format!(
        "\n(allow file-read* file-map-executable\n  (subpath \"{}\"))\n(allow file-read-metadata",
        grants.build.display()
    ));
    for folder in [&grants.home, &grants.build]
        .into_iter()
        .chain(grants.writable.iter())
        .chain(grants.read_only.iter())
    {
        text.push_str(&format!("\n  (path-ancestors \"{}\")", folder.display()));
    }
    let pattern = seatbelt_pattern(data);
    let tmp = grants.tmp.display();
    text.push_str(&format!(
        ")\n(deny file-read* file-write*\n  (regex #\"^{pattern}/[^/]+/[^/]*\\.cap$\")\n  (regex #\"^{pattern}/engine\\.db(-wal|-shm|-journal)?$\"))\n"
    ));
    text.push_str(&locks(&held));
    text.push_str(&agent_config(grants.root));
    text.push_str(&format!(
        "(allow network-bind network-inbound\n  (local ip \"localhost:{port}\"))\n(allow network-bind\n  (local unix-socket (subpath \"{tmp}\")))\n(allow network-outbound\n  (remote unix-socket (subpath \"{tmp}\"))\n  (remote unix-socket (regex #\"^{pattern}/[^/]+/owner\\.sock$\")))\n(allow mach-lookup\n  (global-name \"com.apple.FSEvents\"))\n"
    ));
    assert!(text.starts_with("(version 1)\n(deny default)\n"));
    assert!(text[base..].starts_with(&format!(
        "(deny file-read* file-write*\n  (subpath \"{}\"))",
        data.display()
    )));
    assert!(text.ends_with("(global-name \"com.apple.FSEvents\"))\n"));
    assert!(!closed || !text.contains("(remote ip"));
    if text.len() > PROFILE_BYTES_MAX {
        return Err(failure("ide_sandbox_invalid"));
    }
    Ok(text)
}

/// Writes an IDE server's profile to `<folder>/server.sb` with mode 0600, never through a
/// link, replacing the last launch's. The folder is the engine's private IDE folder of the
/// project, which no grant lets the server write. Inputs: the folder and the grants with the
/// port, the network and the ports kept. Output: the profile path, or `ide_sandbox_invalid`
/// or `ide_state_unavailable`.
pub fn install(
    folder: &Path,
    grants: &Grants,
    port: u16,
    network: SandboxNetwork,
    kept_ports: &[u16],
) -> Result<PathBuf, EngineError> {
    let text = profile(grants, port, network, kept_ports)?;
    let path = folder.join(PROFILE);
    if grants
        .writable
        .iter()
        .chain([&grants.home, &grants.tmp, &grants.root])
        .any(|root| path.starts_with(root))
    {
        return Err(failure("ide_sandbox_invalid"));
    }
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())
                .and_then(|()| file.sync_all())
        })
        .map_err(|_| failure("ide_state_unavailable"))?;
    Ok(path)
}

/// A command that runs `program` under an installed IDE profile through `sandbox-exec`; the
/// caller adds the arguments, environment and process group. Inputs: the profile and the
/// absolute program. Output: the command.
pub fn command(profile: &Path, program: &Path) -> Command {
    assert!(profile.is_absolute() && program.is_absolute());
    let mut command = Command::new(SANDBOX_EXEC);
    command.arg("-f").arg(profile).arg(program);
    command
}

/// The writable grant a locked folder lies strictly inside. Inputs: `home` with the writable
/// folders, and the locked folder. Output: the first such grant, or none.
fn grant<'a>(writes: &[&'a Path], folder: &Path) -> Option<&'a Path> {
    writes
        .iter()
        .copied()
        .find(|write| folder.starts_with(write) && folder != *write)
}

/// The rules that keep each locked folder the engine's: no write of any kind to it or in
/// it, matched in any letter case, and no move or removal of a folder from its parent up to
/// and including its grant, so it cannot be set aside and planted again. Input: each locked
/// folder with the grant it lies inside. Output: the rule text, empty without locks.
fn locks(held: &[(&Path, &Path)]) -> String {
    if held.is_empty() {
        return String::new();
    }
    let mut writes = String::from("(deny file-write*");
    let mut moves = String::from("(deny file-write-unlink");
    for (folder, within) in held {
        writes.push_str(&format!("\n  (regex #\"^{}(/.*)?$\")", caseless(folder)));
        for above in folder.ancestors().skip(1) {
            moves.push_str(&format!("\n  (regex #\"^{}$\")", caseless(above)));
            if above == *within {
                break;
            }
        }
    }
    format!("{writes})\n{moves})\n")
}

/// The rule that keeps agents from setting the owner's IDE (DEC-098 addendum 9, INT-1): the
/// server reads no `.vscode` folder anywhere in an agent worktree, no `.code-workspace` file
/// in one, and nothing at or under the `.git` at an agent folder's top, under either agents
/// root a project uses (`.crazyagent/worktrees` or `worktrees`), in any letter case. With
/// the layout's agents root, and the older one while engine-made worktrees stay there, as
/// git's search ceiling ([`super::git::ceiling`]), git in an agent folder then finds no
/// repository, whether its `.git` is an agent's own clone, a linked
/// worktree's `gitdir:` file or a planted one, neither runs a program its config or hooks
/// name nor walks up to the project's repository. Seatbelt checks a followed link's own path as well as where it leads, so a
/// link named so is refused too. The engine's workspace file and Remote settings stay the
/// only settings source for agent folders. Input: the project folder. Output: the rule text.
fn agent_config(root: &Path) -> String {
    let agents = format!(
        "^{}({})?/{}/[^/]+/",
        caseless(root),
        anycase("/\\.crazyagent"),
        anycase("worktrees")
    );
    let folder = anycase("\\.vscode");
    let workspace = anycase("\\.code-workspace");
    let git = anycase("\\.git");
    format!(
        "(deny file-read*\n  (regex #\"{agents}(.*/)?{folder}(/.*)?$\")\n  (regex #\"{agents}(.*/)?[^/]*{workspace}$\")\n  (regex #\"{agents}{git}(/.*)?$\"))\n"
    )
}

/// A Seatbelt regex matching `path` in any letter case, as APFS matches names, so a case
/// variant of a locked name cannot be created or written. Input: a plain absolute path.
/// Output: the pattern text, without anchors.
fn caseless(path: &Path) -> String {
    anycase(&seatbelt_pattern(path))
}

/// Makes a Seatbelt pattern match in any letter case: each ASCII letter becomes a class of
/// both its cases. Input: the pattern. Output: the caseless pattern.
fn anycase(text: &str) -> String {
    let mut pattern = String::new();
    for character in text.chars() {
        if character.is_ascii_alphabetic() {
            pattern.push('[');
            pattern.push(character.to_ascii_lowercase());
            pattern.push(character.to_ascii_uppercase());
            pattern.push(']');
        } else {
            pattern.push(character);
        }
    }
    pattern
}

fn plain(path: &Path) -> bool {
    path.is_absolute()
        && path.to_str().is_some_and(|text| {
            !text
                .chars()
                .any(|c| c.is_control() || matches!(c, '"' | '\\'))
        })
}

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

#[cfg(all(test, target_os = "macos"))]
mod tests;
