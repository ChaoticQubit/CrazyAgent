use crazyagent::config::resolve_config;
use crazyagent::engine::bootstrap::EngineService;
use rusqlite::{Connection, OpenFlags};
use std::os::unix::fs::PermissionsExt;
use tokio::sync::watch;

#[tokio::test]
async fn real_owner_database_installs_empty_bootstrap_receipts_without_execution_tasks() {
    let root = tempfile::Builder::new()
        .prefix("ca-mb-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("global.toml"), &root.path().join("project.toml"))
        .unwrap();
    let (stop, stopped) = watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let db = Connection::open_with_flags(
        root.path().join("engine.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let receipts: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name='bootstrap_workspaces'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let tasks: i64 = db
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .unwrap();
    let auth: i64 = db
        .query_row("SELECT COUNT(*) FROM native_auth_operations", [], |row| row.get(0))
        .unwrap();
    let rows = if receipts == 1 {
        Some(
            db.query_row("SELECT COUNT(*) FROM bootstrap_workspaces", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        )
    } else {
        None
    };
    drop(db);
    stop.send(true).unwrap();
    service.shutdown().await.unwrap();
    assert_eq!(tasks, 0);
    assert_eq!(auth, 0);
    assert_eq!(receipts, 1, "owner database lacks pre-plan allocation receipts");
    assert_eq!(rows, Some(0));
}
