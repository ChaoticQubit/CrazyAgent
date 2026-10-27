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
            connection TEXT, status TEXT, stopped_ms INTEGER);
         INSERT INTO team_agents VALUES (1,'alpha','master','claude-code','claude-a','idle',NULL);
         INSERT INTO team_agents VALUES (2,'alpha','sub_agent','claude-code',NULL,'working',NULL);
         INSERT INTO team_agents VALUES (3,'beta','master','claude-code','claude-a','stopped',0);",
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
fn a_step_that_panics_ends_only_that_step_and_the_next_step_still_counts() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let team = root.join("team");
    let db = crate::usage::open(&root).unwrap();
    db.execute_batch(
        "CREATE TABLE team_agents (id INTEGER PRIMARY KEY, project TEXT, role TEXT, harness TEXT,
            connection TEXT, status TEXT, stopped_ms INTEGER);
         INSERT INTO team_agents VALUES (4,'alpha','sub_agent','claude-code',NULL,'working',NULL);
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
