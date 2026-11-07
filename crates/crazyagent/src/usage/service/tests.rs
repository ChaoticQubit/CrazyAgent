use super::*;
use std::io::Write;

const NOW: i64 = 1_793_000_000_000;

fn assistant(id: &str, input: u64) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "type": "assistant",
            "message": {"id": id, "usage": {
                "input_tokens": input, "output_tokens": 1,
                "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0
            }}
        })
    )
}

fn append(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}

#[test]
fn the_master_counts_as_its_projects_master_and_an_agent_without_an_account_is_unattributed() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let database = root.join("engine.db");
    let team = root.join("team");
    let db = crate::usage::open(&root).unwrap();
    db.execute_batch(
        "CREATE TABLE team_agents (id INTEGER PRIMARY KEY, project TEXT, role TEXT, harness TEXT,
            connection TEXT, status TEXT, stopped_ms INTEGER, hookless INTEGER NOT NULL DEFAULT 0);
         INSERT INTO team_agents VALUES (1,'alpha','master','claude-code','claude-a','idle',NULL,0);
         INSERT INTO team_agents VALUES (2,'alpha','sub_agent','claude-code',NULL,'working',NULL,0);
         INSERT INTO team_agents VALUES (3,'beta','master','claude-code','claude-a','stopped',0,0);",
    )
    .unwrap();
    for (id, input) in [(1, 50), (2, 7), (3, 1000)] {
        append(
            &team.join(format!("agents/agent-{id}/home/projects/f/s{id}.jsonl")),
            &assistant(&format!("m{id}"), input),
        );
    }
    let mut usage = Usage::open(&database, &team, Box::new(|| None), BTreeMap::new()).unwrap();
    usage.step(NOW).unwrap();
    let rows: Vec<(String, String, String, String, i64)> = db
        .prepare(
            "SELECT kind,project,account,agent,SUM(input) FROM usage_tokens
             GROUP BY kind,project,account,agent ORDER BY agent",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            (
                "master".into(),
                "alpha".into(),
                "claude-a".into(),
                "master-alpha".into(),
                50
            ),
            (
                "agent".into(),
                "alpha".into(),
                String::new(),
                "team-2".into(),
                7
            ),
        ]
    );
}

#[test]
fn a_live_agent_on_a_harness_without_a_reader_is_marked_not_reported() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let database = root.join("engine.db");
    let team = root.join("team");
    let db = crate::usage::open(&root).unwrap();
    db.execute_batch(
        "CREATE TABLE team_agents (id INTEGER PRIMARY KEY, project TEXT, role TEXT, harness TEXT,
            connection TEXT, status TEXT, stopped_ms INTEGER);
         INSERT INTO team_agents VALUES (1,'alpha','master','claude-code','claude-a','idle',NULL);
         INSERT INTO team_agents VALUES (2,'alpha','sub_agent','opencode',NULL,'working',NULL);
         INSERT INTO team_agents VALUES (3,'beta','sub_agent','gemini-cli',NULL,'stopped',0);",
    )
    .unwrap();
    append(
        &team.join("agents/agent-1/home/projects/f/s1.jsonl"),
        &assistant("m1", 50),
    );
    let mut usage = Usage::open(&database, &team, Box::new(|| None), BTreeMap::new()).unwrap();
    usage.step(NOW).unwrap();
    usage.step(NOW + 1).unwrap();
    let report = crate::usage::report::report(&db, None, None, 0, NOW + 2, &[]).unwrap();
    assert_eq!(report.total.input, 50);
    assert_eq!(report.not_reported_by, ["OpenCode"]);
    assert_eq!(
        report
            .agents
            .iter()
            .map(|row| (row.key.as_str(), row.not_reported_by.as_deref()))
            .collect::<Vec<_>>(),
        [("master-alpha", None), ("team-2", Some("OpenCode"))],
        "a long-stopped agent is not scanned, so it is not marked"
    );
    let marks: i64 = db
        .query_row("SELECT COUNT(*) FROM usage_unreported", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(marks, 1, "two steps in one hour keep one mark");
}

#[test]
fn a_step_that_panics_ends_only_that_step_and_the_next_step_still_counts() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let team = root.join("team");
    let db = crate::usage::open(&root).unwrap();
    db.execute_batch(
        "CREATE TABLE team_agents (id INTEGER PRIMARY KEY, project TEXT, role TEXT, harness TEXT,
            connection TEXT, status TEXT, stopped_ms INTEGER, hookless INTEGER NOT NULL DEFAULT 0);
         INSERT INTO team_agents VALUES (4,'alpha','sub_agent','claude-code',NULL,'working',NULL,0);
         CREATE TABLE held (n INTEGER);",
    )
    .unwrap();
    append(
        &team.join("agents/agent-4/home/projects/f/s4.jsonl"),
        &assistant("m4", 9),
    );
    let mut usage = Usage::open(
        &root.join("engine.db"),
        &team,
        Box::new(|| None),
        BTreeMap::new(),
    )
    .unwrap();
    let panicked = guarded(|| {
        let tx = usage.db.unchecked_transaction().map_err(storage)?;
        tx.execute("INSERT INTO held (n) VALUES (1)", [])
            .map_err(storage)?;
        panic!("a step broke an invariant");
    });
    assert_eq!(panicked.as_deref(), Some("usage_step_panicked"));
    let held: i64 = db
        .query_row("SELECT COUNT(*) FROM held", [], |row| row.get(0))
        .unwrap();
    assert_eq!(held, 0);
    assert_eq!(guarded(|| usage.step(NOW)), None);
    let counted: i64 = db
        .query_row(
            "SELECT SUM(input) FROM usage_tokens WHERE agent='team-4'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(counted, 9);
    assert_eq!(
        guarded(|| Err(failure("usage_unavailable"))).as_deref(),
        Some("usage_unavailable")
    );
}

#[test]
fn a_step_that_keeps_panicking_is_logged_once_and_waits_longer_each_time() {
    let mut pace = Pace::default();
    let panicked = || Some(STEP_PANICKED.to_owned());
    let unavailable = || Some("usage_unavailable".to_owned());
    assert_eq!(pace.after(None), (Vec::new(), SCAN_EVERY));
    assert_eq!(
        pace.after(guarded(|| panic!("a step broke an invariant"))),
        (
            vec!["usage: usage_step_panicked".to_owned()],
            SCAN_EVERY * 2
        )
    );
    let pauses: Vec<Duration> = (0..10)
        .map(|_| {
            let (lines, pause) = pace.after(panicked());
            assert!(lines.is_empty(), "{lines:?}");
            pause
        })
        .collect();
    assert_eq!(
        pauses[..3],
        [SCAN_EVERY * 4, SCAN_EVERY * 8, SCAN_EVERY * 16]
    );
    assert!(pauses.windows(2).all(|pair| pair[0] <= pair[1]));
    assert_eq!(pauses.last(), Some(&PANIC_PAUSE_MAX));
    assert_eq!(
        pace.after(None),
        (
            vec!["usage: 11 steps in a row panicked before this one".to_owned()],
            SCAN_EVERY
        )
    );

    assert_eq!(
        pace.after(unavailable()),
        (vec!["usage: usage_unavailable".to_owned()], SCAN_EVERY)
    );
    assert_eq!(pace.after(unavailable()), (Vec::new(), SCAN_EVERY));
    assert_eq!(
        pace.after(panicked()),
        (
            vec!["usage: usage_step_panicked".to_owned()],
            SCAN_EVERY * 2
        )
    );
    assert_eq!(
        pace.after(unavailable()),
        (vec!["usage: usage_unavailable".to_owned()], SCAN_EVERY),
        "one panic needs no count line"
    );
    for _ in 0..u32::BITS + 2 {
        pace.after(panicked());
    }
    assert_eq!(pace.after(panicked()).1, PANIC_PAUSE_MAX);
}

#[test]
fn a_panicking_step_kind_is_counted_marked_working_again_and_forgotten_on_restart() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let database = root.join("engine.db");
    let team = root.join("team");
    let db = crate::usage::open(&root).unwrap();
    db.execute_batch(
        "CREATE TABLE team_agents (id INTEGER PRIMARY KEY, project TEXT, role TEXT, harness TEXT,
            connection TEXT, status TEXT, stopped_ms INTEGER);",
    )
    .unwrap();
    let mut usage = Usage::open(&database, &team, Box::new(|| None), BTreeMap::new()).unwrap();
    for at in [NOW, NOW + 5_000] {
        let problem = counted_step(
            &mut usage,
            |usage| {
                usage.doing = UsageStep::Team;
                panic!("a team scan broke an invariant");
            },
            at,
        );
        assert_eq!(problem.as_deref(), Some("usage_step_panicked"));
    }
    assert_eq!(
        counted_step(&mut usage, |usage| usage.step(NOW + 10_000), NOW + 10_000),
        None
    );
    assert_eq!(
        usage.doing,
        UsageStep::Runs,
        "a whole step ends in its last part"
    );
    let report = crate::usage::report::report(&db, None, None, 0, NOW + 11_000, &[]).unwrap();
    assert_eq!(
        report.health,
        [crate::usage::health::StepHealth {
            step: UsageStep::Team,
            panics: 2,
            last_ms: NOW + 5_000,
            recovered_ms: Some(NOW + 10_000),
        }],
        "a later step that completes the kind marks it working again and keeps the count"
    );
    assert_eq!(
        counted_step(&mut usage, |usage| usage.step(NOW + 15_000), NOW + 15_000),
        None
    );
    assert_eq!(
        crate::usage::health::health(&db).unwrap()[0].recovered_ms,
        Some(NOW + 10_000),
        "the first time it worked again is kept"
    );
    let problem = counted_step(
        &mut usage,
        |usage| {
            usage.doing = UsageStep::Team;
            panic!("the team scan broke again");
        },
        NOW + 20_000,
    );
    assert_eq!(problem.as_deref(), Some("usage_step_panicked"));
    assert_eq!(
        crate::usage::health::health(&db).unwrap(),
        [crate::usage::health::StepHealth {
            step: UsageStep::Team,
            panics: 3,
            last_ms: NOW + 20_000,
            recovered_ms: None,
        }],
        "a new panic makes the kind failing again"
    );
    assert_eq!(
        counted_step(&mut usage, |usage| usage.step(NOW + 25_000), NOW + 25_000),
        None
    );
    assert_eq!(
        crate::usage::health::health(&db).unwrap()[0].recovered_ms,
        Some(NOW + 25_000)
    );
    drop(usage);
    Usage::open(&database, &team, Box::new(|| None), BTreeMap::new()).unwrap();
    assert!(
        crate::usage::health::health(&db).unwrap().is_empty(),
        "a new engine run starts with no counted panics"
    );
}
