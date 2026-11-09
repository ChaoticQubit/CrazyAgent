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

fn rows(db: &Connection) -> Vec<(String, String, i64, i64, i64)> {
    db.prepare(
        "SELECT harness,model,input,cache_read,cost_nanos FROM usage_tokens ORDER BY harness",
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
    .unwrap()
}

#[test]
fn step_merges_harness_spellings_and_keeps_every_total() {
    let (_root, db) = old_store();
    insert(&db, "claude", "s1", 10, 100);
    insert(&db, "claude-code", "s1", 5, 50);
    insert(&db, "codex", "s2", 7, 70);
    migrate(&db);
    assert_eq!(
        rows(&db),
        vec![
            ("claude-code".to_owned(), String::new(), 15, 150, 0),
            ("codex".to_owned(), String::new(), 7, 70, 0),
        ]
    );
}

#[test]
fn step_moves_the_message_in_flight_so_its_next_line_adds_only_growth() {
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
