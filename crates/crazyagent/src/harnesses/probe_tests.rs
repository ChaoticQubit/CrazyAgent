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

#[cfg(unix)]
fn entry_of<'a>(entries: &'a [HarnessEntry], id: &str) -> &'a HarnessEntry {
    entries
        .iter()
        .find(|entry| entry.id == id)
        .expect("the id is in the catalog")
}

/// v241 HR-L1: a found harness the tool resolver can't start is Can't run with the resolver's
/// line over Ready and over Limited control alike, while one that runs and one that is not
/// installed keep the status the probe gave them.
#[cfg(unix)]
#[test]
fn a_found_harness_the_resolver_cant_start_is_cant_run_over_ready_and_limited() {
    let folder = tempfile::Builder::new()
        .prefix("ca-needs-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    for (name, text) in [
        ("codex", "#!/usr/bin/env ca-catalog-missing-runtime\n"),
        ("claude", "#!/bin/sh\n"),
    ] {
        let path = folder.path().join(name);
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let probed = catalog(&Search::new(
        vec![folder.path().to_path_buf()],
        None,
        Vec::new(),
    ));
    let places = crate::tools::Places::new(vec![folder.path().to_path_buf()], None);
    let line = "codex needs ca-catalog-missing-runtime, which is missing. Install it, or run \
                setup again from Settings. Then try again.";
    let cant_run = HarnessStatus::CantRun {
        reason: line.into(),
    };

    let read = with_needs(probed.clone(), &places);
    assert_eq!(codex(&read), &cant_run);
    assert_eq!(
        entry_of(&read, "codex").status_label,
        format!("Can't run: {line}")
    );
    assert_eq!(entry_of(&read, "claude-code").status, HarnessStatus::Ready);
    assert_eq!(entry_of(&read, "gemini-cli").executable, None);
    assert_eq!(
        entry_of(&read, "gemini-cli").status,
        HarnessStatus::NotInstalled
    );
    assert_eq!(
        read.iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        probed
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        "the catalog keeps its order"
    );

    let mut limited = probed;
    limited
        .iter_mut()
        .find(|entry| entry.id == "codex")
        .unwrap()
        .status = HarnessStatus::Limited {
        reason: "no CrazyAgent launch adapter yet".into(),
    };
    assert_eq!(
        codex(&with_needs(limited, &places)),
        &cant_run,
        "a harness that can't start is Can't run even where control is limited"
    );
}
