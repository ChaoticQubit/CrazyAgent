#![cfg(unix)]

mod research_support;

use crazyagent::protocol::CredentialRef;
use crazyagent::research::accounts::AccountPick;
use crazyagent::research::jobs::{ResearchJobs, ResearchStatus};
use crazyagent::research::runner::{ResearchAccount, ResearchLimits, ResearchRunner};
use crazyagent::research::service::{ResearchService, progress};
use research_support::{EngineRuntime, codex, hanging_endpoint, processes_with};
use std::collections::BTreeMap;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind, panic_any};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn ollama() -> AccountPick {
    AccountPick::Account {
        connection: CredentialRef::new("local-connection").unwrap(),
        account: Box::new(ResearchAccount::Endpoint {
            base_url: "http://127.0.0.1:11434/v1".into(),
            model: "qwen2.5:0.5b".into(),
        }),
    }
}

fn wait_for(deadline: Duration, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + deadline;
    while !done() {
        assert!(
            Instant::now() < until,
            "condition not reached in {deadline:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn the_service_waits_for_a_pool_then_runs_research_and_reports_progress() {
    let runtime = EngineRuntime::with_projects(&[(
        "project-a",
        "A website where a small bakery takes cake orders.",
    )]);
    let project = runtime.folder("project-a");
    let state = runtime.state.clone();
    let database = runtime.database.clone();
    let jobs = ResearchJobs::open(&database).unwrap();
    let market = jobs
        .enqueue("project-a", "market_research", "codex", false)
        .unwrap();
    let pools: Arc<Mutex<BTreeMap<String, AccountPick>>> = Arc::new(Mutex::new(BTreeMap::new()));
    pools
        .lock()
        .unwrap()
        .insert("project-a".into(), AccountPick::NeedsPool);
    let limits = ResearchLimits {
        wall_time: Duration::from_secs(480),
        attempts_max: 1,
        ..ResearchLimits::default()
    };
    let runner = Arc::new(ResearchRunner::new(&database, &state, &codex(), limits).unwrap());
    let resolver = {
        let pools = pools.clone();
        let project = project.clone();
        move |id: &str, _harness: &str| {
            let pick = pools.lock().unwrap().get(id).cloned()?;
            Some((project.clone(), pick))
        }
    };
    let service = ResearchService::start(runner, resolver, Duration::from_millis(200));
    wait_for(Duration::from_secs(10), || {
        jobs.get(market).unwrap().unwrap().status == ResearchStatus::NeedsPool
    });
    let waiting = progress(&jobs, "project-a").unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].title, "Market research");
    assert_eq!(waiting[0].status, "Needs a pool");
    assert_eq!(waiting[0].report, None);
    pools
        .lock()
        .unwrap()
        .insert("project-a".into(), AccountPick::NoUsableAccount);
    wait_for(Duration::from_secs(10), || {
        progress(&jobs, "project-a").unwrap()[0].detail.as_deref()
            == Some("No account in this project's pool can run this research")
    });
    assert_eq!(
        progress(&jobs, "project-a").unwrap()[0].status,
        "Needs a pool"
    );
    pools.lock().unwrap().insert("project-a".into(), ollama());
    wait_for(Duration::from_secs(30), || {
        progress(&jobs, "project-a").unwrap()[0].status == "Running"
    });
    let running = progress(&jobs, "project-a").unwrap();
    assert!(running[0].detail.is_none());
    wait_for(Duration::from_secs(540), || {
        let status = jobs.get(market).unwrap().unwrap().status;
        matches!(status, ResearchStatus::Done | ResearchStatus::Failed)
    });
    service.stop();
    let finished = progress(&jobs, "project-a").unwrap();
    let agent = state.join("agents").join(format!("job-{market}-1"));
    assert!(agent.join("events.jsonl").is_file());
    assert!(agent.join("stderr.log").is_file());
    assert_eq!(
        finished[0].status,
        "Done",
        "{:?} {}",
        finished[0].detail,
        fs::read_to_string(agent.join("stderr.log")).unwrap_or_default()
    );
    assert_eq!(
        finished[0].report.as_deref(),
        Some("context/research/market_research.md")
    );
    assert!(finished[0].elapsed_seconds.is_some());
    let report = fs::read_to_string(
        runtime
            .crazyagent("project-a")
            .join("context")
            .join("research")
            .join("market_research.md"),
    )
    .unwrap();
    assert!(report.starts_with("# Market research\n"));
}

#[test]
fn a_service_dropped_by_a_panic_mid_run_leaves_no_agent_process() {
    const MID_RUN: &str = "the test fails while the service's research agent runs";
    let runtime = EngineRuntime::with_projects(&[("project-c", "A site for a bike repair shop.")]);
    let project = runtime.folder("project-c");
    let jobs = ResearchJobs::open(&runtime.database).unwrap();
    let id = jobs
        .enqueue("project-c", "market_research", "codex", false)
        .unwrap();
    let (port, _endpoint) = hanging_endpoint();
    let runner = Arc::new(
        ResearchRunner::new(
            &runtime.database,
            &runtime.state,
            &codex(),
            ResearchLimits::default(),
        )
        .unwrap(),
    );
    let marker = runtime.state.join("agents").join("job-");
    let resolver = {
        let project = project.clone();
        move |id: &str, _harness: &str| {
            (id == "project-c").then(|| {
                (
                    project.clone(),
                    AccountPick::Account {
                        connection: CredentialRef::new("hanging-connection").unwrap(),
                        account: Box::new(ResearchAccount::Endpoint {
                            base_url: format!("http://127.0.0.1:{port}/v1"),
                            model: "hanging-model".into(),
                        }),
                    },
                )
            })
        }
    };
    let mut agents = Vec::new();
    let unwound = catch_unwind(AssertUnwindSafe(|| {
        let _service = ResearchService::start(runner, resolver, Duration::from_millis(100));
        wait_for(Duration::from_secs(60), || {
            agents = processes_with(&marker);
            !agents.is_empty()
        });
        panic_any(MID_RUN);
    }));
    let payload = unwound.expect_err("the test must panic mid-run");
    assert_eq!(payload.downcast_ref::<&str>(), Some(&MID_RUN));
    assert!(!agents.is_empty());
    assert_eq!(processes_with(&marker), Vec::<u32>::new());
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(job.status, ResearchStatus::Queued);
    assert_eq!(job.attempts, 0);
}

#[test]
fn stopping_the_service_stops_running_agents_and_requeues_their_jobs() {
    let runtime = EngineRuntime::with_projects(&[("project-b", "A site for a flower shop.")]);
    let project = runtime.folder("project-b");
    let state = runtime.state.clone();
    let database = runtime.database.clone();
    let jobs = ResearchJobs::open(&database).unwrap();
    let id = jobs
        .enqueue("project-b", "design_research", "codex", true)
        .unwrap();
    let listener = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in crazyagent::spawn_gate::incoming_tcp_std(&listener)
            .take(64)
            .flatten()
        {
            held.push(stream);
        }
    });
    let runner = Arc::new(
        ResearchRunner::new(&database, &state, &codex(), ResearchLimits::default()).unwrap(),
    );
    let unknown = jobs
        .enqueue("project-unknown", "market_research", "codex", true)
        .unwrap();
    let resolver = move |id: &str, _harness: &str| {
        (id == "project-b").then(|| {
            (
                project.clone(),
                AccountPick::Account {
                    connection: CredentialRef::new("hanging-connection").unwrap(),
                    account: Box::new(ResearchAccount::Endpoint {
                        base_url: format!("http://127.0.0.1:{port}/v1"),
                        model: "hanging-model".into(),
                    }),
                },
            )
        })
    };
    let service = ResearchService::start(runner, resolver, Duration::from_millis(100));
    wait_for(Duration::from_secs(30), || {
        jobs.get(id).unwrap().unwrap().status == ResearchStatus::Running
    });
    std::thread::sleep(Duration::from_secs(1));
    let stopped = Instant::now();
    service.stop();
    assert!(stopped.elapsed() < Duration::from_secs(10));
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(job.status, ResearchStatus::Queued);
    assert_eq!(job.attempts, 0);
    let rows = progress(&jobs, "project-b").unwrap();
    assert_eq!(rows[0].status, "Queued");
    assert_eq!(rows[0].title, "Design research");
    assert_eq!(
        jobs.get(unknown).unwrap().unwrap().status,
        ResearchStatus::Queued
    );
}

#[test]
fn each_job_waits_on_the_account_for_its_own_harness_and_unsupported_harnesses_fail_plainly() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    let state = root.join("state");
    fs::create_dir_all(project.join("context")).unwrap();
    fs::create_dir_all(&state).unwrap();
    let database = state.join("research.sqlite3");
    let jobs = ResearchJobs::open(&database).unwrap();
    let claude = jobs
        .enqueue("project-c", "design_research", "claude", true)
        .unwrap();
    let rcli = jobs
        .enqueue("project-c", "market_research", "rcli", true)
        .unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let resolver = {
        let asked = asked.clone();
        move |id: &str, harness: &str| {
            asked.lock().unwrap().push(harness.to_owned());
            (id == "project-c").then(|| (project.clone(), AccountPick::NoUsableAccount))
        }
    };
    let runner = Arc::new(
        ResearchRunner::new(&database, &state, &codex(), ResearchLimits::default()).unwrap(),
    );
    let service = ResearchService::start(runner, resolver, Duration::from_millis(100));
    wait_for(Duration::from_secs(10), || {
        jobs.get(rcli).unwrap().unwrap().status == ResearchStatus::Failed
            && jobs.get(claude).unwrap().unwrap().status == ResearchStatus::NeedsPool
    });
    service.stop();
    assert_eq!(
        jobs.get(rcli).unwrap().unwrap().error.as_deref(),
        Some("This harness cannot run research yet")
    );
    let rows = progress(&jobs, "project-c").unwrap();
    assert_eq!(rows[0].status, "Needs a pool");
    assert_eq!(
        rows[0].detail.as_deref(),
        Some("No account in this project's pool can run this research")
    );
    let asked = asked.lock().unwrap();
    assert!(asked.iter().any(|harness| harness == "claude"));
    assert!(asked.iter().any(|harness| harness == "rcli"));
}

#[test]
fn research_waits_with_the_rate_limit_code_and_its_end_while_every_pool_account_is_paused() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    let state = root.join("state");
    fs::create_dir_all(project.join("context")).unwrap();
    fs::create_dir_all(&state).unwrap();
    let database = state.join("research.sqlite3");
    let jobs = ResearchJobs::open(&database).unwrap();
    let design = jobs
        .enqueue("project-p", "design_research", "claude", true)
        .unwrap();
    let pick = Arc::new(Mutex::new(AccountPick::Paused {
        until_ms: 1_792_729_920_000,
    }));
    let resolver = {
        let pick = pick.clone();
        move |id: &str, _harness: &str| {
            (id == "project-p").then(|| (project.clone(), pick.lock().unwrap().clone()))
        }
    };
    let runner = Arc::new(
        ResearchRunner::new(&database, &state, &codex(), ResearchLimits::default()).unwrap(),
    );
    let service = ResearchService::start(runner, resolver, Duration::from_millis(100));
    wait_for(Duration::from_secs(10), || {
        jobs.get(design).unwrap().unwrap().status == ResearchStatus::NeedsPool
    });
    let rows = progress(&jobs, "project-p").unwrap();
    assert_eq!(
        rows[0].detail.as_deref(),
        Some(
            "rate_limited until 2026-10-23 04:32 UTC: Every account in this project's pool that can run this research hit its rate limit"
        )
    );
    assert_eq!(jobs.get(design).unwrap().unwrap().attempts, 0);
    *pick.lock().unwrap() = AccountPick::Account {
        connection: CredentialRef::new("anthropic-local-connection").unwrap(),
        account: Box::new(ResearchAccount::AnthropicEndpoint {
            base_url: "http://127.0.0.1:11434".into(),
            model: "qwen2.5:0.5b".into(),
        }),
    };
    wait_for(Duration::from_secs(10), || {
        jobs.get(design).unwrap().unwrap().status == ResearchStatus::Failed
    });
    service.stop();
    assert_eq!(
        jobs.get(design).unwrap().unwrap().error.as_deref(),
        Some("This harness is not installed on this computer")
    );
}
