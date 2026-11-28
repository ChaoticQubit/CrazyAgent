use super::*;

#[test]
fn a_run_attempt_outside_the_job_attempt_bound_is_refused() {
    let root = engine_root();
    let runner = ResearchRunner::new(
        &root.database,
        &root.state,
        Path::new("/usr/bin/true"),
        ResearchLimits::default(),
    )
    .unwrap();
    let jobs = ResearchJobs::open(&root.database).unwrap();
    let id = jobs
        .enqueue("alpha", "market_research", "codex", false)
        .unwrap();
    let mut job = jobs.claim("alpha", "attempts-row", 1).unwrap().unwrap();
    assert_eq!(job.id, id);
    for (attempts, named) in [
        (1, Some(format!("job-{id}-1"))),
        (10, Some(format!("job-{id}-10"))),
        (0, None),
        (11, None),
    ] {
        job.attempts = attempts;
        match runner.free_run(&root.base, &job) {
            Ok((name, _)) => assert_eq!(Some(name), named, "attempt {attempts}"),
            Err(RunError::Invalid(_)) => assert_eq!(named, None, "attempt {attempts}"),
            Err(other) => panic!("attempt {attempts}: {other:?}"),
        }
    }
}

/// After the owner's Retry of a job whose second attempt failed, the job's log shows the run
/// that started after the Retry, not the earlier run with the higher attempt number: the
/// Retry starts the attempts over, so the new run's folder name sorts below the old one.
#[test]
#[cfg(target_os = "macos")]
fn the_log_after_a_retry_shows_the_new_run_not_the_older_higher_attempt() {
    let root = engine_root();
    let folder = root.base.join("repo");
    fs::create_dir(&folder).unwrap();
    repository_git(&folder, &["init", "-q"]);
    repository_git(&folder, &["commit", "-q", "--allow-empty", "-m", "Start"]);
    register_project(&root.database, "alpha", &folder);
    let codex = root.base.join("bin").join("codex");
    fs::create_dir_all(codex.parent().unwrap()).unwrap();
    let say = |line: &str| {
        fs::write(&codex, format!("#!/bin/sh\necho '{line}' >&2\nexit 1\n")).unwrap();
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
    };
    let pinned = codex.clone();
    let runner = ResearchRunner::without_codex(
        &root.database,
        &root.state,
        ResearchLimits {
            wall_time: Duration::from_secs(20),
            ..ResearchLimits::default()
        },
    )
    .unwrap()
    .with_harness_lookup(Arc::new(move |harness: &str| {
        (harness == "codex").then(|| pinned.clone())
    }));
    let listening = kernel_processes::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let project = ResearchProject {
        id: "alpha",
        root: &folder,
        account: Some(ResearchAccount::Endpoint {
            base_url: format!(
                "http://127.0.0.1:{}/v1",
                listening.local_addr().unwrap().port()
            ),
            model: "local-model".into(),
            key: None,
        }),
    };
    let jobs = ResearchJobs::open(&root.database).unwrap();
    let id = jobs
        .enqueue("alpha", "market_research", "codex", false)
        .unwrap();
    let log = || {
        let mut connection = rusqlite::Connection::open(&root.database).unwrap();
        let tx = connection.transaction().unwrap();
        crate::project_setup::research_job_log(&tx, &root.database, id).unwrap()
    };
    for (line, ended) in [
        ("first attempt", ResearchStatus::Queued),
        ("second attempt", ResearchStatus::Failed),
    ] {
        say(line);
        let run = runner.run_once(&project).unwrap().unwrap();
        assert_eq!((run.job, run.status), (id, ended), "{line}");
    }
    assert_eq!(log(), ["second attempt"]);
    let connection = rusqlite::Connection::open(&root.database).unwrap();
    let retried = connection
        .execute(
            &format!(
                "UPDATE research_jobs SET status='queued', attempts=0, claimer=NULL, error=NULL,
                cancel_requested=0, output_bytes=0, started_ms=NULL, heartbeat_ms=NULL,
                finished_ms=NULL, {} WHERE id=?1 AND status IN ('failed','cancelled')",
                crate::research::jobs::RUN_AGAIN
            ),
            [id],
        )
        .unwrap();
    assert_eq!(retried, 1);
    say("after the retry");
    let run = runner.run_once(&project).unwrap().unwrap();
    assert_eq!((run.job, run.status), (id, ResearchStatus::Queued));
    for name in [format!("job-{id}-2"), format!("job-{id}-1-1")] {
        assert!(root.state.join("agents").join(&name).is_dir(), "{name}");
    }
    assert_eq!(log(), ["after the retry"]);
    drop(listening);
}

/// A job handed back before its run folder is made, here a managed account that is not
/// ready, keeps the run name its record had: its log still shows the earlier run that
/// exists, not a folder that was never made.
#[test]
#[cfg(target_os = "macos")]
fn a_job_refused_before_its_run_folder_is_made_keeps_the_log_of_its_earlier_run() {
    let root = engine_root();
    let folder = root.base.join("repo");
    fs::create_dir(&folder).unwrap();
    repository_git(&folder, &["init", "-q"]);
    repository_git(&folder, &["commit", "-q", "--allow-empty", "-m", "Start"]);
    register_project(&root.database, "alpha", &folder);
    let codex = root.base.join("bin").join("codex");
    fs::create_dir_all(codex.parent().unwrap()).unwrap();
    fs::write(&codex, "#!/bin/sh\necho 'first attempt' >&2\nexit 1\n").unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
    let pinned = codex.clone();
    let runner = ResearchRunner::without_codex(
        &root.database,
        &root.state,
        ResearchLimits {
            wall_time: Duration::from_secs(20),
            ..ResearchLimits::default()
        },
    )
    .unwrap()
    .with_harness_lookup(Arc::new(move |harness: &str| {
        (harness == "codex").then(|| pinned.clone())
    }));
    let listening = kernel_processes::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let endpoint = ResearchProject {
        id: "alpha",
        root: &folder,
        account: Some(ResearchAccount::Endpoint {
            base_url: format!(
                "http://127.0.0.1:{}/v1",
                listening.local_addr().unwrap().port()
            ),
            model: "local-model".into(),
            key: None,
        }),
    };
    let jobs = ResearchJobs::open(&root.database).unwrap();
    let id = jobs
        .enqueue("alpha", "market_research", "codex", false)
        .unwrap();
    let log = || {
        let mut connection = rusqlite::Connection::open(&root.database).unwrap();
        let tx = connection.transaction().unwrap();
        crate::project_setup::research_job_log(&tx, &root.database, id).unwrap()
    };
    let run_name = || -> String {
        rusqlite::Connection::open(&root.database)
            .unwrap()
            .query_row(
                "SELECT run_name FROM research_jobs WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap()
    };
    let run = runner.run_once(&endpoint).unwrap().unwrap();
    assert_eq!((run.job, run.status), (id, ResearchStatus::Queued));
    assert_eq!(run_name(), format!("job-{id}-1"));
    assert_eq!(log(), ["first attempt"]);
    let managed = ResearchProject {
        id: "alpha",
        root: &folder,
        account: Some(ResearchAccount::ManagedCodex {
            connection: codex_connection(),
            model: None,
        }),
    };
    let run = runner.run_once(&managed).unwrap().unwrap();
    assert_eq!((run.job, run.status), (id, ResearchStatus::Queued));
    assert_eq!(run.process_group, None);
    assert!(
        !root
            .state
            .join("agents")
            .join(format!("job-{id}-2"))
            .exists()
    );
    assert_eq!(run_name(), format!("job-{id}-1"));
    assert_eq!(log(), ["first attempt"]);
    drop(listening);
}
