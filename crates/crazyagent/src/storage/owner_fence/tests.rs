use super::super::*;
use crate::state_log::fence::hosted;
use std::os::unix::fs::PermissionsExt;

/// Starts the owner runtime through the storage actor. Input: the storage. Output: its host.
async fn start(storage: &Storage) -> HostStamp {
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerStart(send))
        .unwrap_or_else(|_| panic!("owner start queue unavailable"));
    receive.await.unwrap().unwrap().host
}

#[tokio::test]
async fn an_engine_start_runs_its_host_here_and_its_stop_or_close_withdraws_it() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config =
        crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let storage = Storage::open(&root.path().join("db"), config, false)
        .await
        .unwrap();
    let host = start(&storage).await;
    assert!(hosted(&host), "a started engine's writers can append");
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerStop(host.clone(), send))
        .unwrap_or_else(|_| panic!("owner stop queue unavailable"));
    receive.await.unwrap().unwrap();
    assert!(!hosted(&host), "a stopped engine's writers are refused");
    let again = start(&storage).await;
    assert!(hosted(&again));
    assert_ne!(again.engine_epoch, host.engine_epoch);
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::Close(send))
        .unwrap_or_else(|_| panic!("close queue unavailable"));
    receive.await.unwrap().unwrap();
    assert!(!hosted(&again), "a closed engine's writers are refused");
}
