use crazyagent::research::jobs::{ResearchJobs, ResearchStatus};
use crazyagent::research::runner::{
    ResearchAccount, ResearchLimits, ResearchProject, ResearchRunner,
};
use std::fs;
use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const OLLAMA: &str = "http://127.0.0.1:11434/v1";
const OLLAMA_MODEL: &str = "qwen2.5:0.5b";

struct Case {
    _temp: tempfile::TempDir,
    project: PathBuf,
    state: PathBuf,
    database: PathBuf,
}

fn codex() -> PathBuf {
    let path = std::env::var_os("CRAZYAGENT_CODEX")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/opt/homebrew/bin/codex"))
        .canonicalize()
        .expect("real codex binary required");
    assert!(
        path.is_file(),
        "real codex binary required: {}",
        path.display()
    );
    path
}

fn case(brief: &str) -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    let state = root.join("state");
    fs::create_dir_all(project.join("context").join("documents")).unwrap();
    fs::create_dir_all(&state).unwrap();
    fs::write(project.join("context").join("brief.md"), brief).unwrap();
    fs::write(
        project.join("context").join("documents").join("notes.md"),
        "Customer interview notes.\n",
    )
    .unwrap();
    let database = state.join("research.sqlite3");
    Case {
        _temp: temp,
        project,
        state,
        database,
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

fn hanging_endpoint() -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().take(64) {
            let Ok(mut stream) = stream else { break };
            let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer);
            held.push(stream);
        }
    });
    (port, handle)
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

#[test]
fn queued_research_runs_codex_in_the_project_sandbox_and_writes_a_markdown_report() {
    let marker = format!("BRIEF-MARKER-{}", std::process::id());
    let case = case(&format!(
        "# Pet clinic booking app\n\nA web app for small vet clinics to take bookings. {marker}\n"
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
        case.project
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
        .project
        .join("worktrees")
        .join(format!("research-market_research-{id}-1"));
    assert!(worktree.join(".git").is_file());
    let profile =
        fs::read_to_string(case.state.join("profiles").join(format!("job-{id}-1.sb"))).unwrap();
    assert!(profile.starts_with("(version 1)\n(deny default)\n"));
    assert!(profile.contains(&format!("(subpath \"{}\")", case.project.display())));
    assert!(profile.contains(&format!("(subpath \"{}/context\")", case.project.display())));
    assert!(
        files_containing(&case.state.join("agents"), &marker) > 0,
        "the prompt sent to the agent must carry the project brief"
    );
    assert!(job.output_bytes > 0);
}

#[test]
fn research_without_a_pool_waits_with_a_plain_status() {
    let case = case("# Brief\n");
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
    assert!(!case.project.join("worktrees").exists());
}

#[test]
fn research_with_a_harness_that_cannot_run_yet_fails_plainly() {
    let case = case("# Brief\n");
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
    let case = case("# Brief\n");
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
            .project
            .join("context")
            .join("research")
            .join("competitor_research.md")
            .exists()
    );
}

#[test]
fn research_output_beyond_the_limit_stops_the_agent() {
    let case = case("# Brief\n");
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
    let case = case("# Brief\n");
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
    let case = case("# Brief\n");
    let jobs = ResearchJobs::open(&case.database).unwrap();
    let ids: Vec<_> = ["market_research", "competitor_research", "design_research"]
        .iter()
        .map(|kind| jobs.enqueue("project-a", kind, "codex", true).unwrap())
        .collect();
    let (port, _server) = hanging_endpoint();
    let runner = Arc::new(runner(&case, limits(Duration::from_secs(120))));
    let handles = runner.spawn_ready(
        "project-a".into(),
        case.project.clone(),
        Some(endpoint(port)),
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

#[test]
#[ignore = "BLOCKED until a signed-in managed Codex account exists; set CRAZYAGENT_MANAGED_CODEX_HOME to its managed profile directory"]
fn research_runs_on_a_signed_in_managed_codex_account() {
    let home = PathBuf::from(
        std::env::var_os("CRAZYAGENT_MANAGED_CODEX_HOME")
            .expect("CRAZYAGENT_MANAGED_CODEX_HOME must name a managed profile directory"),
    )
    .canonicalize()
    .unwrap();
    let case = case("# Habit tracker\n\nA mobile app that tracks daily habits.\n");
    let jobs = ResearchJobs::open(&case.database).unwrap();
    let id = jobs
        .enqueue("project-a", "competitor_research", "codex", true)
        .unwrap();
    let run = runner(&case, limits(Duration::from_secs(900)))
        .run_once(&project(
            &case,
            Some(ResearchAccount::ManagedCodex { home, model: None }),
        ))
        .unwrap()
        .unwrap();
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(run.status, ResearchStatus::Done, "{:?}", job.error);
    let report = fs::read_to_string(
        case.project
            .join("context")
            .join("research")
            .join("competitor_research.md"),
    )
    .unwrap();
    assert!(report.starts_with("# Competitor research\n"));
    assert!(report.contains("https://"));
}

fn process_group_alive(group: u32) -> bool {
    let pid = rustix::process::Pid::from_raw(group as i32).unwrap();
    rustix::process::test_kill_process_group(pid).is_ok()
}

#[test]
fn research_with_tools_off_reaches_only_the_model_endpoint_and_still_reports() {
    let case = case("# Recipe box\n\nA mobile app that stores family recipes.\n");
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
        case.project
            .join("context")
            .join("research")
            .join("design_research.md"),
    )
    .unwrap();
    assert!(report.starts_with("# Design research\n"), "{report}");
}
