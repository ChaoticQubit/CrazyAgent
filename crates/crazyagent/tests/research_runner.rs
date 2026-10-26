#![cfg(target_os = "macos")]

mod research_support;

use crazyagent::config::{ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference};
use crazyagent::protocol::CredentialRef;
use crazyagent::research::jobs::{ResearchJobs, ResearchStatus};
use crazyagent::research::runner::{
    ResearchAccount, ResearchLimits, ResearchProject, ResearchRunner, RunError,
};
use research_support::{EngineRuntime, claude, codex, hanging_endpoint, processes_with};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const OLLAMA_HOST: &str = "127.0.0.1:11434";
const OLLAMA: &str = "http://127.0.0.1:11434/v1";
const OLLAMA_ANTHROPIC: &str = "http://127.0.0.1:11434";
const OLLAMA_MODEL: &str = "qwen2.5:7b";
const OLLAMA_TIME: Duration = Duration::from_secs(10);
const OLLAMA_TAGS_BYTES_MAX: u64 = 1024 * 1024;

static LOCAL_MODEL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Takes the one live local model slot, after [`require_model`]. Input: none. Output: the
/// guard that keeps the other live tests waiting until it drops.
fn local_model() -> std::sync::MutexGuard<'static, ()> {
    let guard = LOCAL_MODEL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    require_model();
    guard
}

/// Fails the test with the command that fixes it unless the local Ollama the live tests
/// reach serves their model. Input: none; reads the model list from Ollama's `/api/tags`.
/// Output: none.
fn require_model() {
    assert_eq!(OLLAMA, format!("http://{OLLAMA_HOST}/v1"));
    assert_eq!(OLLAMA_ANTHROPIC, format!("http://{OLLAMA_HOST}"));
    let listed = crazyagent::spawn_gate::connect_tcp_std(OLLAMA_HOST).and_then(|mut stream| {
        stream.set_read_timeout(Some(OLLAMA_TIME))?;
        stream.set_write_timeout(Some(OLLAMA_TIME))?;
        stream.write_all(
            format!("GET /api/tags HTTP/1.1\r\nHost: {OLLAMA_HOST}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )?;
        let mut reply = String::new();
        stream
            .take(OLLAMA_TAGS_BYTES_MAX)
            .read_to_string(&mut reply)?;
        Ok(reply)
    });
    let reply = listed.unwrap_or_else(|problem| {
        panic!("the local Ollama at {OLLAMA_HOST} is required and did not answer: {problem}")
    });
    assert!(
        reply.contains(&format!("\"name\":\"{OLLAMA_MODEL}\"")),
        "the local Ollama does not have {OLLAMA_MODEL}; run: ollama pull {OLLAMA_MODEL}"
    );
}

struct Case {
    _runtime: EngineRuntime,
    project: PathBuf,
    crazyagent: PathBuf,
    state: PathBuf,
    database: PathBuf,
}

fn case(about: &str) -> Case {
    let runtime =
        EngineRuntime::with_projects(&[("project-a", about), ("project-b", "A second project.")]);
    let project = runtime.folder("project-a");
    Case {
        crazyagent: runtime.crazyagent("project-a"),
        project,
        state: runtime.state.clone(),
        database: runtime.database.clone(),
        _runtime: runtime,
    }
}

fn wait_for_file(path: &Path, deadline: Duration) {
    let until = Instant::now() + deadline;
    while !path.is_file() {
        assert!(Instant::now() < until, "{} never appeared", path.display());
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn runner(case: &Case, limits: ResearchLimits) -> ResearchRunner {
    ResearchRunner::new(&case.database, &case.state, &codex(), limits).unwrap()
}

fn limits(wall_time: Duration) -> ResearchLimits {
    ResearchLimits {
        wall_time,
        output_bytes_max: 4 * 1024 * 1024,
        report_bytes_max: 512 * 1024,
        running_max: 2,
        attempts_max: 1,
    }
}

fn endpoint(port: u16) -> ResearchAccount {
    ResearchAccount::Endpoint {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        model: "hanging-model".into(),
    }
}

fn project<'a>(case: &'a Case, account: Option<ResearchAccount>) -> ResearchProject<'a> {
    ResearchProject {
        id: "project-a",
        root: &case.project,
        account,
    }
}

fn files_containing(root: &Path, needle: &str) -> usize {
    let mut found = 0;
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            found += files_containing(&path, needle);
        } else if kind.is_file()
            && fs::read_to_string(&path).is_ok_and(|text| text.contains(needle))
        {
            found += 1;
        }
    }
    found
}

/// Asserts no agent worktree was added to the project: project setup creates
/// the `.crazyagent/worktrees` folder empty and only a started agent adds one.
fn assert_no_agent_worktree(case: &Case) {
    let worktrees = case.crazyagent.join("worktrees");
    assert!(worktrees.is_dir(), "{} is missing", worktrees.display());
    let added: Vec<_> = fs::read_dir(&worktrees)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(added.is_empty(), "{added:?}");
}

#[test]
fn queued_research_runs_codex_in_the_project_sandbox_and_writes_a_markdown_report() {
    let _model = local_model();
    let marker = format!("BRIEF-MARKER-{}", std::process::id());
    let case = case(&format!(
        "A web app for small vet clinics to take bookings. {marker}"
    ));
    let jobs = ResearchJobs::open(&case.database).unwrap();
    let id = jobs
        .enqueue("project-a", "market_research", "codex", true)
        .unwrap();
    let runner = runner(&case, limits(Duration::from_secs(600)));
    let account = ResearchAccount::Endpoint {
        base_url: OLLAMA.into(),
        model: OLLAMA_MODEL.into(),
    };
    let started = Instant::now();
    let run = runner
        .run_once(&project(&case, Some(account)))
        .unwrap()
        .unwrap();
    eprintln!("research run took {:?}", started.elapsed());
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(run.job, id);
    assert_eq!(
        job.status,
        ResearchStatus::Done,
        "{:?}",
        job.error.as_deref()
    );
    assert_eq!(run.status, ResearchStatus::Done);
    assert_eq!(
        job.report_path.as_deref(),
        Some("context/research/market_research.md")
    );
    let report = fs::read_to_string(
        case.crazyagent
            .join("context")
            .join("research")
            .join("market_research.md"),
    )
    .unwrap();
    assert!(report.starts_with("# Market research\n"), "{report}");
    assert!(report.contains("Researched by Codex"), "{report}");
    assert!(report.contains("## Links and references"), "{report}");
    assert!(report.len() > 200, "{report}");
    let worktree = case
        .crazyagent
        .join("worktrees")
        .join(format!("research-market_research-{id}-1"));
    assert!(worktree.join(".git").is_file());
    let profile =
        fs::read_to_string(case.state.join("profiles").join(format!("job-{id}-1.sb"))).unwrap();
    assert!(profile.starts_with("(version 1)\n(deny default)\n"));
    assert!(profile.contains(&format!("(subpath \"{}\")", case.project.display())));
    assert!(profile.contains(&format!("(subpath \"{}/context\")", case.project.display())));
    assert!(profile.contains(&format!(
        "(subpath \"{}/context\")",
        case.crazyagent.display()
    )));
    assert!(
        files_containing(&case.state.join("agents"), &marker) > 0,
        "the prompt sent to the agent must carry the project brief"
    );
    assert!(job.output_bytes > 0);
}

#[test]
fn research_without_a_pool_waits_with_a_plain_status() {
    let case = case("A plain project.");
    let jobs = ResearchJobs::open(&case.database).unwrap();
    let id = jobs
        .enqueue("project-a", "design_research", "codex", true)
        .unwrap();
    let run = runner(&case, limits(Duration::from_secs(5)))
        .run_once(&project(&case, None))
        .unwrap()
        .unwrap();
    assert_eq!(run.status, ResearchStatus::NeedsPool);
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(job.status.label(), "Needs a pool");
    assert_no_agent_worktree(&case);
}

#[test]
fn research_with_a_harness_that_cannot_run_yet_fails_plainly() {
    let case = case("A plain project.");
    let jobs = ResearchJobs::open(&case.database).unwrap();
    let id = jobs
        .enqueue("project-a", "design_research", "gemini-cli", true)
        .unwrap();
    let (port, _server) = hanging_endpoint();
    let mut bounds = limits(Duration::from_secs(5));
    bounds.attempts_max = 3;
    let run = runner(&case, bounds)
        .run_once(&project(&case, Some(endpoint(port))))
        .unwrap()
        .unwrap();
    assert_eq!(run.status, ResearchStatus::Failed);
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(
        job.error.as_deref(),
        Some("This harness cannot run research yet")
    );
}

#[test]
fn research_that_runs_out_of_time_is_stopped_and_retried_then_failed() {
    let case = case("A plain project.");
    let jobs = ResearchJobs::open(&case.database).unwrap();
    let id = jobs
        .enqueue("project-a", "competitor_research", "codex", true)
        .unwrap();
    let (port, _server) = hanging_endpoint();
    let mut bounds = limits(Duration::from_secs(3));
    bounds.attempts_max = 2;
    let runner = runner(&case, bounds);
    let started = Instant::now();
    let first = runner
        .run_once(&project(&case, Some(endpoint(port))))
        .unwrap()
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(first.status, ResearchStatus::Queued);
    assert_eq!(jobs.get(id).unwrap().unwrap().attempts, 1);
    let second = runner
        .run_once(&project(&case, Some(endpoint(port))))
        .unwrap()
        .unwrap();
    assert_eq!(second.status, ResearchStatus::Failed);
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(job.attempts, 2);
    assert_eq!(job.error.as_deref(), Some("Agent ran out of time"));
    assert!(!process_group_alive(first.process_group.unwrap()));
    assert!(!process_group_alive(second.process_group.unwrap()));
    assert!(
        !case
            .crazyagent
            .join("context")
            .join("research")
            .join("competitor_research.md")
            .exists()
    );
}

#[test]
fn research_output_beyond_the_limit_stops_the_agent() {
    let case = case("A plain project.");
    let jobs = ResearchJobs::open(&case.database).unwrap();
    let id = jobs
        .enqueue("project-a", "market_research", "codex", true)
        .unwrap();
    let (port, _server) = hanging_endpoint();
    let mut bounds = limits(Duration::from_secs(60));
    bounds.output_bytes_max = 16;
    let run = runner(&case, bounds)
        .run_once(&project(&case, Some(endpoint(port))))
        .unwrap()
        .unwrap();
    assert_eq!(run.status, ResearchStatus::Failed);
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(
        job.error.as_deref(),
        Some("Agent output exceeded the limit")
    );
    assert!(!process_group_alive(run.process_group.unwrap()));
}

#[test]
fn cancelling_running_research_stops_its_process_group() {
    let case = case("A plain project.");
    let jobs = ResearchJobs::open(&case.database).unwrap();
    let id = jobs
        .enqueue("project-a", "market_research", "codex", true)
        .unwrap();
    let (port, _server) = hanging_endpoint();
    let runner = Arc::new(runner(&case, limits(Duration::from_secs(120))));
    let project_root = case.project.clone();
    let handle = {
        let runner = runner.clone();
        std::thread::spawn(move || {
            runner
                .run_once(&ResearchProject {
                    id: "project-a",
                    root: &project_root,
                    account: Some(endpoint(port)),
                })
                .unwrap()
                .unwrap()
        })
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while jobs.get(id).unwrap().unwrap().status != ResearchStatus::Running {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_secs(1));
    assert!(jobs.request_cancel(id).unwrap());
    let cancelled_at = Instant::now();
    let run = handle.join().unwrap();
    assert!(cancelled_at.elapsed() < Duration::from_secs(10));
    assert_eq!(run.status, ResearchStatus::Cancelled);
    assert_eq!(jobs.get(id).unwrap().unwrap().status.label(), "Cancelled");
    assert!(!process_group_alive(run.process_group.unwrap()));
}

#[test]
fn spawn_ready_runs_at_most_the_per_project_limit_at_once() {
    let case = case("A plain project.");
    let jobs = ResearchJobs::open(&case.database).unwrap();
    let ids: Vec<_> = ["market_research", "competitor_research", "design_research"]
        .iter()
        .map(|kind| jobs.enqueue("project-a", kind, "codex", true).unwrap())
        .collect();
    let (port, _server) = hanging_endpoint();
    let runner = Arc::new(runner(&case, limits(Duration::from_secs(120))));
    let handles = runner.spawn_ready(
        "project-a".into(),
        "codex",
        case.project.clone(),
        Some((
            CredentialRef::new("local-connection").unwrap(),
            endpoint(port),
        )),
    );
    assert_eq!(handles.len(), 2);
    let listed = jobs.list("project-a").unwrap();
    let running = listed
        .iter()
        .filter(|job| job.status == ResearchStatus::Running)
        .count();
    assert_eq!(running, 2);
    assert_eq!(
        listed
            .iter()
            .filter(|job| job.status == ResearchStatus::Queued)
            .count(),
        1
    );
    for id in &ids {
        jobs.request_cancel(*id).unwrap();
    }
    for handle in handles {
        let run = handle.join().unwrap().unwrap();
        assert_eq!(run.status, ResearchStatus::Cancelled);
    }
    assert!(
        jobs.list("project-a")
            .unwrap()
            .iter()
            .all(|job| job.status == ResearchStatus::Cancelled)
    );
}

fn process_group_alive(group: u32) -> bool {
    let pid = rustix::process::Pid::from_raw(group as i32).unwrap();
    rustix::process::test_kill_process_group(pid).is_ok()
}

#[test]
fn research_with_tools_off_reaches_only_the_model_endpoint_and_still_reports() {
    let _model = local_model();
    let case = case("A mobile app that stores family recipes.");
    let jobs = ResearchJobs::open(&case.database).unwrap();
    let id = jobs
        .enqueue("project-a", "design_research", "codex", false)
        .unwrap();
    let run = runner(&case, limits(Duration::from_secs(600)))
        .run_once(&project(
            &case,
            Some(ResearchAccount::Endpoint {
                base_url: OLLAMA.into(),
                model: OLLAMA_MODEL.into(),
            }),
        ))
        .unwrap()
        .unwrap();
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(run.status, ResearchStatus::Done, "{:?}", job.error);
    let profile =
        fs::read_to_string(case.state.join("profiles").join(format!("job-{id}-1.sb"))).unwrap();
    assert!(!profile.contains("(allow network*)"));
    assert!(profile.contains("(allow network-outbound (remote ip \"localhost:11434\"))"));
    let report = fs::read_to_string(
        case.crazyagent
            .join("context")
            .join("research")
            .join("design_research.md"),
    )
    .unwrap();
    assert!(report.starts_with("# Design research\n"), "{report}");
}

#[test]
fn queued_research_runs_claude_code_in_the_project_sandbox_on_an_anthropic_endpoint() {
    let _model = local_model();
    let marker = format!("CLAUDE-BRIEF-{}", std::process::id());
    let case = case(&format!(
        "A mobile app that matches dog owners with local walkers. {marker}"
    ));
    let jobs = ResearchJobs::open(&case.database).unwrap();
    let id = jobs
        .enqueue("project-a", "competitor_research", "claude", false)
        .unwrap();
    let runner = runner(&case, limits(Duration::from_secs(600)))
        .with_claude(&claude())
        .unwrap();
    let account = ResearchAccount::AnthropicEndpoint {
        base_url: OLLAMA_ANTHROPIC.into(),
        model: OLLAMA_MODEL.into(),
    };
    let started = Instant::now();
    let run = runner
        .run_once(&project(&case, Some(account)))
        .unwrap()
        .unwrap();
    eprintln!("claude research run took {:?}", started.elapsed());
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(
        job.status,
        ResearchStatus::Done,
        "{:?} {}",
        job.error,
        fs::read_to_string(
            case.state
                .join("agents")
                .join(format!("job-{id}-1/stderr.log"))
        )
        .unwrap_or_default()
    );
    assert_eq!(run.status, ResearchStatus::Done);
    let report = fs::read_to_string(
        case.crazyagent
            .join("context")
            .join("research")
            .join("competitor_research.md"),
    )
    .unwrap();
    assert!(report.starts_with("# Competitor research\n"), "{report}");
    assert!(report.contains("Researched by Claude Code"), "{report}");
    assert!(report.contains("## Links and references"), "{report}");
    assert!(
        case.crazyagent
            .join("worktrees")
            .join(format!("research-competitor_research-{id}-1"))
            .join(".git")
            .is_file()
    );
    assert!(
        files_containing(&case.state.join("agents"), &marker) > 0,
        "the prompt sent to the agent must carry the project brief"
    );
    assert!(!process_group_alive(run.process_group.unwrap()));
}

#[test]
fn a_job_waits_for_a_pool_when_the_account_does_not_fit_its_harness() {
    let case = case("A plain project.");
    let jobs = ResearchJobs::open(&case.database).unwrap();
    let claude_job = jobs
        .enqueue("project-a", "design_research", "claude-code", true)
        .unwrap();
    let runner = runner(&case, limits(Duration::from_secs(5)))
        .with_claude(&claude())
        .unwrap();
    let run = runner
        .run_once(&project(&case, Some(endpoint(9))))
        .unwrap()
        .unwrap();
    assert_eq!(run.job, claude_job);
    assert_eq!(run.status, ResearchStatus::NeedsPool);
    let codex_job = jobs
        .enqueue("project-b", "design_research", "codex", true)
        .unwrap();
    let anthropic = ResearchAccount::AnthropicEndpoint {
        base_url: "http://127.0.0.1:9".into(),
        model: "m".into(),
    };
    let run = runner
        .run_once(&ResearchProject {
            id: "project-b",
            root: &case.project,
            account: Some(anthropic),
        })
        .unwrap()
        .unwrap();
    assert_eq!(run.job, codex_job);
    assert_eq!(run.status, ResearchStatus::NeedsPool);
    assert_eq!(jobs.get(codex_job).unwrap().unwrap().attempts, 0);
    assert_no_agent_worktree(&case);
}

#[test]
fn a_claude_job_without_claude_installed_fails_plainly() {
    let case = case("A plain project.");
    let jobs = ResearchJobs::open(&case.database).unwrap();
    let id = jobs
        .enqueue("project-a", "design_research", "claude", true)
        .unwrap();
    let account = ResearchAccount::AnthropicEndpoint {
        base_url: "http://127.0.0.1:9".into(),
        model: "m".into(),
    };
    let run = runner(&case, limits(Duration::from_secs(5)))
        .run_once(&project(&case, Some(account)))
        .unwrap()
        .unwrap();
    assert_eq!(run.status, ResearchStatus::Failed);
    assert_eq!(
        jobs.get(id).unwrap().unwrap().error.as_deref(),
        Some("This harness is not installed on this computer")
    );
}

#[test]
fn a_job_interrupted_by_an_engine_stop_runs_again_after_a_restart() {
    let case = case("A plain project.");
    let jobs = ResearchJobs::open(&case.database).unwrap();
    let id = jobs
        .enqueue("project-a", "market_research", "codex", true)
        .unwrap();
    let (port, _server) = hanging_endpoint();
    let first = Arc::new(runner(&case, limits(Duration::from_secs(120))));
    let handle = {
        let first = first.clone();
        let root = case.project.clone();
        std::thread::spawn(move || {
            first
                .run_once(&ResearchProject {
                    id: "project-a",
                    root: &root,
                    account: Some(endpoint(port)),
                })
                .unwrap()
                .unwrap()
        })
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while jobs.get(id).unwrap().unwrap().status != ResearchStatus::Running {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_secs(1));
    first.shutdown();
    let interrupted = handle.join().unwrap();
    assert_eq!(interrupted.status, ResearchStatus::Queued);
    assert!(!process_group_alive(interrupted.process_group.unwrap()));
    assert_eq!(jobs.get(id).unwrap().unwrap().attempts, 0);
    let restarted = runner(&case, limits(Duration::from_secs(3)));
    let rerun = restarted
        .run_once(&project(&case, Some(endpoint(port))))
        .unwrap()
        .unwrap();
    assert_eq!(rerun.job, id);
    assert_eq!(rerun.status, ResearchStatus::Failed);
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(job.attempts, 1);
    assert_eq!(job.error.as_deref(), Some("Agent ran out of time"));
    assert!(!process_group_alive(rerun.process_group.unwrap()));
    let worktrees = fs::read_dir(case.crazyagent.join("worktrees"))
        .unwrap()
        .count();
    assert_eq!(worktrees, 2);
}

#[test]
fn each_codex_launch_gets_the_project_context_in_its_home_only_while_it_runs() {
    let marker = format!("CODEX-CONTEXT-{}", std::process::id());
    let case = case(&format!("A booking site for a bike repair shop. {marker}"));
    let jobs = ResearchJobs::open(&case.database).unwrap();
    let id = jobs
        .enqueue("project-a", "market_research", "codex", true)
        .unwrap();
    let (port, _server) = hanging_endpoint();
    let runner = Arc::new(runner(&case, limits(Duration::from_secs(120))));
    let handle = {
        let runner = runner.clone();
        let root = case.project.clone();
        std::thread::spawn(move || {
            runner
                .run_once(&ResearchProject {
                    id: "project-a",
                    root: &root,
                    account: Some(endpoint(port)),
                })
                .unwrap()
                .unwrap()
        })
    };
    let home = case
        .state
        .join("agents")
        .join(format!("job-{id}-1"))
        .join("home");
    wait_for_file(&home.join("config.toml"), Duration::from_secs(30));
    let config = fs::read_to_string(home.join("config.toml")).unwrap();
    assert!(config.contains("developer_instructions"), "{config}");
    assert!(config.contains(&marker), "{config}");
    assert!(home.join("crazyagent").is_dir());
    std::thread::sleep(Duration::from_secs(1));
    assert!(jobs.request_cancel(id).unwrap());
    let run = handle.join().unwrap();
    assert_eq!(run.status, ResearchStatus::Cancelled);
    assert!(!process_group_alive(run.process_group.unwrap()));
    assert!(!home.join("config.toml").exists());
    assert!(!home.join("crazyagent").exists());
}

#[test]
fn each_claude_launch_gets_the_project_context_as_files_it_is_pointed_at() {
    let marker = format!("CLAUDE-CONTEXT-{}", std::process::id());
    let case = case(&format!("A site that books piano lessons. {marker}"));
    let jobs = ResearchJobs::open(&case.database).unwrap();
    let id = jobs
        .enqueue("project-a", "design_research", "claude", false)
        .unwrap();
    let (port, _server) = hanging_endpoint();
    let runner = Arc::new(
        runner(&case, limits(Duration::from_secs(120)))
            .with_claude(&claude())
            .unwrap(),
    );
    let handle = {
        let runner = runner.clone();
        let root = case.project.clone();
        std::thread::spawn(move || {
            runner
                .run_once(&ResearchProject {
                    id: "project-a",
                    root: &root,
                    account: Some(ResearchAccount::AnthropicEndpoint {
                        base_url: format!("http://127.0.0.1:{port}"),
                        model: "hanging-model".into(),
                    }),
                })
                .unwrap()
                .unwrap()
        })
    };
    let state = case
        .state
        .join("agents")
        .join(format!("job-{id}-1"))
        .join("home")
        .join("crazyagent");
    wait_for_file(&state.join("instructions.md"), Duration::from_secs(30));
    let instructions = fs::read_to_string(state.join("instructions.md")).unwrap();
    assert!(instructions.contains(&marker), "{instructions}");
    let servers = fs::read_to_string(state.join("mcp.json")).unwrap();
    assert!(servers.contains("mcpServers"), "{servers}");
    assert!(!state.join("plugin").exists());
    std::thread::sleep(Duration::from_secs(1));
    assert!(jobs.request_cancel(id).unwrap());
    let run = handle.join().unwrap();
    assert_eq!(run.status, ResearchStatus::Cancelled);
    assert!(!process_group_alive(run.process_group.unwrap()));
    assert!(!state.exists());
}

#[test]
fn a_managed_account_not_signed_in_for_this_engine_run_waits_for_a_pool() {
    let case = case("A site for a neighbourhood tool library.");
    let jobs = ResearchJobs::open(&case.database).unwrap();
    let id = jobs
        .enqueue("project-a", "market_research", "codex", true)
        .unwrap();
    let reference = |value: &str| CredentialRef::new(value).unwrap();
    let connection = ModelConnection {
        id: reference("codex-connection"),
        provider: reference("openai"),
        harness: reference("codex"),
        protocol: reference("native"),
        account_ref: reference("codex-account"),
        endpoint: ModelEndpoint::Native {},
        model: ModelReference {
            id: "native-default".into(),
            author: reference("openai"),
        },
        auth: ModelAuthentication::NativeManaged {
            profile_ref: reference("codex-profile"),
        },
    };
    let run = runner(&case, limits(Duration::from_secs(5)))
        .run_once(&project(
            &case,
            Some(ResearchAccount::ManagedCodex {
                connection,
                model: None,
            }),
        ))
        .unwrap()
        .unwrap();
    assert_eq!(run.job, id);
    assert_eq!(run.status, ResearchStatus::NeedsPool);
    assert_eq!(run.process_group, None);
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(job.status.label(), "Needs a pool");
    assert_eq!(job.attempts, 0);
    assert_no_agent_worktree(&case);
    assert!(
        !case
            .state
            .join("agents")
            .join(format!("job-{id}-1"))
            .exists()
    );
}

#[test]
fn research_for_a_project_the_engine_does_not_know_fails_without_starting_an_agent() {
    let case = case("A plain project.");
    let jobs = ResearchJobs::open(&case.database).unwrap();
    let id = jobs
        .enqueue("project-ghost", "market_research", "codex", true)
        .unwrap();
    let result = runner(&case, limits(Duration::from_secs(5))).run_once(&ResearchProject {
        id: "project-ghost",
        root: &case.project,
        account: Some(endpoint(9)),
    });
    assert!(
        matches!(&result, Err(RunError::Context(code)) if code.0 == "plugin_project_unknown"),
        "{result:?}"
    );
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(job.status, ResearchStatus::Failed);
    assert_eq!(
        job.error.as_deref(),
        Some("Could not give the agent this project's context")
    );
    assert_no_agent_worktree(&case);
    assert!(
        !case.state.join("agents").exists()
            || fs::read_dir(case.state.join("agents")).unwrap().count() == 0
    );
}

const RESEARCH_ENGINE_STATE: &str = "CRAZYAGENT_TEST_RESEARCH_ENGINE_STATE";
const RESEARCH_ENGINE_PROJECT: &str = "CRAZYAGENT_TEST_RESEARCH_ENGINE_PROJECT";
const RESEARCH_ENGINE_HARNESS: &str = "CRAZYAGENT_TEST_RESEARCH_ENGINE_HARNESS";

/// A private folder under `/tmp` for a test harness. Takes nothing. Returns the folder,
/// removed when dropped.
fn harness_folder() -> tempfile::TempDir {
    let folder = tempfile::Builder::new()
        .prefix("ca-research-harness-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    fs::set_permissions(folder.path(), fs::Permissions::from_mode(0o700)).unwrap();
    folder
}

/// Writes a harness standing in for Codex that leaves what a shell tool can leave: a
/// process in its own session that writes `detached` in the agent's temp folder once its
/// session has started and then sleeps `seconds`. The harness waits for that file, then
/// sleeps the same time when it `stays`, or exits. Takes the folder, the seconds and whether
/// it stays. Returns the harness.
fn detaching_harness(folder: &Path, seconds: u32, stays: bool) -> PathBuf {
    let then = if stays {
        format!("exec /bin/sleep {seconds}")
    } else {
        "exit 0".to_owned()
    };
    let harness = folder.canonicalize().unwrap().join("codex");
    fs::write(
        &harness,
        format!(
            r#"#!/bin/sh
/usr/bin/perl -mPOSIX -e 'POSIX::setsid() > 0 or exit 1; open(my $file, ">", "$ENV{{TMPDIR}}/detached") or exit 1; close($file); exec q(/bin/sleep {seconds})' < /dev/null > /dev/null 2>&1 &
while [ ! -e "$TMPDIR/detached" ]; do /bin/sleep 0.05; done
{then}
"#
        ),
    )
    .unwrap();
    fs::set_permissions(&harness, fs::Permissions::from_mode(0o700)).unwrap();
    harness
}

/// Waits up to 30 s for at least `count` processes to name `needle`. Takes the text and
/// the count. Returns their ids.
fn running_naming(needle: &str, count: usize) -> Vec<u32> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let found = processes_with(Path::new(needle));
        if found.len() >= count {
            return found;
        }
        assert!(Instant::now() < deadline, "{needle}: only {found:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Waits up to `wait` for no process to name `needle`, then kills by id any that still do,
/// so a failing test leaves nothing behind. Takes the text and the wait. Returns the ids
/// that outlived the wait.
fn survivors_naming(needle: &str, wait: Duration) -> Vec<u32> {
    let deadline = Instant::now() + wait;
    loop {
        let left = processes_with(Path::new(needle));
        if left.is_empty() || Instant::now() >= deadline {
            for pid in &left {
                let pid = rustix::process::Pid::from_raw(i32::try_from(*pid).unwrap()).unwrap();
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
            return left;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn cancelling_research_stops_what_its_harness_detached_into_its_own_session() {
    const SLEEP: &str = "sleep 3301";
    let case = case("A plain project.");
    let folder = harness_folder();
    let harness = detaching_harness(folder.path(), 3301, true);
    let jobs = ResearchJobs::open(&case.database).unwrap();
    let id = jobs
        .enqueue("project-a", "market_research", "codex", true)
        .unwrap();
    let runner = Arc::new(
        ResearchRunner::new(
            &case.database,
            &case.state,
            &harness,
            limits(Duration::from_secs(120)),
        )
        .unwrap(),
    );
    let project_root = case.project.clone();
    let handle = {
        let runner = runner.clone();
        std::thread::spawn(move || {
            runner
                .run_once(&ResearchProject {
                    id: "project-a",
                    root: &project_root,
                    account: Some(endpoint(9)),
                })
                .unwrap()
                .unwrap()
        })
    };
    running_naming(SLEEP, 2);
    assert!(jobs.request_cancel(id).unwrap());
    let run = handle.join().unwrap();
    assert_eq!(run.status, ResearchStatus::Cancelled);
    assert_eq!(
        survivors_naming(SLEEP, Duration::from_secs(2)),
        Vec::<u32>::new()
    );
}

#[test]
fn research_whose_harness_exits_stops_what_it_detached_into_its_own_session() {
    const SLEEP: &str = "sleep 3302";
    let case = case("A plain project.");
    let folder = harness_folder();
    let harness = detaching_harness(folder.path(), 3302, false);
    let jobs = ResearchJobs::open(&case.database).unwrap();
    jobs.enqueue("project-a", "market_research", "codex", true)
        .unwrap();
    let run = ResearchRunner::new(
        &case.database,
        &case.state,
        &harness,
        limits(Duration::from_secs(60)),
    )
    .unwrap()
    .run_once(&project(&case, Some(endpoint(9))))
    .unwrap()
    .unwrap();
    assert_eq!(run.status, ResearchStatus::Failed);
    assert_eq!(
        survivors_naming(SLEEP, Duration::from_secs(2)),
        Vec::<u32>::new()
    );
}

/// Runs the research job in a second engine process, which this test runs again as itself
/// with the `RESEARCH_ENGINE_*` variables naming the runtime, and kills that engine with
/// SIGKILL once its harness and the detached process both run.
#[test]
fn research_whose_engine_dies_leaves_nothing_its_harness_detached() {
    const SLEEP: &str = "sleep 3303";
    if let (Some(state), Some(project), Some(harness)) = (
        std::env::var_os(RESEARCH_ENGINE_STATE),
        std::env::var_os(RESEARCH_ENGINE_PROJECT),
        std::env::var_os(RESEARCH_ENGINE_HARNESS),
    ) {
        let state = PathBuf::from(state);
        let database = state.parent().unwrap().join("engine.db");
        let ran = ResearchRunner::new(
            &database,
            &state,
            Path::new(&harness),
            limits(Duration::from_secs(120)),
        )
        .unwrap()
        .run_once(&ResearchProject {
            id: "project-a",
            root: Path::new(&project),
            account: Some(endpoint(9)),
        })
        .is_ok();
        panic!("the engine under test was not killed, run ok: {ran}");
    }
    let case = case("A plain project.");
    let folder = harness_folder();
    let harness = detaching_harness(folder.path(), 3303, true);
    let jobs = ResearchJobs::open(&case.database).unwrap();
    jobs.enqueue("project-a", "market_research", "codex", true)
        .unwrap();
    assert_eq!(
        case.database,
        case.state.parent().unwrap().join("engine.db")
    );
    let mut engine = crazyagent::spawn_gate::spawn(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "research_whose_engine_dies_leaves_nothing_its_harness_detached",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(RESEARCH_ENGINE_STATE, &case.state)
            .env(RESEARCH_ENGINE_PROJECT, &case.project)
            .env(RESEARCH_ENGINE_HARNESS, &harness)
            .stdin(Stdio::null()),
    )
    .unwrap();
    running_naming(SLEEP, 2);
    let exited = engine.try_wait().unwrap();
    engine.kill().unwrap();
    engine.wait().unwrap();
    assert_eq!(exited, None);
    assert_eq!(
        survivors_naming(SLEEP, Duration::from_secs(5)),
        Vec::<u32>::new()
    );
}
