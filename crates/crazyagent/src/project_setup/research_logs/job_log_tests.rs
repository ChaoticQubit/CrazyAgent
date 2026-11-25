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
