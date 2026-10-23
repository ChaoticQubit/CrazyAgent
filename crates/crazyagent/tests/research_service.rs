use crazyagent::protocol::CredentialRef;
use crazyagent::research::accounts::AccountPick;
use crazyagent::research::jobs::{ResearchJobs, ResearchStatus};
use crazyagent::research::runner::{ResearchAccount, ResearchLimits, ResearchRunner};
use crazyagent::research::service::{ResearchService, progress};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

fn ollama() -> AccountPick {
    AccountPick::Account {
        connection: CredentialRef::new("local-connection").unwrap(),
        account: ResearchAccount::Endpoint {
            base_url: "http://127.0.0.1:11434/v1".into(),
            model: "qwen2.5:0.5b".into(),
        },
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
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    let state = root.join("state");
    fs::create_dir_all(project.join("context")).unwrap();
    fs::create_dir_all(&state).unwrap();
    fs::write(
        project.join("context").join("brief.md"),
        "# Bakery ordering\n\nA website where a small bakery takes cake orders.\n",
    )
    .unwrap();
    let database = state.join("research.sqlite3");
    let jobs = ResearchJobs::open(&database).unwrap();
    let market = jobs
        .enqueue("project-a", "market_research", "codex", true)
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
        move |id: &str| {
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
        project
            .join("context")
            .join("research")
            .join("market_research.md"),
    )
    .unwrap();
    assert!(report.starts_with("# Market research\n"));
}

#[test]
fn stopping_the_service_stops_running_agents_and_requeues_their_jobs() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    let state = root.join("state");
    fs::create_dir_all(project.join("context")).unwrap();
    fs::create_dir_all(&state).unwrap();
    let database = state.join("research.sqlite3");
    let jobs = ResearchJobs::open(&database).unwrap();
    let id = jobs
        .enqueue("project-b", "design_research", "codex", true)
        .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().take(64).flatten() {
            held.push(stream);
        }
    });
    let runner = Arc::new(
        ResearchRunner::new(&database, &state, &codex(), ResearchLimits::default()).unwrap(),
    );
    let unknown = jobs
        .enqueue("project-unknown", "market_research", "codex", true)
        .unwrap();
    let resolver = move |id: &str| {
        (id == "project-b").then(|| {
            (
                project.clone(),
                AccountPick::Account {
                    connection: CredentialRef::new("hanging-connection").unwrap(),
                    account: ResearchAccount::Endpoint {
                        base_url: format!("http://127.0.0.1:{port}/v1"),
                        model: "hanging-model".into(),
                    },
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
