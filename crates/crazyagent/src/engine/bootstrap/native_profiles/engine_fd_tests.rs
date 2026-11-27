use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

const CHILD_TOOK_LOCK: i32 = 5;
const CHILD_REFUSED: i32 = 3;
const CHILD_DATABASE: &str = "CRAZYAGENT_ENGINE_FD_CHILD_DB";

/// Runs in a child process: tries to take engine.db out of WAL mode with no busy wait,
/// which needs the exclusive lock on the database file itself. Input: the database path
/// in `CRAZYAGENT_ENGINE_FD_CHILD_DB`. Output: exit code 5 when the mode changed, 3 when
/// SQLite was busy, 4 on anything else.
#[test]
#[ignore = "child of the engine.db descriptor rows"]
fn engine_fd_child() {
    let Some(path) = std::env::var_os(CHILD_DATABASE) else {
        return;
    };
    let db = rusqlite::Connection::open(path).unwrap();
    db.busy_timeout(std::time::Duration::ZERO).unwrap();
    let code = match db.query_row("PRAGMA journal_mode=DELETE", [], |row| {
        row.get::<_, String>(0)
    }) {
        Ok(mode) if mode == "delete" => CHILD_TOOK_LOCK,
        Ok(_) => CHILD_REFUSED,
        Err(rusqlite::Error::SqliteFailure(failure, _))
            if failure.code == rusqlite::ErrorCode::DatabaseBusy =>
        {
            CHILD_REFUSED
        }
        Err(_) => 4,
    };
    std::process::exit(code);
}

/// Runs the child on engine.db from another process and returns its exit code.
/// Input: the database path. Output: 5, 3 or 4, as `engine_fd_child` exits.
fn child(path: &Path) -> i32 {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "engine::bootstrap::native_profiles::engine_fd_tests::engine_fd_child",
            "--exact",
            "--ignored",
            "--quiet",
        ])
        .env(CHILD_DATABASE, path)
        .status()
        .unwrap()
        .code()
        .unwrap()
}

struct IdleWal {
    root: tempfile::TempDir,
    database: PathBuf,
    _connection: rusqlite::Connection,
}

/// Opens a private engine.db in WAL mode with one write done and leaves its connection
/// idle, after checking that the child is refused while that connection lives.
/// Input: none. Output: the root, the database path and the idle connection.
fn idle_wal() -> IdleWal {
    let root = tempfile::Builder::new()
        .prefix("ca-fd-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let state = root.path().join("state");
    std::fs::create_dir(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    let database = state.join("engine.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mode: String = connection
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    connection
        .execute_batch("CREATE TABLE t (x INTEGER); INSERT INTO t VALUES (1);")
        .unwrap();
    assert!(connection.is_autocommit());
    assert_eq!(
        child(&database),
        CHILD_REFUSED,
        "the child sees the idle WAL connection's lock"
    );
    IdleWal {
        root,
        database,
        _connection: connection,
    }
}

#[test]
fn a_dropped_custody_leaves_an_idle_wal_engine_db_held() {
    let wal = idle_wal();
    let custody =
        NativeProfileCustody::capture(&wal.database, &std::fs::metadata(&wal.database).unwrap());
    assert!(custody.is_ok());
    drop(custody);
    assert_eq!(
        child(&wal.database),
        CHILD_REFUSED,
        "another process took engine.db after a custody was dropped"
    );
}

#[test]
fn a_dropped_retained_custody_leaves_an_idle_wal_engine_db_held() {
    let wal = idle_wal();
    let Ok(custody) =
        NativeProfileCustody::capture(&wal.database, &std::fs::metadata(&wal.database).unwrap())
    else {
        panic!("custody over a private engine.db");
    };
    let retained = custody.retain();
    assert!(retained.is_ok());
    drop(retained);
    assert_eq!(
        child(&wal.database),
        CHILD_REFUSED,
        "another process took engine.db after a retained custody was dropped"
    );
    drop(custody);
}

#[test]
fn a_refused_capture_leaves_an_idle_wal_engine_db_held() {
    let wal = idle_wal();
    let other = wal.root.path().join("other");
    std::fs::write(&other, b"other").unwrap();
    let refused = NativeProfileCustody::capture(&wal.database, &std::fs::metadata(&other).unwrap());
    assert!(matches!(&refused, Err(failure) if failure.0 == "native_profile_custody_unsafe"));
    drop(refused);
    assert_eq!(
        child(&wal.database),
        CHILD_REFUSED,
        "another process took engine.db after a refused capture"
    );
}

#[test]
fn a_held_engine_db_is_closed_on_exec() {
    let wal = idle_wal();
    let Ok(custody) =
        NativeProfileCustody::capture(&wal.database, &std::fs::metadata(&wal.database).unwrap())
    else {
        panic!("custody over a private engine.db");
    };
    let flags = rustix::io::fcntl_getfd(&custody.database).unwrap();
    assert!(
        flags.contains(rustix::io::FdFlags::CLOEXEC),
        "a held engine.db descriptor would pass into a child process"
    );
}
