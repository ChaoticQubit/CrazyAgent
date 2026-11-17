use super::initialize;
use rusqlite::Connection;

const LANDING_BUILD_WAITING_RESOLVES: &str = "CREATE TABLE IF NOT EXISTS master_waiting_resolves (
session TEXT PRIMARY KEY,
project TEXT NOT NULL,
operation TEXT NOT NULL,
input INTEGER NOT NULL CHECK(input > 0),
body TEXT NOT NULL
)";

fn waiting_resolves(db: &Connection) -> bool {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='master_waiting_resolves')",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn a_fresh_database_keeps_waiting_resolves_and_reopens() {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    assert!(waiting_resolves(&db));
    initialize(&db).unwrap();
    assert!(waiting_resolves(&db));
}

#[test]
fn a_database_from_a_build_that_made_the_table_on_first_use_reopens() {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    db.execute_batch(LANDING_BUILD_WAITING_RESOLVES).unwrap();
    db.execute(
        "INSERT INTO master_waiting_resolves(session,project,operation,input,body) VALUES ('s','p','o',1,'{}')",
        [],
    )
    .unwrap();
    initialize(&db).unwrap();
    let kept: i64 = db
        .query_row("SELECT COUNT(*) FROM master_waiting_resolves", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(kept, 1);
}

#[test]
fn a_database_from_before_waiting_resolves_gains_the_table() {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    db.execute_batch("DROP TABLE IF EXISTS master_waiting_resolves")
        .unwrap();
    initialize(&db).unwrap();
    assert!(waiting_resolves(&db));
}

#[test]
fn an_unknown_master_object_is_still_a_conflict() {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    db.execute_batch("CREATE TABLE master_zz (x INTEGER)")
        .unwrap();
    assert_eq!(
        initialize(&db).unwrap_err().to_string(),
        "master_schema_conflict"
    );
}
