use super::{ProjectSandbox, SandboxNetwork};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// A tools-on agent confined by the real project Seatbelt profile, for tests where an agent
/// attacks what the unsandboxed engine reads. One private /tmp root holds the project (read
/// only to the agent), the agent's writable home and tmp, a peer agent's home, and an engine
/// runtime whose `github.json` the profile denies.
pub(crate) struct TestAgent {
    _temp: tempfile::TempDir,
    pub(crate) project: PathBuf,
    pub(crate) home: PathBuf,
    pub(crate) peer: PathBuf,
    sandbox: ProjectSandbox,
    profile: PathBuf,
}

impl TestAgent {
    /// Makes the folders 0700, the engine secret file and the installed profile.
    pub(crate) fn new() -> Self {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let root = temp.path().canonicalize().unwrap();
        let project = root.join("project");
        let profiles = root.join("profiles");
        let runtime = root.join("runtime");
        let home = root.join("agents").join("agent-1").join("home");
        let tmp = root.join("agents").join("agent-1").join("tmp");
        let peer = root.join("agents").join("agent-2").join("home");
        for folder in [&project, &profiles, &runtime, &home, &tmp, &peer] {
            fs::create_dir_all(folder).unwrap();
            fs::set_permissions(folder, fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(
            runtime.join("github.json"),
            "{\"engine_secret\":\"FIXTURE-ENGINE-SECRET\"}",
        )
        .unwrap();
        let sandbox = ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Only(Vec::new()))
            .unwrap()
            .guard_engine(&runtime)
            .unwrap();
        let profile = profiles.join("agent-1.sb");
        sandbox.install(&profile).unwrap();
        Self {
            _temp: temp,
            project,
            home,
            peer,
            sandbox,
            profile,
        }
    }

    /// Runs `script` with `/bin/sh -c` in the sandbox, the way a tools-on agent's shell tool
    /// does. Input: the script. Output: the finished process with its captured output.
    pub(crate) fn run(&self, script: &str) -> std::process::Output {
        let mut command = self.sandbox.command(&self.profile, Path::new("/bin/sh"));
        command
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crate::spawn_gate::output(&mut command).expect("the sandboxed shell starts")
    }

    /// Runs `script` in the sandbox and fails the test unless it succeeds.
    pub(crate) fn shell(&self, script: &str) {
        let output = self.run(script);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
