use super::Storage;
use crazyagent_win_sandbox::private;
use std::path::{Path, PathBuf};

/// A temp root holding a W1 private data folder, the engine database path inside it and a
/// config with no settings files. Output: the root guard, the database path and the config.
fn fixture() -> (tempfile::TempDir, PathBuf, crate::config::EffectiveConfig) {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    drop(private::dir(&data).unwrap());
    let config =
        crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    (root, data.join("engine.db"), config)
}

/// Grants authenticated users inheritable modify on a folder through icacls. Input: the
/// folder.
fn open_to_other_users(folder: &Path) {
    let status = std::process::Command::new("icacls")
        .args([folder.to_str().unwrap(), "/grant", "*S-1-5-11:(OI)(CI)(M)"])
        .status()
        .unwrap();
    assert!(status.success());
}

/// W1 D1-D2: engine storage opens in a private data folder on Windows, and the database
/// and the write-ahead and shared-memory files SQLite makes beside it pass W1's check.
#[tokio::test]
async fn engine_storage_opens_in_a_private_folder_and_its_side_files_stay_private() {
    let (_root, path, config) = fixture();
    let storage = Storage::open(&path, config, false).await.unwrap();
    for name in ["engine.db", "engine.db-wal", "engine.db-shm"] {
        let side = path.with_file_name(name);
        let file = private::file(&side, false).unwrap_or_else(|error| panic!("{name}: {error}"));
        private::check(&file).unwrap_or_else(|error| panic!("{name}: {error}"));
    }
    drop(storage);
}

/// W1 D4: a second engine on the same data folder is refused by the engine lock, as on
/// Unix.
#[tokio::test]
async fn a_second_engine_on_the_same_folder_is_refused() {
    let (_root, path, config) = fixture();
    let first = Storage::open(&path, config.clone(), false).await.unwrap();
    let Err(error) = Storage::open(&path, config, false).await else {
        panic!("a second engine opened the same database");
    };
    assert!(
        error.to_string().contains("database_lock_acquire_failed"),
        "{error}"
    );
    drop(first);
}

/// Closes engine storage through its own close job, which releases the engine lock before it
/// answers. Input: the storage. Output: none; a failed close fails the test.
async fn close(storage: Storage) {
    let (send, receive) = tokio::sync::oneshot::channel();
    storage
        .sender
        .try_send(super::Job::Close(send))
        .unwrap_or_else(|_| panic!("the close job was not queued"));
    receive.await.unwrap().unwrap();
}

/// W1 D3 (F2): while the engine runs, neither its database nor its data folder can be
/// renamed away, so the path checked before SQLite opened it keeps naming the same file.
#[tokio::test]
async fn the_database_and_its_folder_cannot_be_renamed_while_the_engine_runs() {
    let (root, path, config) = fixture();
    let data = path.parent().unwrap().to_path_buf();
    let storage = Storage::open(&path, config, false).await.unwrap();
    assert!(std::fs::rename(&path, data.join("moved.db")).is_err());
    assert!(std::fs::rename(&data, root.path().join("moved")).is_err());
    assert!(path.is_file() && !data.join("moved.db").exists());
    close(storage).await;
}

/// W1 D4 (rv-code4 F2): the lock probe says free before an engine opens the folder, held
/// while one runs, and free again once that engine has closed.
#[tokio::test]
async fn the_lock_probe_sees_a_running_engine_and_its_close() {
    let (_root, path, config) = fixture();
    let data = path.parent().unwrap().to_path_buf();
    assert!(super::engine_lock_released(&data));
    let storage = Storage::open(&path, config, false).await.unwrap();
    assert!(!super::engine_lock_released(&data));
    close(storage).await;
    assert!(super::engine_lock_released(&data));
}

/// W1 D1 and D5: a data folder other users may change is refused by W1's check, not by
/// the blanket `storage_unavailable` refusal the port replaces.
#[tokio::test]
async fn a_data_folder_other_users_may_change_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::create_dir(&data).unwrap();
    open_to_other_users(&data);
    let config =
        crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let Err(error) = Storage::open(&data.join("engine.db"), config, false).await else {
        panic!("storage opened in a folder other users may change");
    };
    assert!(
        !error.to_string().contains("storage_unavailable"),
        "{error}"
    );
}
