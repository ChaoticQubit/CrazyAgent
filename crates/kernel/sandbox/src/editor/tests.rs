use super::*;
use crate::policy::{AgentAccess, AgentNetwork, IdeAgentFiles, ide_network};
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;

const PORT: u16 = 45_731;

/// The folder of the engine data root that holds the IDE servers' data.
const FOLDER: &str = "ide";

/// The network of a server that reaches nothing, as a Sandboxed one has.
fn closed() -> SandboxNetwork {
    SandboxNetwork::Only(Vec::new())
}

fn folder(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

/// Runs `program` with `args` the way the IDE server starts: under the installed profile
/// through [`command`], from a cleared environment. Output: whether it exited 0, and its
/// standard output.
fn run(profile: &Path, program: &str, args: &[&std::ffi::OsStr]) -> (bool, String) {
    let mut command = super::command(profile, Path::new(program));
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = kernel_processes::spawn_gate::output(&mut command).unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

/// Runs one `/bin/sh` script under the profile with `path` as `$1`. Output: whether it
/// exited 0.
fn sh(profile: &Path, script: &str, path: &Path) -> bool {
    run(
        profile,
        "/bin/sh",
        &[
            std::ffi::OsStr::new("-c"),
            std::ffi::OsStr::new(script),
            std::ffi::OsStr::new("sh"),
            path.as_os_str(),
        ],
    )
    .0
}

mod profile_rows;
