#![cfg(unix)]

use crazyagent::engine::bootstrap::EngineService;
use crazyagent_desktop::bridge::{BridgeQueue, NativeBridge};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const WAVES: usize = 40;
const READS_PER_WAVE: usize = 8;

/// Fires waves of plain reads at once through the window's bridge queue, as a project
/// Overview does, and collects every failed read's code and engine cause. Input: the
/// engine folder. Output: the failures, or the connect failure.
async fn waves(root: &Path) -> Result<Vec<String>, String> {
    let queue = Arc::new(BridgeQueue::new(NativeBridge::new(Some(
        root.join("e.sock"),
    ))));
    let status = queue
        .enter()
        .await
        .map_err(|e| e.code.to_owned())?
        .connect()
        .await
        .map_err(|e| e.code.to_owned())?;
    assert!(status.connected);
    let mut failed = Vec::new();
    for _ in 0..WAVES {
        let mut reads = tokio::task::JoinSet::new();
        for at in 0..READS_PER_WAVE {
            let queue = Arc::clone(&queue);
            reads.spawn(async move {
                let bridge = queue.read().await?;
                if at % 2 == 0 {
                    bridge.harness_catalog(false).await.map(drop)
                } else {
                    bridge.pool_summaries().await.map(drop)
                }
            });
        }
        while let Some(read) = reads.join_next().await {
            if let Err(error) = read.unwrap() {
                failed.push(format!(
                    "{} ({})",
                    error.code,
                    error.cause.unwrap_or_default()
                ));
            }
        }
    }
    Ok(failed)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_overview_s_reads_side_by_side_are_all_served_by_the_engine() {
    let root = tempfile::Builder::new()
        .prefix("ca-desk-reads-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("g.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 2000\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &root.path().join("p.toml"),
        &stopped,
    )
    .await
    .unwrap();
    let path = root.path().to_path_buf();
    let mut body = tokio::spawn(async move { waves(&path).await });
    let result = tokio::time::timeout(Duration::from_secs(60), &mut body).await;
    if result.is_err() {
        body.abort();
        let _ = body.await;
    }
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(
        matches!(cleanup, Ok(Ok(()))),
        "actual owner service must stop"
    );
    let failed = result.unwrap().unwrap().unwrap();
    assert_eq!(
        failed,
        Vec::<String>::new(),
        "{} of {} reads failed",
        failed.len(),
        WAVES * READS_PER_WAVE
    );
}
