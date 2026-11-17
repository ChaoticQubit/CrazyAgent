#![cfg(unix)]

mod e2e_support;

use crazyagent::engine::owner::OwnerResult;
use crazyagent::ide::workspace;
use crazyagent::protocol::ProjectId;
use e2e_support::{Engine, Owner, scratch};
use rusqlite::Connection;
use std::fs;
use std::time::Duration;

const PROJECT: &str = "v2-ide-agents";
const OUT_OF_PROCESS: &str = "state_log_fence_out_of_process";

/// How many rows of a table of the engine database match a filter, none for a table not
/// created yet. Inputs: the database, the table and the filter. Output: the count.
fn count(db: &Connection, table: &str, filter: &str) -> i64 {
    let exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |row| row.get(0),
        )
        .unwrap();
    if !exists {
        return 0;
    }
    db.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE {filter}"),
        [],
        |row| row.get(0),
    )
    .unwrap()
}

/// A workspace write from outside the engine process is refused at append (v2.5 plan 3.2,
/// L7 Q7 c): this test process holds the running engine's own host stamp, read from its
/// database, yet that host runs only in the engine's process, so the write and the Show all
/// switch leave no workspace row, no ide event and no file. The cap and Show all rows run
/// in process on the log (src/ide/log/tests.rs).
#[tokio::test(flavor = "multi_thread")]
async fn ide_workspace_write_from_outside_the_engine_process_is_refused() {
    let engine = Engine::start("ca-v2opt-agents-out-");
    let mut owner = Owner::attach(&engine).await;
    let disk = scratch("ca-v2opt-agents-home-");
    let home = disk.path().join("Projects");
    fs::create_dir(&home).unwrap();
    assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
    assert_eq!(
        owner.create_project(PROJECT, "Buoy agents").await,
        OwnerResult::Applied
    );
    let project = ProjectId::new(PROJECT).unwrap();
    let db = Connection::open(engine.path().join("engine.db")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    let host = crazyagent::ide::log::active_host(&db)
        .unwrap()
        .expect("the engine's host runs");

    let written = workspace::write(engine.path(), &host, &project).unwrap_err();
    assert_eq!(written.0, OUT_OF_PROCESS);
    let switched = workspace::show_all(engine.path(), &host, &project, true).unwrap_err();
    assert_eq!(switched.0, OUT_OF_PROCESS);

    let followed = format!("project='{PROJECT}'");
    assert_eq!(count(&db, "ide_workspaces", &followed), 0);
    assert_eq!(count(&db, "state_log", "aggregate='ide'"), 0);
    let file = engine
        .path()
        .join(crazyagent::ide::FOLDER)
        .join(PROJECT)
        .join(workspace::FILE);
    assert!(fs::symlink_metadata(file).is_err(), "no workspace file");
}
