use super::*;
use crate::engine::EngineError;
use crate::engine::owner::OwnerResponse;
use crate::tui::client::OwnerEvent;
use crate::tui::client::app_setup::restored;
use crate::tui::frame::Tone;
use crate::tui::screens::settings::{sections, tabs};
use crate::tui::screens::settings_accounts::tests::ctx;
use crate::tui::seam::Request;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const UNTIL_MS: i64 = 1_797_500_000_000;
const NOW_MS: i64 = UNTIL_MS - 10 * 86_400_000;
const CODES: [&str; 6] = [
    "engine_restoring",
    "engine_restored",
    "storage_restore_unavailable",
    "storage_restore_agents_running",
    "storage_restore_busy",
    "storage_restore_failed",
];

fn event(result: Result<(), String>) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::AppSetup(AppSetupEvent::Restored(result)),
    ))))
}

fn jobs(ctx: &mut Ctx) -> Vec<AppSetupJob> {
    std::iter::from_fn(|| ctx.next_request())
        .take(64)
        .filter_map(|request| match request {
            Request::Settings(job) => match *job {
                SettingsJob::AppSetup(job) => Some(job),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn offered() -> RestorePage {
    RestorePage::with(offer(Some(UNTIL_MS), None, NOW_MS, 0))
}

fn confirm(page: &mut RestorePage, ctx: &mut Ctx) {
    page.key(&Key::Enter, ctx);
    page.key(&Key::Right, ctx);
    page.key(&Key::Enter, ctx);
}

fn words(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.content.to_string()))
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn the_page_offers_the_restore_only_while_the_backup_lasts_and_no_restore_runs_or_is_done() {
    let open = offer(Some(UNTIL_MS), None, NOW_MS, 0);
    assert!(open.offer);
    assert!(
        open.line
            .starts_with("You can bring back your 2.4 data until "),
        "{}",
        open.line
    );
    assert_eq!(open.failure, None);
    let none = Offer {
        offer: false,
        line: RESTORE_NONE.into(),
        failure: None,
    };
    assert_eq!(offer(None, None, NOW_MS, 0), none);
    let running = offer(Some(UNTIL_MS), Some(&RestoreState::Pending), NOW_MS, 0);
    assert_eq!(running.line, RESTORE_RUNNING);
    assert!(!running.offer);
    let restored_state = RestoreState::Restored {
        copy: "replaced-20261117T093000000Z".into(),
    };
    let done = offer(Some(UNTIL_MS), Some(&restored_state), NOW_MS, 0);
    assert_eq!(done.line, plain("engine_restored"));
    assert!(!done.offer);
    let busy = RestoreState::Failed {
        code: "storage_restore_busy".into(),
    };
    let again = offer(Some(UNTIL_MS), Some(&busy), NOW_MS, 0);
    assert!(again.offer, "a failed restore may be tried again");
    assert_eq!(
        again.failure.as_deref(),
        Some(plain("storage_restore_busy"))
    );
    let late = offer(None, Some(&busy), NOW_MS, 0);
    assert!(!late.offer);
    assert_eq!(late.line, RESTORE_NONE);
    assert_eq!(late.failure.as_deref(), Some(plain("storage_restore_busy")));
}

#[test]
fn the_confirm_names_the_three_things_a_restore_does_and_sends_it_once() {
    let mut ctx = ctx(100, 30);
    let mut page = offered();
    assert!(page.modal(&ctx).is_none(), "nothing asks before Enter");
    assert!(matches!(page.key(&Key::Enter, &mut ctx), Flow::Handled));
    let (modal, _) = page.modal(&ctx).expect("Enter opens the confirm");
    let asked = words(&modal.body);
    for line in RESTORE_CONFIRM {
        assert!(asked.contains(line), "the confirm says {line}");
    }
    page.key(&Key::Enter, &mut ctx);
    assert!(page.modal(&ctx).is_none(), "Enter on Cancel closes it");
    assert!(jobs(&mut ctx).is_empty(), "Cancel sends nothing");
    page.key(&Key::Enter, &mut ctx);
    page.key(&Key::Esc, &mut ctx);
    assert!(page.modal(&ctx).is_none(), "Esc closes it");
    assert!(jobs(&mut ctx).is_empty(), "Esc sends nothing");

    confirm(&mut page, &mut ctx);

    let sent = jobs(&mut ctx);
    assert_eq!(sent.len(), 1);
    assert!(matches!(sent[0], AppSetupJob::Restore { .. }));
    confirm(&mut page, &mut ctx);
    assert!(page.modal(&ctx).is_none());
    assert!(jobs(&mut ctx).is_empty(), "a restore is sent once");
    let mut gone = RestorePage::with(offer(None, None, NOW_MS, 0));
    gone.key(&Key::Enter, &mut ctx);
    assert!(
        gone.modal(&ctx).is_none(),
        "no confirm once the backup is gone"
    );
}

#[test]
fn a_refused_restore_says_why_in_its_own_sentence_and_an_admitted_one_shows_it_running() {
    let mut ctx = ctx(100, 30);
    let mut page = offered();
    page.arrive(&event(Ok(())), &mut ctx);
    assert!(
        ctx.message().is_none(),
        "an answer to nothing sent is not shown"
    );
    confirm(&mut page, &mut ctx);
    assert_eq!(jobs(&mut ctx).len(), 1);

    page.arrive(
        &event(Err("storage_restore_agents_running".into())),
        &mut ctx,
    );

    let said = ctx.message().expect("the refusal is shown").clone();
    assert_eq!(said.tone, Tone::Warning);
    assert_eq!(said.lead, plain("storage_restore_agents_running"));
    assert!(
        page.shown.offer,
        "a refusal changed nothing, so it can be tried again"
    );
    confirm(&mut page, &mut ctx);
    assert_eq!(jobs(&mut ctx).len(), 1);

    page.arrive(&event(Ok(())), &mut ctx);

    assert_eq!(ctx.message().map(|said| said.tone), Some(Tone::Success));
    assert!(!page.shown.offer);
    assert_eq!(page.shown.line, RESTORE_RUNNING);
    let fallback = plain("storage_restore_failed");
    for code in CODES {
        assert!(!plain(code).is_empty(), "{code}");
        assert!(!plain(code).contains('('), "{code}");
        assert!(
            code == "storage_restore_failed" || plain(code) != fallback,
            "{code}"
        );
    }
    assert_eq!(plain("stale_revision"), fallback);
}

#[test]
fn the_restore_tab_shows_only_for_an_engine_with_a_2_4_backup_or_a_restore_record() {
    let mut ctx = ctx(160, 30);
    let strip = |ctx: &Ctx, active| words(&tabs(active, ctx)[..1]);
    assert_eq!(
        sections(SettingsSection::Profile, &ctx),
        SettingsSection::ALL.to_vec()
    );
    assert!(!strip(&ctx, SettingsSection::Profile).contains("Restore 2.4"));
    assert!(
        sections(SettingsSection::Restore, &ctx).contains(&SettingsSection::Restore),
        "an open Restore page keeps its tab"
    );
    ctx.restore_shown = true;
    let shown = sections(SettingsSection::Profile, &ctx);
    assert_eq!(shown.len(), SettingsSection::ALL.len() + 1);
    assert_eq!(shown.last(), Some(&SettingsSection::Restore));
    assert!(strip(&ctx, SettingsSection::Restore).contains("Restore 2.4"));
    let next = |section: SettingsSection, forward| section.step(forward, &shown);
    assert_eq!(
        next(SettingsSection::Profile, true),
        SettingsSection::Restore
    );
    assert_eq!(
        next(SettingsSection::Restore, true),
        SettingsSection::Accounts
    );
    assert_eq!(
        next(SettingsSection::Accounts, false),
        SettingsSection::Restore
    );

    let root = tempfile::Builder::new()
        .prefix("ca-tui-restore-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!offered_at(root.path()), "no backup and no record, no tab");
    let mut record = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.path().join("restore-record"))
        .unwrap();
    std::io::Write::write_all(&mut record, b"failed storage_restore_busy").unwrap();
    drop(record);
    assert!(offered_at(root.path()), "a restore record shows the tab");
}

#[test]
fn an_unanswered_restore_counts_as_sent_and_drops_the_connection_and_a_refusal_keeps_its_code() {
    let mut lost = false;
    assert_eq!(restored(Ok(None), &mut lost), Ok(()));
    assert!(
        lost,
        "the engine stops to restore, so the connection is dropped"
    );
    let mut lost = false;
    let refused = OwnerResponse::Rejected {
        code: "storage_restore_agents_running".into(),
    };
    assert_eq!(
        restored(Ok(Some(refused)), &mut lost),
        Err("storage_restore_agents_running".to_owned())
    );
    assert!(!lost);
    let not_resent = EngineError("controller_restore_not_resent".into());
    assert_eq!(
        restored(Err(not_resent), &mut lost),
        Err("controller_restore_not_resent".to_owned())
    );
    assert!(!lost);
}

fn write_record(root: &std::path::Path, line: &str) {
    let path = root.join("restore-record");
    let _ = std::fs::remove_file(&path);
    let mut record = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    std::io::Write::write_all(&mut record, line.as_bytes()).unwrap();
}

/// L5 F1: a failed connect to the TUI's own engine names the restore that holds it, as the
/// desktop's connect does: restoring only while the login service still runs the copying
/// process, restored either way; a failed record or none leaves the problem as it was.
#[test]
fn a_failed_connect_names_the_restore_that_holds_the_engine() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-held-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let held_by = |running: bool| held("engine_start_failed".into(), root.path(), running);
    assert_eq!(held_by(true), "engine_start_failed", "no record");
    write_record(root.path(), "pending");
    assert_eq!(held_by(true), "engine_restoring");
    assert_eq!(
        held_by(false),
        "engine_start_failed",
        "a pending record whose process is gone was left by a restore that died"
    );
    write_record(root.path(), "restored replaced-20261117T020000Z");
    assert_eq!(held_by(true), "engine_restored");
    assert_eq!(held_by(false), "engine_restored");
    write_record(root.path(), "failed storage_restore_busy");
    assert_eq!(
        held_by(true),
        "engine_start_failed",
        "2.5 runs again after a failure"
    );
}
