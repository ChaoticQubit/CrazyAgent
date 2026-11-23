use std::time::{Duration, Instant};

use super::{install_release, lock_install};
use kernel_processes::update::{Asset, Manifest, target};

/// An update run that finds the install folder held by another waits its bound, then fails
/// with `update_in_progress`; once the first run lets go, the next one takes the folder.
#[test]
fn a_second_update_run_on_one_install_folder_waits_then_is_refused() {
    let install = tempfile::tempdir().unwrap();
    let held = lock_install(install.path(), Duration::ZERO).unwrap();
    let folder = install.path().to_path_buf();
    let second = std::thread::spawn(move || {
        let started = Instant::now();
        (
            lock_install(&folder, Duration::from_millis(200)).map(|_| ()),
            started.elapsed(),
        )
    })
    .join()
    .unwrap();
    let refused = second.0.unwrap_err();
    assert_eq!(refused.code, "update_in_progress", "{}", refused.message);
    assert!(second.1 >= Duration::from_millis(200), "{:?}", second.1);
    assert!(second.1 < Duration::from_secs(5), "{:?}", second.1);
    drop(held);
    lock_install(install.path(), Duration::ZERO).unwrap();
}

/// An install that fails after taking the install folder lets go of it, so the next run
/// takes it at once.
#[test]
fn a_failed_install_lets_go_of_the_install_folder() {
    let install = tempfile::tempdir().unwrap();
    let platform = target().expect("a published platform");
    let manifest = Manifest {
        version: "9.9.9".to_owned(),
        platforms: [(
            platform.to_owned(),
            Asset {
                url: "http://127.0.0.1:9/crazyagent.tar.gz".to_owned(),
                sha256: "0".repeat(64),
                signature: String::new(),
            },
        )]
        .into_iter()
        .collect(),
    };
    assert!(install_release(&manifest, install.path(), "").is_err());
    lock_install(install.path(), Duration::ZERO).unwrap();
}
