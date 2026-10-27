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
    let report = crate::usage::report::report(&db, None, None, 0, NOW + 2).unwrap();
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
