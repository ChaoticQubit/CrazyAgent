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
/// folders in the project's IDE folder and `readable` the files there it may read.
pub struct Grants<'a> {
    pub data: &'a Path,
    pub root: &'a Path,
    pub build: &'a Path,
    pub home: &'a Path,
    pub tmp: &'a Path,
    pub writable: &'a [&'a Path],
    pub readable: &'a [&'a Path],
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
/// denied again. Network: listen on 127.0.0.1 at `port`, unix sockets in `tmp`, and connect
/// to an owner socket. Inputs: the grants and the server port. Output: the profile text, or
/// `ide_sandbox_invalid` for an unsafe place.
pub fn profile(grants: &Grants, port: u16) -> Result<String, EngineError> {
    let invalid = |_| failure("ide_sandbox_invalid");
    let data = grants.data;
    if port == 0
        || !plain(data)
        || crate::sandbox::canonical(data).ok().as_deref() != Some(data)
        || grants.root.starts_with(data)
        || data.starts_with(grants.root)
        || grants.tmp.starts_with(data)
        || !grants.home.starts_with(data)
        || grants.writable.len() > GRANTS_MAX
        || grants.readable.len() > GRANTS_MAX
        || grants
            .writable
            .iter()
            .chain(grants.readable.iter())
            .any(|path| !plain(path) || !path.starts_with(data) || *path == data)
    {
        return Err(failure("ide_sandbox_invalid"));
    }
    let homes = ToolHomes::owner().map_err(invalid)?;
    let sandbox = ProjectSandbox::new(
        grants.root,
        grants.home,
        grants.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
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
    text.push_str(")\n(allow file-read*");
    for file in grants.readable {
        text.push_str(&format!("\n  (literal \"{}\")", file.display()));
    }
    text.push_str(&format!(
        ")\n(allow file-read* file-map-executable\n  (subpath \"{}\"))\n(allow file-read-metadata",
        grants.build.display()
    ));
    for folder in [&grants.home, &grants.build]
        .into_iter()
        .chain(grants.writable.iter())
    {
        text.push_str(&format!("\n  (path-ancestors \"{}\")", folder.display()));
    }
    let pattern = seatbelt_pattern(data);
    let tmp = grants.tmp.display();
    text.push_str(&format!(
        ")\n(deny file-read* file-write*\n  (regex #\"^{pattern}/[^/]+/[^/]*\\.cap$\")\n  (regex #\"^{pattern}/engine\\.db(-wal|-shm|-journal)?$\"))\n(allow network-bind network-inbound\n  (local ip \"localhost:{port}\"))\n(allow network-bind\n  (local unix-socket (subpath \"{tmp}\")))\n(allow network-outbound\n  (remote unix-socket (subpath \"{tmp}\"))\n  (remote unix-socket (regex #\"^{pattern}/[^/]+/owner\\.sock$\")))\n(allow mach-lookup\n  (global-name \"com.apple.FSEvents\"))\n"
    ));
    assert!(text.starts_with("(version 1)\n(deny default)\n"));
    assert!(text[base..].starts_with(&format!(
        "(deny file-read* file-write*\n  (subpath \"{}\"))",
        data.display()
    )));
    assert!(text.ends_with("(global-name \"com.apple.FSEvents\"))\n"));
    assert!(!text.contains("(remote ip"));
    if text.len() > PROFILE_BYTES_MAX {
        return Err(failure("ide_sandbox_invalid"));
    }
    Ok(text)
}

/// Writes an IDE server's profile to `<folder>/server.sb` with mode 0600, never through a
/// link, replacing the last launch's. The folder is the engine's private IDE folder of the
/// project, which no grant lets the server write. Inputs: the folder and the grants with the
/// port. Output: the profile path, or `ide_sandbox_invalid` or `ide_state_unavailable`.
pub fn install(folder: &Path, grants: &Grants, port: u16) -> Result<PathBuf, EngineError> {
    let text = profile(grants, port)?;
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
