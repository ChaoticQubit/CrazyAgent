use crazyagent::research::jobs::{ResearchJobs, ResearchStatus};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const CATALOG: &str = "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n\n[model_access]\naccounts = [\"local-account\"]\n\n[[model_access.connections]]\nid = \"local-connection\"\nprovider = \"openai-compatible\"\nharness = \"rcli\"\nprotocol = \"openai-chat\"\naccount_ref = \"local-account\"\nendpoint = { kind = \"http\", url = \"http://127.0.0.1:11434/v1\" }\nmodel = { id = \"qwen2.5:0.5b\", author = \"openai-compatible\" }\nauth = { method = \"none\" }\n\n[[model_access.pools]]\nid = \"local-pool\"\nmembers = [\"local-connection\"]\nprojects = [\"project-cafe\"]\nroles = [\"project_leader\"]\nfallback_allowed = false\n";

fn codex() -> PathBuf {
    let path = std::env::var_os("CRAZYAGENT_CODEX")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/opt/homebrew/Caskroom/codex/0.163.0/bin/codex"));
    assert!(
        path.is_file(),
        "real codex binary required: {}",
        path.display()
    );
    path
}

struct Engine {
    child: Child,
    stderr: Option<std::thread::JoinHandle<String>>,
}

impl Engine {
    fn start(runtime: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_crazyagent"))
            .arg("engine")
            .arg(runtime)
            .arg(runtime.join("global.toml"))
            .arg(runtime.join("project.toml"))
            .arg("engine.sock")
            .arg("--writable-settings")
            .arg("--codex-executable")
            .arg(codex())
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
            let mut rest = Vec::new();
            let _ = reader.take(65536).read_to_end(&mut rest);
        });
        let stderr = child.stderr.take().unwrap();
        let stderr = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.take(65536).read_to_string(&mut text);
            text
        });
        let line = ready
            .recv_timeout(Duration::from_secs(20))
            .unwrap_or_default();
        let mut engine = Self {
            child,
            stderr: Some(stderr),
        };
        if line != "ready\n" {
            let _ = engine.child.kill();
            let _ = engine.child.wait();
            panic!("engine not ready: {line:?} {}", engine.stderr_text());
        }
        engine
    }

    fn stderr_text(&mut self) -> String {
        self.stderr
            .take()
            .map(|handle| handle.join().unwrap_or_default())
            .unwrap_or_default()
    }

    fn stop(mut self) -> String {
        assert!(
            Command::new("/bin/kill")
                .args(["-s", "TERM", &self.child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "engine did not stop");
            std::thread::sleep(Duration::from_millis(20));
        };
        let stderr = self.stderr_text();
        assert!(status.success(), "{stderr}");
        stderr
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn runtime() -> PathBuf {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../.local-state")
        .canonicalize()
        .unwrap();
    assert!(
        base.ends_with("CrazyAgent/.local-state"),
        "{}",
        base.display()
    );
    let runtime = base.join(format!("r-sbx-{}", std::process::id()));
    if runtime.exists() {
        fs::remove_dir_all(&runtime).unwrap();
    }
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    for (name, text) in [("global.toml", CATALOG), ("project.toml", "")] {
        fs::write(runtime.join(name), text).unwrap();
        fs::set_permissions(runtime.join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    runtime
}

#[test]
fn the_engine_runs_queued_research_for_a_project_with_an_assigned_pool() {
    let runtime = runtime();
    let engine = Engine::start(&runtime);
    let projects = runtime.join("projects");
    let research = runtime.join("research");
    assert!(projects.is_dir());
    assert!(research.is_dir());
    let project = projects.join("project-cafe");
    fs::create_dir_all(project.join("context")).unwrap();
    fs::write(
        project.join("context").join("brief.md"),
        "# Corner cafe\n\nA website for a corner cafe with a menu and table booking.\n",
    )
    .unwrap();
    let jobs = ResearchJobs::open(&research.join("research.sqlite3")).unwrap();
    let id = jobs
        .enqueue("project-cafe", "market_research", "codex", true)
        .unwrap();
    let waiting = jobs
        .enqueue("project-unassigned", "design_research", "codex", true)
        .unwrap();
    fs::create_dir_all(projects.join("project-unassigned")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let job = jobs.get(id).unwrap().unwrap();
        if matches!(job.status, ResearchStatus::Done | ResearchStatus::Failed) {
            assert_eq!(job.status, ResearchStatus::Done, "{:?}", job.error);
            break;
        }
        assert!(Instant::now() < deadline, "research did not finish");
        std::thread::sleep(Duration::from_millis(500));
    }
    assert_eq!(
        jobs.get(waiting).unwrap().unwrap().status.label(),
        "Needs a pool"
    );
    let stderr = engine.stop();
    assert!(!stderr.contains("panicked"), "{stderr}");
    let report = fs::read_to_string(
        project
            .join("context")
            .join("research")
            .join("market_research.md"),
    )
    .unwrap();
    assert!(report.starts_with("# Market research\n"), "{report}");
    assert!(project.join("worktrees").is_dir());
    fs::remove_dir_all(&runtime).unwrap();
}
