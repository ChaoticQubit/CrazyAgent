use super::*;

const OLD: &str = "CREATE TABLE usage_tokens (
    hour_ms INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('master','agent','research')),
    project TEXT NOT NULL,
    pool TEXT NOT NULL,
    account TEXT NOT NULL,
    agent TEXT NOT NULL,
    harness TEXT NOT NULL,
    session TEXT NOT NULL,
    input INTEGER NOT NULL,
    output INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    cache_write INTEGER NOT NULL,
    PRIMARY KEY (hour_ms, kind, project, pool, account, agent, harness, session)
);
CREATE TABLE usage_files (path TEXT PRIMARY KEY, progress TEXT NOT NULL);";

fn old_store() -> (tempfile::TempDir, Connection) {
    let root = tempfile::tempdir().unwrap();
    let db = Connection::open(root.path().join("engine.db")).unwrap();
    db.execute_batch(OLD).unwrap();
    (root, db)
}

fn insert(db: &Connection, harness: &str, session: &str, input: i64, read: i64) {
    db.execute(
        "INSERT INTO usage_tokens VALUES (3600000,'agent','alpha','','acct','team-7',?1,?2,?3,1,?4,2)",
        params![harness, session, input, read],
    )
    .unwrap();
}

fn migrate(db: &Connection) {
    let tx = db.unchecked_transaction().unwrap();
    migrate_counts(&tx).unwrap();
    tx.commit().unwrap();
}

fn rows(db: &Connection) -> Vec<(String, String, String, [i64; 5])> {
    db.prepare(
        "SELECT harness,session,model,input,output,cache_read,cache_write,cost_nanos
         FROM usage_tokens ORDER BY harness,session",
    )
    .unwrap()
    .query_map([], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            [
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
            ],
        ))
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

#[test]
fn step_merges_harness_spellings_and_keeps_every_total() {
    let (_root, db) = old_store();
    insert(&db, "claude", "s1", 10, 100);
    insert(&db, "claude-code", "s1", 5, 50);
    insert(&db, "claude", "s3", 3, 30);
    insert(&db, "codex", "s2", 7, 70);
    migrate(&db);
    assert_eq!(
        rows(&db),
        vec![
            (
                "claude-code".to_owned(),
                "s1".to_owned(),
                String::new(),
                [15, 2, 150, 4, 0]
            ),
            (
                "claude-code".to_owned(),
                "s3".to_owned(),
                String::new(),
                [3, 1, 30, 2, 0]
            ),
            (
                "codex".to_owned(),
                "s2".to_owned(),
                String::new(),
                [7, 1, 70, 2, 0]
            ),
        ]
    );
}

#[test]
fn step_seeds_the_message_in_flight() {
    let (_root, db) = old_store();
    db.execute(
        "INSERT INTO usage_files VALUES ('/h/a.jsonl',?1)",
        [r#"{"identity":[1,2],"offset":9,"skipping":false,"message":"msg_a","counted":{"input":100,"output":10,"cache_read":0,"cache_write":0},"total":{"input":0,"output":0,"cache_read":0,"cache_write":0}}"#],
    )
    .unwrap();
    migrate(&db);
    let seeded: (i64, i64) = db
        .query_row(
            "SELECT input,output FROM usage_messages WHERE id='claude:msg_a'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(seeded, (100, 10));
    let progress: String = db
        .query_row("SELECT progress FROM usage_files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Progress>(&progress).unwrap().offset,
        9
    );
}

#[test]
fn step_on_the_new_shape_changes_nothing() {
    let (_root, db) = store_new();
    insert_new(&db);
    let before = rows(&db);
    migrate(&db);
    assert_eq!(rows(&db), before);
}

#[test]
fn a_seeded_message_adds_only_its_growth_on_its_next_line() {
    let (root, db) = old_store();
    let home = root.path().join("home");
    let file = home.join("projects/-repo/s1.jsonl");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let line = |output: u64| {
        format!(
            "{}\n",
            serde_json::json!({"type": "assistant", "message": {"id": "msg_a",
                "usage": {"input_tokens": 100, "output_tokens": output}}})
        )
    };
    let first = line(10);
    std::fs::write(&file, &first).unwrap();
    let (device, inode) = file_identity(&std::fs::metadata(&file).unwrap());
    let progress = serde_json::json!({
        "identity": [device, inode], "offset": first.len(), "skipping": false, "message": "msg_a",
        "counted": {"input": 100, "output": 10, "cache_read": 0, "cache_write": 0},
        "total": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0},
    });
    db.execute(
        "INSERT INTO usage_files VALUES (?1,?2)",
        params![file.to_string_lossy(), progress.to_string()],
    )
    .unwrap();
    migrate(&db);
    std::fs::write(&file, format!("{first}{}", line(40))).unwrap();
    let who = Attribution {
        kind: RunKind::Agent,
        project: Some(ProjectId::new("alpha").unwrap()),
        pool: None,
        account: Some(CredentialRef::new("acct").unwrap()),
        agent: "team-7".into(),
        harness: "claude-code".into(),
    };
    assert_eq!(
        scan_home(&db, &home, Format::Claude, &who, 1_793_000_000_000).unwrap(),
        Tokens {
            output: 30,
            ..Tokens::default()
        }
    );
}

#[test]
fn a_progress_row_the_step_cannot_read_stops_nothing() {
    let (_root, db) = old_store();
    let total = r#""total":{"input":0,"output":0,"cache_read":0,"cache_write":0}"#;
    let head = r#""identity":[1,2],"offset":9,"skipping":false"#;
    for (path, progress) in [
        (
            "/h/good.jsonl",
            format!(
                r#"{{{head},"message":"msg_a","counted":{{"input":100,"output":10,"cache_read":0,"cache_write":0}},{total}}}"#
            ),
        ),
        ("/h/not-json.jsonl", "not json".to_owned()),
        (
            "/h/no-counted.jsonl",
            format!(r#"{{{head},"message":"msg_b",{total}}}"#),
        ),
        (
            "/h/part.jsonl",
            format!(r#"{{{head},"message":"msg_c","counted":{{"input":1}},{total}}}"#),
        ),
    ] {
        db.execute(
            "INSERT INTO usage_files VALUES (?1,?2)",
            params![path, progress],
        )
        .unwrap();
    }
    let tx = db.unchecked_transaction().unwrap();
    let done = migrate_counts(&tx);
    assert!(done.is_ok(), "{done:?}");
    tx.commit().unwrap();
    let seeded: Vec<String> = db
        .prepare("SELECT id FROM usage_messages ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(seeded, ["claude:msg_a"]);
    let stored = |path: &str| -> String {
        db.query_row(
            "SELECT progress FROM usage_files WHERE path=?1",
            [path],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(stored("/h/not-json.jsonl"), "not json");
    for path in ["/h/good.jsonl", "/h/no-counted.jsonl", "/h/part.jsonl"] {
        assert_eq!(
            serde_json::from_str::<Progress>(&stored(path))
                .unwrap()
                .offset,
            9,
            "{path} is readable after the step"
        );
    }
}

#[test]
fn step_seeds_only_an_id_a_live_scan_could_key() {
    let (_root, db) = old_store();
    let longest = "a".repeat(128);
    let ids = [
        longest.clone(),
        "b".repeat(129),
        "\u{e9}".repeat(65),
        String::new(),
    ];
    for (index, id) in ids.iter().enumerate() {
        let progress = serde_json::json!({
            "identity": [1, 2], "offset": 9, "skipping": false, "message": id,
            "counted": {"input": 1, "output": 1, "cache_read": 0, "cache_write": 0},
            "total": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0},
        });
        db.execute(
            "INSERT INTO usage_files VALUES (?1,?2)",
            params![format!("/h/{index}.jsonl"), progress.to_string()],
        )
        .unwrap();
    }
    migrate(&db);
    let seeded: Vec<String> = db
        .prepare("SELECT id FROM usage_messages")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(seeded, [format!("claude:{longest}")]);
    let kept: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM usage_files WHERE json_type(progress,'$.message') IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kept, 0, "no stored row keeps its message");
}

fn store_new() -> (tempfile::TempDir, Connection) {
    let root = tempfile::tempdir().unwrap();
    let db = super::super::open(root.path()).unwrap();
    (root, db)
}

fn insert_new(db: &Connection) {
    db.execute(
        "INSERT INTO usage_tokens(hour_ms,kind,project,pool,account,agent,harness,session,model,input,output,cache_read,cache_write,cost_nanos)
         VALUES (3600000,'agent','alpha','','acct','team-7','codex','s','gpt-5',1,1,1,1,9)",
        [],
    )
    .unwrap();
}
