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

/// Waits for the job in flight to end and hands its result back to the poll, with no
/// sleep. Input: the screen. Output: none.
fn settle(screen: &mut Recovering) {
    let receive = screen.pending.take().expect("a job runs");
    let done = receive.recv().expect("the job sends its end");
    let (send, again) = channel();
    send.send(done).expect("the poll's channel is open");
    screen.pending = Some(again);
}

/// A refused put-back keeps its code and reads the offer again, so a record that went
/// away under it is never offered once more.
#[test]
fn a_refused_put_back_reads_again_and_keeps_its_code() {
    let offered = read(Some(Recovery::PutBack {
        copy: "replaced-20261117T093000000Z".into(),
    }));
    let mut screen =
        Recovering::acted(offered, Step::PutBack, Err("storage_restore_failed".into()));

    assert_eq!(screen.poll(), Next::Stay);
    assert!(screen.busy());
    settle(&mut screen);
    assert_eq!(screen.poll(), Next::Stay);

    let lines = screen.lines();
    assert!(
        lines.iter().any(|line| line == "Code: not_configured"),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line == "Code: storage_restore_failed"),
        "{lines:?}"
    );
    assert!(screen.actions().is_empty());
}

/// A refused continue reads the offer again, as a refused put-back does.
#[test]
fn a_refused_continue_reads_again() {
    let mut screen = Recovering::acted(
        read(Some(Recovery::Continue)),
        Step::Continue,
        Err("storage_putback_check_failed".into()),
    );

    assert_eq!(screen.poll(), Next::Stay);
    assert!(screen.busy());
    settle(&mut screen);
    assert_eq!(screen.poll(), Next::Stay);
}

/// A refused finish keeps its read and code and starts no read, since a read that still
/// finds the finished restore would run the finish again on its own.
#[test]
fn a_refused_finish_does_not_read_again() {
    let finished = read(Some(Recovery::Finished {
        copy: "replaced-20261117T093000000Z".into(),
    }));
    let mut screen =
        Recovering::acted(finished, Step::Finish, Err("storage_restore_failed".into()));

    assert_eq!(screen.poll(), Next::Stay);
    assert!(!screen.busy());
    let lines = screen.lines();
    assert!(
        lines
            .iter()
            .any(|line| line == crate::tui::screens::settings::restore::RESTORE_RUNNING),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line == "Code: storage_restore_failed"),
        "{lines:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_job_reads_the_engine_the_screen_was_started_with() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let layout = crate::paths::Layout::new(home.path().to_path_buf()).unwrap();
    std::fs::create_dir_all(layout.root())?;
    std::fs::write(
        layout.root().join(kernel_storage::migrations::DATABASE),
        b"",
    )?;
    let Done::Read(Ok(read)) = run(Step::Read, &layout, false) else {
        panic!("an engine with no restore record reads as nothing to recover");
    };
    assert_eq!(read.offer, None);
    assert_eq!(read.folder, crate::restore::backups(layout.root()));
    Ok(())
}
