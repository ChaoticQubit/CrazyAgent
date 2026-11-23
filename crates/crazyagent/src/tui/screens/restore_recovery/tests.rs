use super::*;
use crate::restore::Recovery;
use std::path::PathBuf;

const FOLDER: &str = "/home/mira/.local/share/crazyagent/engine/backups";

/// A read of the recovery offer at the test folder. Input: the offer. Output: the read.
fn read(offer: Option<Recovery>) -> Result<Read, String> {
    Ok(Read {
        offer,
        folder: PathBuf::from(FOLDER),
    })
}

/// The keys a read offers. Input: the read. Output: the keys.
fn keys(read: &Result<Read, String>) -> Vec<char> {
    actions(read).into_iter().map(|(key, _)| key).collect()
}

#[test]
fn a_named_copy_that_passes_its_check_is_offered_back_with_the_backups_folder() {
    let shown = read(Some(Recovery::PutBack {
        copy: "replaced-20261117T093000000Z".into(),
    }));
    let lines = lines(&shown, None);
    assert!(
        lines.iter().any(|line| line == RECOVERY_BROKEN),
        "{lines:?}"
    );
    assert!(lines.iter().any(|line| line.contains(FOLDER)), "{lines:?}");
    assert_eq!(keys(&shown), vec!['p']);
    assert_eq!(actions(&shown)[0].1, PUT_BACK);
}

#[test]
fn a_restore_that_changed_nothing_offers_to_continue_with_the_2_5_data() {
    let shown = read(Some(Recovery::Continue));
    let lines = lines(&shown, None);
    assert!(
        lines.iter().any(|line| line == RECOVERY_CONTINUE),
        "{lines:?}"
    );
    assert_eq!(keys(&shown), vec!['n']);
    assert_eq!(actions(&shown)[0].1, CONTINUE);
}

#[test]
fn a_restore_with_no_usable_copy_offers_nothing_but_the_folder_and_the_code() {
    let shown = read(Some(Recovery::Stuck {
        code: "storage_putback_copy_missing".into(),
    }));
    let lines = lines(&shown, None);
    assert!(
        lines.iter().any(|line| line == RECOVERY_BROKEN),
        "{lines:?}"
    );
    assert!(lines.iter().any(|line| line.contains(FOLDER)), "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|line| line.contains("storage_putback_copy_missing")),
        "{lines:?}"
    );
    assert!(keys(&shown).is_empty());
}

#[test]
fn a_finished_restore_shows_it_running_and_a_refusal_shows_its_code() {
    let finished = read(Some(Recovery::Finished {
        copy: "replaced-20261117T093000000Z".into(),
    }));
    let lines_now = lines(&finished, None);
    assert!(
        lines_now
            .iter()
            .any(|line| line == crate::tui::screens::settings::restore::RESTORE_RUNNING),
        "{lines_now:?}"
    );
    assert!(keys(&finished).is_empty());
    let refused = lines(
        &read(Some(Recovery::Continue)),
        Some("storage_putback_busy"),
    );
    assert!(
        refused
            .iter()
            .any(|line| line.contains("storage_putback_busy")),
        "{refused:?}"
    );
    let failed: Result<Read, String> = Err("storage_putback_busy".into());
    assert!(keys(&failed).is_empty());
    assert!(
        lines(&failed, None)
            .iter()
            .any(|line| line.contains("storage_putback_busy"))
    );
}
