use super::*;

/// A runtime folder with an engine database holding job 42, whose stored run name is the
/// one given, and the run folders named, each with one log line of its own name. Inputs:
/// the stored run name and the run folder names. Output: the folder, removed when dropped.
fn job_with(stored: Option<&str>, runs: &[&str]) -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("research-job-log-")
        .tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap())
        .unwrap();
    let agents = root.path().join("research/agents");
    std::fs::create_dir_all(&agents).unwrap();
    for run in runs {
        std::fs::create_dir(agents.join(run)).unwrap();
        std::fs::write(agents.join(run).join("stderr.log"), format!("{run}\n")).unwrap();
    }
    let connection = Connection::open(root.path().join("engine.db")).unwrap();
    crate::project_setup::initialize(&connection).unwrap();
    connection
        .execute(
            "INSERT INTO research_jobs (id, project, kind, harness, tools, status, created_ms,
             run_name) VALUES (42, 'alpha', 'market_research', 'codex', 0, 'failed', 0, ?1)",
            [stored],
        )
        .unwrap();
    root
}

/// Reads job 42's log the way the owner's read does, in one transaction. Input: the runtime
/// folder. Output: the lines.
fn read(root: &tempfile::TempDir) -> Vec<String> {
    let database = root.path().join("engine.db");
    let mut connection = Connection::open(&database).unwrap();
    let tx = connection.transaction().unwrap();
    research_job_log(&tx, &database, 42).unwrap()
}

/// A stored run name is read as it is, even when an older run of a higher attempt is there.
#[test]
fn the_stored_run_is_read_and_not_a_higher_attempt() {
    let root = job_with(Some("job-42-1-1"), &["job-42-1", "job-42-1-1", "job-42-2"]);
    assert_eq!(read(&root), ["job-42-1-1"]);
}

/// The empty name a start over stores reads no lines, though earlier runs are there.
#[test]
fn an_empty_stored_name_reads_nothing() {
    let root = job_with(Some(""), &["job-42-1", "job-42-2-3"]);
    assert_eq!(read(&root), Vec::<String>::new());
}

/// A record from before the engine kept the name takes the legacy read by folder name,
/// which finds the newest attempt and then its newest launch.
#[test]
fn no_stored_name_finds_the_newest_run_by_folder_name() {
    let root = job_with(None, &["job-42-1-5", "job-42-2", "job-42-2-3", "job-4-9"]);
    assert_eq!(read(&root), ["job-42-2-3"]);
}

/// A stored name the runner never gives this job reads no lines, even where an entry of that
/// name or a run of the job is there: another job's run, a parent folder, a path, an attempt
/// or launch past its bound, a zero, a sign or a leading zero.
#[test]
fn a_stored_name_of_any_other_shape_reads_nothing() {
    for stored in [
        "job-8-1",
        "..",
        "job-42-1/../job-42-2",
        "job-42-11",
        "job-42-0",
        "job-42-1-0",
        "job-42-1-100",
        "job-42-+1",
        "job-42-01",
        "job-42-1-2-3",
        "job-42-",
        "job-42",
    ] {
        let root = job_with(Some(stored), &["job-8-1", "job-42-1", "job-42-2"]);
        assert_eq!(read(&root), Vec::<String>::new(), "{stored}");
    }
}

/// A setup save that queues research again blanks the run name of a stopped job, whose
/// attempts start over, and keeps the run name of a job still waiting for its next attempt,
/// so that job's log keeps showing the run it last placed.
#[test]
fn a_setup_save_blanks_the_run_name_of_a_stopped_job_and_keeps_a_waiting_one() {
    let root = tempfile::tempdir().unwrap();
    let connection = Connection::open(root.path().join("jobs.db")).unwrap();
    crate::project_setup::initialize(&connection).unwrap();
    let kinds = [
        ResearchKind::MarketResearch,
        ResearchKind::CompetitorResearch,
    ];
    let team = TeamPlan {
        master: "codex".into(),
        master_tools: true,
        sub_harnesses: vec![],
        min_agents: 1,
        max_agents: 1,
    };
    let project = ProjectId::new("assigned-project").unwrap();
    let tx = connection.unchecked_transaction().unwrap();
    insert_jobs(&tx, &project, &kinds, &team, true, 1000).unwrap();
    tx.commit().unwrap();
    let ids: Vec<i64> = connection
        .prepare("SELECT id FROM research_jobs ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(ids.len(), 2);
    let (waiting, stopped) = (ids[0], ids[1]);
    let place = |id: i64, status: &str, attempts: u32| {
        connection
            .execute(
                "UPDATE research_jobs SET status=?2, attempts=?3, run_name=?4 WHERE id=?1",
                params![id, status, attempts, format!("job-{id}-{attempts}")],
            )
            .unwrap();
    };
    place(waiting, "queued", 1);
    place(stopped, "failed", 2);
    let tx = connection.unchecked_transaction().unwrap();
    replan_jobs(&tx, &project, &[], &kinds, &team, true).unwrap();
    tx.commit().unwrap();
    let kept = |id: i64| -> (String, u32, Option<String>) {
        connection
            .query_row(
                "SELECT status, attempts, run_name FROM research_jobs WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    };
    assert_eq!(
        kept(waiting),
        ("queued".into(), 1, Some(format!("job-{waiting}-1")))
    );
    assert_eq!(kept(stopped), ("queued".into(), 0, Some(String::new())));
}
