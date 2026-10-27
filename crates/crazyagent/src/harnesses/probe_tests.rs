use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn codex(entries: &[HarnessEntry]) -> &HarnessStatus {
    &entries
        .iter()
        .find(|entry| entry.id == "codex")
        .expect("codex is in the catalog")
        .status
}

fn install(folder: &Path) {
    let path = folder.join("codex");
    std::fs::write(&path, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(not(target_os = "linux"))]
#[test]
fn the_built_in_search_skips_linux_only_folders_under_the_home_automount() {
    assert!(
        SYSTEM_DIRECTORIES
            .iter()
            .all(|directory| !directory.starts_with("/home/"))
    );
    assert!(
        !Search::from_environment()
            .directories
            .contains(&PathBuf::from("/home/linuxbrew/.linuxbrew/bin"))
    );
}

#[tokio::test]
async fn a_cached_catalog_is_served_while_its_re_probe_runs_and_a_check_probes_now() {
    let folder = tempfile::Builder::new()
        .prefix("probe-cache-")
        .tempdir()
        .unwrap();
    let search = Search::new(vec![folder.path().to_path_buf()], None, Vec::new());
    let cache = ProbeCache::new(Duration::ZERO);

    let first = cache.entries(search.clone(), false).await.unwrap();
    assert_eq!(codex(&first), &HarnessStatus::NotInstalled);
    assert!(!cache.probing());

    install(folder.path());
    let served = cache.entries(search.clone(), false).await.unwrap();
    assert_eq!(
        codex(&served),
        &HarnessStatus::NotInstalled,
        "a stale read answers from the cache instead of waiting on the probe"
    );
    assert!(cache.probing(), "the stale read started a background probe");
    let again = cache.entries(search.clone(), false).await.unwrap();
    assert_eq!(codex(&again), &HarnessStatus::NotInstalled);

    let mut ready = false;
    for _ in 0..200 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        if !cache.probing() {
            ready = matches!(
                codex(&cache.entries(search.clone(), false).await.unwrap()),
                HarnessStatus::Ready
            );
            break;
        }
    }
    assert!(ready, "the background probe replaced the cached catalog");

    std::fs::remove_file(folder.path().join("codex")).unwrap();
    let checked = cache.entries(search.clone(), true).await.unwrap();
    assert_eq!(
        codex(&checked),
        &HarnessStatus::NotInstalled,
        "an owner's check probes before answering"
    );

    let elsewhere = tempfile::Builder::new()
        .prefix("probe-other-")
        .tempdir()
        .unwrap();
    install(elsewhere.path());
    let moved = Search::new(vec![elsewhere.path().to_path_buf()], None, Vec::new());
    assert_eq!(
        codex(&cache.entries(moved, false).await.unwrap()),
        &HarnessStatus::Ready,
        "a changed search is never answered from another search's probe"
    );
}

#[tokio::test]
async fn a_fresh_cached_catalog_starts_no_probe() {
    let folder = tempfile::Builder::new()
        .prefix("probe-fresh-")
        .tempdir()
        .unwrap();
    let search = Search::new(vec![folder.path().to_path_buf()], None, Vec::new());
    let cache = ProbeCache::new(Duration::from_secs(3600));
    cache.entries(search.clone(), false).await.unwrap();
    install(folder.path());
    let served = cache.entries(search, false).await.unwrap();
    assert_eq!(codex(&served), &HarnessStatus::NotInstalled);
    assert!(!cache.probing());
}
