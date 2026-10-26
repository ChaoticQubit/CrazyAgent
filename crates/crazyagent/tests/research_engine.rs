#![cfg(unix)]

mod research_support;

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::owner::OwnerClientKind;
use crazyagent::research::jobs::{ResearchJobs, ResearchStatus};
use crazyagent::terminal::FOLDER;
use crazyagent::terminal::tmux::{SOCKET, installed};
use research_support::{claude, codex, counted_hanging_endpoint, processes_with};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::panic::{AssertUnwindSafe, catch_unwind, panic_any};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const CATALOG: &str = "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n\n[model_access]\naccounts = [\"anthropic-account\", \"local-account\"]\n\n[[model_access.connections]]\nid = \"anthropic-connection\"\nprovider = \"anthropic-compatible\"\nharness = \"claude\"\nprotocol = \"anthropic-messages\"\naccount_ref = \"anthropic-account\"\nendpoint = { kind = \"http\", url = \"http://127.0.0.1:11434\" }\nmodel = { id = \"qwen2.5:7b\", author = \"anthropic-compatible\" }\nauth = { method = \"none\" }\n\n[[model_access.connections]]\nid = \"local-connection\"\nprovider = \"openai-compatible\"\nharness = \"rcli\"\nprotocol = \"openai-chat\"\naccount_ref = \"local-account\"\nendpoint = { kind = \"http\", url = \"http://127.0.0.1:11434/v1\" }\nmodel = { id = \"qwen2.5:7b\", author = \"openai-compatible\" }\nauth = { method = \"none\" }\n\n[[model_access.pools]]\nid = \"local-pool\"\nmembers = [\"local-connection\", \"anthropic-connection\"]\nprojects = [\"project-cafe\"]\nroles = [\"project_leader\"]\nfallback_allowed = false\n";

const ENGINE_STOP_WAIT: Duration = Duration::from_secs(30);
const AGENT_END_WAIT: Duration = Duration::from_secs(10);
const MODEL: &str = "qwen2.5:7b";
const OLLAMA: &str = "127.0.0.1:11434";
const OLLAMA_TIME: Duration = Duration::from_secs(10);
const OLLAMA_TAGS_BYTES_MAX: u64 = 1024 * 1024;

/// Fails the test with the command that fixes it unless the local Ollama serves the pool
/// model. Input: none; reads the model list from Ollama's `/api/tags`. Output: none.
fn require_model() {
    assert_eq!(
        CATALOG
            .matches(&format!("model = {{ id = \"{MODEL}\""))
            .count(),
        2
    );
    let listed = crazyagent::spawn_gate::connect_tcp_std(OLLAMA).and_then(|mut stream| {
        stream.set_read_timeout(Some(OLLAMA_TIME))?;
        stream.set_write_timeout(Some(OLLAMA_TIME))?;
        stream.write_all(
            format!("GET /api/tags HTTP/1.1\r\nHost: {OLLAMA}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )?;
        let mut reply = String::new();
        stream
            .take(OLLAMA_TAGS_BYTES_MAX)
            .read_to_string(&mut reply)?;
        Ok(reply)
    });
    let reply = listed.unwrap_or_else(|problem| {
        panic!("the local Ollama at {OLLAMA} is required and did not answer: {problem}")
    });
    assert!(
        reply.contains(&format!("\"name\":\"{MODEL}\"")),
        "the local Ollama does not have {MODEL}; run: ollama pull {MODEL}"
    );
}

/// Creates `project-cafe` in the runtime's projects home through the owner
/// client of the running engine binary, retrying the attach until the owner
/// endpoint answers.
fn create_cafe(runtime: &Path) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut owner = loop {
                match OwnerController::attach(runtime, OwnerClientKind::Tui, Duration::from_secs(2))
                    .await
                {
                    Ok(owner) => break owner,
                    Err(error) => {
                        assert!(Instant::now() < deadline, "owner attach: {error:?}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            };
            research_support::setup_projects(
                &mut owner,
                &home(runtime),
                &[(
                    "project-cafe",
                    "A website for a corner cafe with a menu and table booking.",
                )],
            )
            .await;
        });
}

/// The engine's private tmux server. Agent sessions outlive the engine, so dropping
/// this ends the server and every pane left in it, also when the test fails.
struct Server {
    tmux: PathBuf,
    socket: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = crazyagent::spawn_gate::status(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(&self.socket)
                .arg("kill-server")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
    }
}

struct Engine {
    child: Child,
    stderr: Option<std::thread::JoinHandle<String>>,
}

impl Engine {
    fn start(runtime: &Path) -> Self {
        let mut child = crazyagent::spawn_gate::spawn(
            Command::new(env!("CARGO_BIN_EXE_crazyagent"))
                .arg("engine")
                .arg(runtime)
                .arg(runtime.join("global.toml"))
                .arg(runtime.join("project.toml"))
                .arg("engine.sock")
                .arg("--writable-settings")
                .arg("--codex-executable")
                .arg(codex())
                .arg("--claude-executable")
                .arg(claude())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
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
            crazyagent::spawn_gate::status(Command::new("/bin/kill").args([
                "-s",
                "TERM",
                &self.child.id().to_string()
            ]))
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
    /// Stops an engine that is still running the way the owner does, with
    /// SIGTERM, so it stops every research agent it started even when the test
    /// panicked. Kills it only if it is still running after `ENGINE_STOP_WAIT`.
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_some() {
            return;
        }
        let _ = crazyagent::spawn_gate::status(Command::new("/bin/kill").args([
            "-s",
            "TERM",
            &self.child.id().to_string(),
        ]));
        let deadline = Instant::now() + ENGINE_STOP_WAIT;
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The projects home beside a test runtime folder, outside the engine's data.
fn home(runtime: &Path) -> PathBuf {
    let mut name = runtime.file_name().unwrap().to_os_string();
    name.push("-home");
    runtime.with_file_name(name)
}

/// A fresh private runtime folder `ca-r-<name>-<random>` under the real temporary root,
/// short enough for the engine's sockets from any checkout, holding `catalog` as the
/// global settings. It and its projects home are kept after the test, so each test gets
/// its own engine folder and a failed run keeps its evidence.
fn runtime(name: &str, catalog: &str) -> PathBuf {
    let runtime = tempfile::Builder::new()
        .prefix(&format!("ca-r-{name}-"))
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap()
        .keep();
    fs::create_dir(home(&runtime)).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    for (name, text) in [("global.toml", catalog), ("project.toml", "")] {
        fs::write(runtime.join(name), text).unwrap();
        fs::set_permissions(runtime.join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    runtime
}

#[test]
fn the_engine_runs_queued_research_on_each_job_harness_from_the_engine_database() {
    require_model();
    let runtime = runtime("q", CATALOG);
    let server = installed(&runtime).map(|tmux| Server {
        tmux,
        socket: runtime.join(FOLDER).join(SOCKET),
    });
    let engine = Engine::start(&runtime);
    let projects = runtime.join("projects");
    let research = runtime.join("research");
    assert!(projects.is_dir());
    assert!(research.is_dir());
    create_cafe(&runtime);
    let project = home(&runtime).join("project-cafe");
    let state = project.join(".crazyagent");
    assert!(state.join("context").join("brief.md").is_file());
    let database = rusqlite::Connection::open(runtime.join("engine.db")).unwrap();
    database.busy_timeout(Duration::from_secs(5)).unwrap();
    let queue = |project: &str, kind: &str, harness: &str, tools: bool| -> i64 {
        database
            .query_row(
                "INSERT INTO research_jobs(project,kind,harness,tools,status,report_path,created_ms)
                VALUES (?1,?2,?3,?4,'queued',?5,1) RETURNING id",
                rusqlite::params![
                    project,
                    kind,
                    harness,
                    tools,
                    format!("context/research/{kind}.md")
                ],
                |row| row.get(0),
            )
            .unwrap()
    };
    let codex_job = queue("project-cafe", "front_end_design", "codex", true);
    let claude_job = queue("project-cafe", "competitor_research", "claude", false);
    let rcli_job = queue("project-cafe", "market_research", "rcli", true);
    let waiting = queue("project-unassigned", "design_research", "codex", true);
    drop(database);
    let jobs = ResearchJobs::open(&runtime.join("engine.db")).unwrap();
    fs::create_dir_all(projects.join("project-unassigned")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(900);
    for id in [codex_job, claude_job, rcli_job] {
        loop {
            let job = jobs.get(id).unwrap().unwrap();
            if matches!(
                job.status,
                ResearchStatus::Done | ResearchStatus::Failed | ResearchStatus::Cancelled
            ) {
                break;
            }
            assert!(Instant::now() < deadline, "research did not finish");
            std::thread::sleep(Duration::from_millis(500));
        }
    }
    let codex = jobs.get(codex_job).unwrap().unwrap();
    assert_eq!(codex.status, ResearchStatus::Done, "{:?}", codex.error);
    let claude = jobs.get(claude_job).unwrap().unwrap();
    assert_eq!(claude.status, ResearchStatus::Done, "{:?}", claude.error);
    let rcli = jobs.get(rcli_job).unwrap().unwrap();
    assert_eq!(rcli.status, ResearchStatus::Failed);
    assert_eq!(
        rcli.error.as_deref(),
        Some("This harness cannot run research yet")
    );
    assert_eq!(
        jobs.get(waiting).unwrap().unwrap().status.label(),
        "Needs a pool"
    );
    let stderr = engine.stop();
    assert!(!stderr.contains("panicked"), "{stderr}");
    let report = |kind: &str| {
        fs::read_to_string(
            state
                .join("context")
                .join("research")
                .join(format!("{kind}.md")),
        )
        .unwrap()
    };
    let front_end = report("front_end_design");
    assert!(front_end.starts_with("# Front-end design\n"), "{front_end}");
    assert!(front_end.contains("Researched by Codex"), "{front_end}");
    let competitors = report("competitor_research");
    assert!(
        competitors.starts_with("# Competitor research\n"),
        "{competitors}"
    );
    assert!(
        competitors.contains("Researched by Claude Code"),
        "{competitors}"
    );
    assert!(
        competitors.contains("## Links and references"),
        "{competitors}"
    );
    assert_eq!(
        jobs.get(codex_job).unwrap().unwrap().report_path.as_deref(),
        Some("context/research/front_end_design.md")
    );
    assert!(state.join("worktrees").is_dir());
    drop(server);
    fs::remove_dir_all(&runtime).unwrap();
    fs::remove_dir_all(home(&runtime)).unwrap();
}

#[test]
fn a_test_that_panics_while_the_engine_runs_research_leaves_no_agent_process() {
    const MID_RUN: &str = "the test fails while the engine's research agent runs";
    let (port, _endpoint, requests) = counted_hanging_endpoint();
    let catalog = CATALOG.replace(
        "http://127.0.0.1:11434/v1",
        &format!("http://127.0.0.1:{port}/v1"),
    );
    assert_ne!(catalog, CATALOG);
    let runtime = runtime("p", &catalog);
    let server = installed(&runtime).map(|tmux| Server {
        tmux,
        socket: runtime.join(FOLDER).join(SOCKET),
    });
    let marker = home(&runtime)
        .join("project-cafe")
        .join(".crazyagent")
        .join("worktrees")
        .join("research-market_research-");
    let mut agents = Vec::new();
    let unwound = catch_unwind(AssertUnwindSafe(|| {
        let _engine = Engine::start(&runtime);
        create_cafe(&runtime);
        let jobs = ResearchJobs::open(&runtime.join("engine.db")).unwrap();
        jobs.enqueue("project-cafe", "market_research", "codex", false)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(120);
        while requests.load(Ordering::SeqCst) == 0 || agents.is_empty() {
            assert!(
                Instant::now() < deadline,
                "no research agent asked its model"
            );
            std::thread::sleep(Duration::from_millis(200));
            agents = processes_with(&marker);
        }
        panic_any(MID_RUN);
    }));
    let payload = unwound.expect_err("the test must panic mid-run");
    assert_eq!(payload.downcast_ref::<&str>(), Some(&MID_RUN));
    assert!(!agents.is_empty());
    assert_eq!(processes_with(&marker), Vec::<u32>::new());
    drop(server);
    fs::remove_dir_all(&runtime).unwrap();
    fs::remove_dir_all(home(&runtime)).unwrap();
}

#[test]
fn killing_the_engine_ends_its_research_agents() {
    let (port, _endpoint, requests) = counted_hanging_endpoint();
    let catalog = CATALOG.replace(
        "http://127.0.0.1:11434/v1",
        &format!("http://127.0.0.1:{port}/v1"),
    );
    assert_ne!(catalog, CATALOG);
    let runtime = runtime("k", &catalog);
    let server = installed(&runtime).map(|tmux| Server {
        tmux,
        socket: runtime.join(FOLDER).join(SOCKET),
    });
    let marker = home(&runtime)
        .join("project-cafe")
        .join(".crazyagent")
        .join("worktrees")
        .join("research-market_research-");
    let mut engine = Engine::start(&runtime);
    create_cafe(&runtime);
    let jobs = ResearchJobs::open(&runtime.join("engine.db")).unwrap();
    jobs.enqueue("project-cafe", "market_research", "codex", false)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    while requests.load(Ordering::SeqCst) == 0 || processes_with(&marker).is_empty() {
        assert!(
            Instant::now() < deadline,
            "no research agent asked its model"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    engine.child.kill().unwrap();
    engine.child.wait().unwrap();
    let deadline = Instant::now() + AGENT_END_WAIT;
    let mut left = processes_with(&marker);
    while !left.is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        left = processes_with(&marker);
    }
    for pid in &left {
        let _ = crazyagent::spawn_gate::status(Command::new("/bin/kill").args([
            "-s",
            "KILL",
            &pid.to_string(),
        ]));
    }
    drop(server);
    assert_eq!(left, Vec::<u32>::new(), "agents outlived the killed engine");
    fs::remove_dir_all(&runtime).unwrap();
    fs::remove_dir_all(home(&runtime)).unwrap();
}

/// Reads a project's research list through the owner client of the running engine, the
/// read the TUI and the desktop make. Input: the runtime and the project id.
/// Output: the jobs as the owner sees them.
fn owner_jobs(runtime: &Path, project: &str) -> Vec<crazyagent::project_setup::ResearchJob> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut owner =
                OwnerController::attach(runtime, OwnerClientKind::Tui, Duration::from_secs(2))
                    .await
                    .unwrap();
            owner
                .research_jobs(crazyagent::protocol::ProjectId::new(project).unwrap())
                .await
                .unwrap()
        })
}

#[test]
fn research_waiting_on_a_rate_limit_reads_rate_limited_not_needs_a_pool() {
    let runtime = runtime("rl", CATALOG);
    let server = installed(&runtime).map(|tmux| Server {
        tmux,
        socket: runtime.join(FOLDER).join(SOCKET),
    });
    let engine = Engine::start(&runtime);
    create_cafe(&runtime);
    let paused_until = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
        + 10 * 60_000;
    let database = rusqlite::Connection::open(runtime.join("engine.db")).unwrap();
    database.busy_timeout(Duration::from_secs(5)).unwrap();
    database
        .execute(
            "INSERT INTO team_paused_accounts(connection,until_ms) VALUES ('anthropic-connection',?1)",
            [paused_until],
        )
        .unwrap();
    drop(database);
    let jobs = ResearchJobs::open(&runtime.join("engine.db")).unwrap();
    let id = jobs
        .enqueue("project-cafe", "competitor_research", "claude", false)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while jobs
        .get(id)
        .unwrap()
        .unwrap()
        .error
        .is_none_or(|detail| !detail.starts_with("rate_limited until "))
    {
        assert!(
            Instant::now() < deadline,
            "the job never waited on the rate limit: {:?}",
            jobs.get(id).unwrap()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let listed = owner_jobs(&runtime, "project-cafe");
    let job = listed.iter().find(|job| job.id == id).unwrap();
    assert_eq!(
        (job.status, job.status_label.as_str()),
        (
            crazyagent::project_setup::ResearchStatus::NeedsPool,
            "Rate limited"
        ),
        "{job:?}"
    );
    assert_eq!(
        job.rate_limited(),
        Some((
            paused_until - paused_until.rem_euclid(60_000),
            "Every account in this project's pool that can run this research hit its rate limit"
        )),
        "{job:?}"
    );
    let stderr = engine.stop();
    assert!(!stderr.contains("panicked"), "{stderr}");
    drop(server);
    fs::remove_dir_all(&runtime).unwrap();
    fs::remove_dir_all(home(&runtime)).unwrap();
}
