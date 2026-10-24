#![allow(dead_code)]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::owner::{OwnerClientKind, OwnerResponse, OwnerResult};
use crazyagent::owner_profile::OwnerProfile;
use crazyagent::project_setup::{Platform, ProjectSetup, ResearchKind};
use crazyagent::projects_home::FoundProject;
use crazyagent::protocol::{ProjectId, ProjectSpec};
use crazyagent::setup::{DependencyState, SetupStatus};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub const ENGINE: &str = env!("CARGO_BIN_EXE_crazyagent");
pub const GIT: &str = "UNAVAILABLE: git is not on PATH; install git";
pub const TMUX: &str = "UNAVAILABLE: tmux is not installed; install tmux 3.x";
const READY_TIME: Duration = Duration::from_secs(30);
const STOP_TIME: Duration = Duration::from_secs(40);
const ATTACH_TIME: Duration = Duration::from_secs(10);
const FOUND_TIME: Duration = Duration::from_secs(20);
const STDERR_BYTES_MAX: u64 = 64 * 1024;
const PANE_EXIT_TIME: Duration = Duration::from_secs(5);
const PANES_MAX: usize = 16;

/// Reports whether a process with this id still exists.
/// Input: a process id. Output: true while `kill -0` succeeds for it.
pub fn alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Lists the process ids every pane of a test tmux server runs.
/// Input: the tmux binary and server socket. Output: at most 16 pane process ids;
/// none when the server is already gone.
pub fn pane_pids(tmux: &Path, socket: &Path) -> Vec<u32> {
    Command::new(tmux)
        .arg("-S")
        .arg(socket)
        .args(["list-panes", "-a", "-F", "#{pane_pid}"])
        .env_remove("TMUX")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| line.trim().parse().ok())
                .take(PANES_MAX)
                .collect()
        })
        .unwrap_or_default()
}

/// Test processes started from this test binary's `crazyagent` that must not outlive
/// the test. Dropping it gives each one five seconds to exit, then kills and waits for
/// any still running, so a failed test leaves no TUI behind.
/// Input: the process ids. Output: none.
pub struct Survivors(pub Vec<u32>);

impl Drop for Survivors {
    fn drop(&mut self) {
        assert!(self.0.len() <= PANES_MAX);
        let deadline = Instant::now() + PANE_EXIT_TIME;
        while self.0.iter().any(|pid| alive(*pid)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        for pid in self.0.iter().copied().filter(|pid| alive(*pid)) {
            let command = Command::new("ps")
                .args(["-o", "command=", "-p", &pid.to_string()])
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
                .unwrap_or_default();
            if !command.contains(ENGINE) {
                continue;
            }
            eprintln!("test process {pid} outlived its terminal; killing it: {command}");
            let _ = Command::new("/bin/kill")
                .args(["-KILL", &pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let killed = Instant::now() + PANE_EXIT_TIME;
            while alive(pid) && Instant::now() < killed {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Stops a test tmux server and every `crazyagent` process its panes ran.
/// Input: the tmux binary and server socket. Output: none; pane processes that
/// survive the hang-up are killed by `Survivors`.
pub fn stop_tmux(tmux: &Path, socket: &Path) {
    let _survivors = Survivors(pane_pids(tmux, socket));
    let _ = Command::new(tmux)
        .arg("-S")
        .arg(socket)
        .arg("kill-server")
        .env_remove("TMUX")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Makes a private scratch folder with a short path, so engine and tmux socket paths
/// stay under the Unix socket length limit.
/// Input: a name prefix. Output: the folder, removed when dropped.
pub fn scratch(prefix: &str) -> tempfile::TempDir {
    let base = if Path::new("/private/tmp").is_dir() {
        "/private/tmp"
    } else {
        "/tmp"
    };
    let folder = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(base)
        .unwrap();
    fs::set_permissions(folder.path(), fs::Permissions::from_mode(0o700)).unwrap();
    folder
}

/// The real `crazyagent engine` process on a private root, started as the desktop app
/// and the TUI start it. Inputs: a scratch prefix. Output: a running engine whose
/// stderr is kept for failure messages; dropping it kills the process.
pub struct Engine {
    pub root: tempfile::TempDir,
    child: Option<Child>,
    stderr: Option<std::thread::JoinHandle<String>>,
}

impl Engine {
    /// Prepares the private root and settings without starting the process.
    /// Input: a scratch prefix. Output: the engine, not yet running.
    pub fn new(prefix: &str) -> Self {
        let root = scratch(prefix);
        let global = root.path().join("global.toml");
        fs::write(&global, "version = 1\ntimeout_ms = 5000\n").unwrap();
        fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            root,
            child: None,
            stderr: None,
        }
    }

    /// Prepares and starts the engine, then waits for its `ready` line.
    /// Input: a scratch prefix. Output: the running engine.
    pub fn start(prefix: &str) -> Self {
        let mut engine = Self::new(prefix);
        engine.launch();
        engine
    }

    /// The engine root exactly as the process was given it.
    pub fn path(&self) -> &Path {
        self.root.path()
    }

    /// The socket the TUI connects to.
    pub fn socket(&self) -> PathBuf {
        self.root.path().join("engine.sock")
    }

    /// Starts the process on this root and waits up to 30 s for `ready`.
    /// Output: none; panics with the engine's stderr when it never becomes ready.
    pub fn launch(&mut self) {
        assert!(self.child.is_none());
        let root = self.root.path();
        let mut child = Command::new(ENGINE)
            .arg("engine")
            .arg(root)
            .arg(root.join("global.toml"))
            .arg(root.join("project.toml"))
            .arg("engine.sock")
            .arg("--writable-settings")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, ready) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let _ = send.send(line);
            let _ = std::io::copy(&mut reader, &mut std::io::sink());
        });
        let mut stderr = child.stderr.take().unwrap();
        self.stderr = Some(std::thread::spawn(move || {
            let mut kept = Vec::new();
            let _ = (&mut stderr).take(STDERR_BYTES_MAX).read_to_end(&mut kept);
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
            String::from_utf8_lossy(&kept).into_owned()
        }));
        self.child = Some(child);
        let line = ready.recv_timeout(READY_TIME).unwrap_or_default();
        if line != "ready\n" {
            let text = self.halt();
            panic!("the engine was not ready within 30 s: {line:?}\n{text}");
        }
    }

    /// Stops the engine with SIGTERM as the desktop app does and checks it exits cleanly.
    /// Output: its stderr text.
    pub fn stop(&mut self) -> String {
        let mut child = self.child.take().expect("the engine is running");
        let signalled = Command::new("/bin/kill")
            .args(["-s", "TERM", &child.id().to_string()])
            .status()
            .unwrap();
        assert!(signalled.success());
        let deadline = Instant::now() + STOP_TIME;
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "the engine did not stop within 40 s\n{}",
                    self.stderr_text()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let text = self.stderr_text();
        assert!(status.success(), "engine exit {status}\n{text}");
        text
    }

    /// Stops and starts the engine on the same root, as a second launch of the app does.
    pub fn restart(&mut self) {
        self.stop();
        self.launch();
    }

    fn halt(&mut self) -> String {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.stderr_text()
    }

    fn stderr_text(&mut self) -> String {
        self.stderr
            .take()
            .map(|handle| handle.join().unwrap_or_default())
            .unwrap_or_default()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The owner's connection to a running engine, as the TUI holds it, with the client
/// generation every change carries.
pub struct Owner {
    pub controller: OwnerController,
    generation: u64,
}

impl Owner {
    /// Attaches to the engine's owner socket as the TUI.
    /// Input: the running engine. Output: the attached owner.
    pub async fn attach(engine: &Engine) -> Self {
        let controller = OwnerController::attach(engine.path(), OwnerClientKind::Tui, ATTACH_TIME)
            .await
            .unwrap();
        Self {
            controller,
            generation: 0,
        }
    }

    /// The next client generation for one change.
    pub fn next(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    /// Saves the first-run profile. Input: the profile. Output: the engine's result.
    pub async fn save_profile(&mut self, profile: OwnerProfile) -> OwnerResult {
        let generation = self.next();
        result(
            self.controller
                .save_owner_profile(profile, generation)
                .await
                .unwrap(),
        )
    }

    /// Picks the projects home. Input: an existing folder. Output: the engine's result.
    pub async fn set_home(&mut self, home: &Path) -> OwnerResult {
        let generation = self.next();
        result(
            self.controller
                .set_projects_home(home.to_str().unwrap().into(), generation)
                .await
                .unwrap(),
        )
    }

    /// Sets up one project the way New project does, with no pool.
    /// Inputs: the project id and name. Output: the engine's result.
    pub async fn create_project(&mut self, id: &str, name: &str) -> OwnerResult {
        let generation = self.next();
        result(
            self.controller
                .setup_project(
                    ProjectId::new(id).unwrap(),
                    ProjectSpec {
                        name: name.into(),
                        repositories: vec![],
                    },
                    ProjectSetup {
                        about: "Watches sensor buoys and raises alerts.".into(),
                        tech_stack: vec!["rust".into()],
                        platforms: vec![Platform::Website],
                        research: vec![ResearchKind::MarketResearch],
                        pool: None,
                        team: None,
                        documents: vec![],
                    },
                    generation,
                )
                .await
                .unwrap(),
        )
    }

    /// Reads the found projects until the check holds, for up to 20 s.
    /// Inputs: what is awaited, for the failure message, and the check.
    /// Output: the list the check accepted.
    /// Reads setup until no tool is still being checked; the engine checks tool versions
    /// in the background, so an early read may say Checking. Output: the settled status.
    pub async fn setup_checked(&mut self) -> SetupStatus {
        let start = Instant::now();
        loop {
            let status = self.controller.setup_status().await.unwrap();
            let checking = status
                .dependencies
                .iter()
                .any(|dependency| dependency.state == DependencyState::Checking);
            if !checking {
                return status;
            }
            assert!(
                start.elapsed() < FOUND_TIME,
                "tools still checking: {status:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn found_until(
        &mut self,
        what: &str,
        want: impl Fn(&[FoundProject]) -> bool,
    ) -> Vec<FoundProject> {
        let start = Instant::now();
        loop {
            let found = self.controller.found_projects().await.unwrap();
            if want(&found) {
                return found;
            }
            assert!(start.elapsed() < FOUND_TIME, "{what}: {found:?}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

/// The recorded result of one owner change; anything else fails the test.
/// Input: the engine's response. Output: its result.
pub fn result(response: OwnerResponse) -> OwnerResult {
    match response {
        OwnerResponse::Applied { operation } => {
            assert!(!operation.historical_replay);
            operation.result
        }
        other => panic!("the owner change must be recorded: {other:?}"),
    }
}

/// A rejected result with its plain code.
pub fn rejected(code: &str) -> OwnerResult {
    OwnerResult::Rejected { code: code.into() }
}

/// One found project by its shown name.
pub fn named<'a>(found: &'a [FoundProject], name: &str) -> Option<&'a FoundProject> {
    found.iter().find(|entry| entry.name == name)
}

/// The owner profile every flow saves at first run.
pub fn profile() -> OwnerProfile {
    OwnerProfile {
        name: "Mira Okafor".into(),
        role: "Founder and full-stack developer".into(),
        bio: "Builds tools for marine researchers.".into(),
        preferred_stack: vec!["Rust".into(), "TypeScript".into()],
        working_style: Default::default(),
    }
}

/// Runs the real git with no system or global configuration, so the owner's own git
/// settings never change the result. Inputs: the folder and arguments.
/// Output: git's output; a failed git fails the test with its stderr.
pub fn git(folder: &Path, args: &[&str]) -> Output {
    let output = Command::new("git")
        .args(args)
        .current_dir(folder)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "E2E Owner")
        .env("GIT_AUTHOR_EMAIL", "e2e@example.invalid")
        .env("GIT_COMMITTER_NAME", "E2E Owner")
        .env("GIT_COMMITTER_EMAIL", "e2e@example.invalid")
        .stdin(Stdio::null())
        .output()
        .expect(GIT);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// Makes a git repository with one commit holding one file.
/// Inputs: the folder, the file name and its text.
pub fn repository(folder: &Path, file: &str, text: &str) {
    fs::create_dir_all(folder).unwrap();
    git(folder, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(folder.join(file), text).unwrap();
    git(folder, &["add", file]);
    git(folder, &["commit", "--quiet", "-m", "first"]);
}

/// A random lowercase hex text for markers and throwaway tokens.
/// Input: the byte count. Output: twice as many hex digits.
pub fn hex(bytes: usize) -> String {
    let mut random = vec![0_u8; bytes];
    getrandom::fill(&mut random).unwrap();
    random.iter().map(|byte| format!("{byte:02x}")).collect()
}
