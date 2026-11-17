#![cfg(unix)]

mod e2e_support;

use crazyagent::cleanup_settings::{CleanupMode, CleanupSettings};
use crazyagent::client_controller::OwnerController;
use crazyagent::engine::owner::{
    OwnerClientKind, OwnerMutation, OwnerRead, OwnerResponse, OwnerResult,
};
use crazyagent::quit_settings::{LeftRunning, OnQuit, QuitSettings};
use crazyagent::sandbox::LinuxSockets;
use crazyagent::sandbox_settings::{SandboxSettings, StoredSandboxSettings};
use e2e_support::{Engine, Owner, result};
use std::time::Duration;

const ATTACH_TIME: Duration = Duration::from_secs(10);

const MINE: QuitSettings = QuitSettings {
    on_quit: OnQuit::Stop,
    left_running: LeftRunning::Never,
    hours: 12,
};

const THEIRS: QuitSettings = QuitSettings {
    on_quit: OnQuit::KeepRunning,
    left_running: LeftRunning::AfterHours,
    hours: 48,
};

/// Attaches the form's window and a second window to one running engine.
/// Input: the form window's client kind. Output: the engine, the form and the other window.
async fn windows(kind: OwnerClientKind) -> (Engine, OwnerController, Owner) {
    let engine = Engine::start("ca-own-retry-");
    let form = OwnerController::attach(engine.path(), kind, ATTACH_TIME)
        .await
        .unwrap();
    let other = Owner::attach(&engine).await;
    (engine, form, other)
}

/// Saves, from the other window, a setting the quit form does not show, which moves the
/// engine's one shared revision as an engine-own write does.
/// Inputs: the other window and the cleanup days it saves, different from the stored ones.
async fn change_another_setting(other: &mut Owner, days: u16) {
    let generation = other.next();
    let changed = other
        .controller
        .set_cleanup_settings(
            CleanupSettings {
                mode: CleanupMode::Manual,
                days,
            },
            generation,
        )
        .await
        .unwrap();
    assert_eq!(result(changed), OwnerResult::Applied);
}

/// Asserts the engine's own `stale_revision` refusal reached the form, recorded as the
/// outcome of the request the form sent, and that the controller sent it no second time.
/// Inputs: the form's answer and the form.
fn assert_stale_refusal(response: &OwnerResponse, form: &OwnerController) {
    assert!(
        matches!(response, OwnerResponse::Applied { operation }
            if !operation.historical_replay
                && matches!(&operation.result, OwnerResult::Rejected { code } if code == "stale_revision")),
        "{response:?}"
    );
    assert!(
        form.resent().is_none(),
        "a refused change is not sent again"
    );
}

/// V250-OWN-REVISION-RETRY red row. The quit form reads its settings, the other window
/// saves an unrelated setting, and the form's save lands: the controller reads the form
/// again, finds it unchanged and sends the same change once more under a new request id.
/// Input: the form window's client kind, so the TUI and the desktop run the same row.
async fn a_save_after_an_unrelated_change_lands(kind: OwnerClientKind) {
    let (_engine, mut form, mut other) = windows(kind).await;
    let shown = form.quit_settings().await.unwrap();
    assert_ne!(shown, MINE, "the form starts from the defaults");
    change_another_setting(&mut other, 30).await;
    form.begin_mutation(OwnerMutation::SetQuitSettings { settings: MINE }, 1)
        .unwrap();
    let first = form.pending().unwrap().request().request_id.clone();
    let saved = form.send_pending().await.unwrap();
    let OwnerResponse::Applied { operation } = saved else {
        panic!("the form's save must land after one resend: {saved:?}");
    };
    assert_eq!(operation.result, OwnerResult::Applied);
    assert_ne!(
        operation.provenance.operation_id, first,
        "the resend is its own request"
    );
    assert!(form.pending().is_none());
    assert_eq!(other.controller.quit_settings().await.unwrap(), MINE);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tui_save_after_an_unrelated_change_lands() {
    a_save_after_an_unrelated_change_lands(OwnerClientKind::Tui).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_desktop_save_after_an_unrelated_change_lands() {
    a_save_after_an_unrelated_change_lands(OwnerClientKind::Desktop).await;
}

/// Guard: when the other window saved the form's own setting in between, the form's save
/// is refused `stale_revision` as before and the other window's value stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_save_after_its_own_setting_changed_is_refused() {
    let (_engine, mut form, mut other) = windows(OwnerClientKind::Desktop).await;
    form.quit_settings().await.unwrap();
    let generation = other.next();
    let theirs = other
        .controller
        .set_quit_settings(THEIRS, generation)
        .await
        .unwrap();
    assert_eq!(result(theirs), OwnerResult::Applied);
    form.begin_mutation(OwnerMutation::SetQuitSettings { settings: MINE }, 1)
        .unwrap();
    let refused = form.send_pending().await.unwrap();
    assert_stale_refusal(&refused, &form);
    assert!(form.pending().is_none());
    assert!(
        !form.view().unwrap().fresh,
        "a refused change leaves the form to read again"
    );
    assert_eq!(form.quit_settings().await.unwrap(), THEIRS);
}

/// Guard: a save whose own version is older than the stored one is refused `stale_revision`
/// as before; the resend never refreshes a version inside the change.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_save_from_an_older_version_is_still_refused() {
    let (_engine, mut form, mut other) = windows(OwnerClientKind::Tui).await;
    let opened = form.sandbox_settings().await.unwrap();
    let theirs = SandboxSettings {
        linux_sockets: LinuxSockets::Refuse,
        ..opened.settings
    };
    let generation = other.next();
    let saved = other
        .controller
        .set_sandbox_settings(theirs, opened.version, generation)
        .await
        .unwrap();
    assert_eq!(result(saved), OwnerResult::Applied);
    let mine = SandboxSettings {
        linux_sockets: LinuxSockets::TcpPorts,
        ..opened.settings
    };
    let refused = form
        .set_sandbox_settings(mine, opened.version, 1)
        .await
        .unwrap();
    assert_stale_refusal(&refused, &form);
    assert!(
        !form.view().unwrap().fresh,
        "a refused change leaves the form to read again"
    );
    assert_eq!(
        form.sandbox_settings().await.unwrap(),
        StoredSandboxSettings {
            settings: theirs,
            version: opened.version + 1,
        }
    );
}

/// Guard: a save built after a read that carries no form data, `Capabilities`, is refused
/// `stale_revision` as before and changes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_save_after_a_capabilities_read_is_refused() {
    let (_engine, mut form, mut other) = windows(OwnerClientKind::Tui).await;
    let defaults = form.quit_settings().await.unwrap();
    form.read(OwnerRead::Capabilities).await.unwrap();
    change_another_setting(&mut other, 30).await;
    form.begin_mutation(OwnerMutation::SetQuitSettings { settings: MINE }, 1)
        .unwrap();
    let refused = form.send_pending().await.unwrap();
    assert_stale_refusal(&refused, &form);
    assert!(
        !form.view().unwrap().fresh,
        "a refused change leaves the form to read again"
    );
    assert_eq!(form.quit_settings().await.unwrap(), defaults);
}

/// V250-OWN-RETRY-BIND-QUERY red row. The quit form reads its settings and begins its save,
/// a background read of another query replaces the controller's view at the same revision,
/// and the other window saves the quit setting. The form's save is refused `stale_revision`
/// and not resent: the resend checks the read the save was built from, which changed, not
/// the controller's last read, which did not.
/// Input: the form window's client kind, so the TUI and the desktop run the same row.
async fn a_save_is_checked_against_its_own_read(kind: OwnerClientKind) {
    let (_engine, mut form, mut other) = windows(kind).await;
    form.quit_settings().await.unwrap();
    form.begin_mutation(OwnerMutation::SetQuitSettings { settings: MINE }, 1)
        .unwrap();
    let built_at = form.pending().unwrap().request().expected_revision.clone();
    let background = form.read(OwnerRead::CleanupSettings).await.unwrap();
    assert_eq!(
        background.revision, built_at,
        "the background read moved nothing"
    );
    let generation = other.next();
    let theirs = other
        .controller
        .set_quit_settings(THEIRS, generation)
        .await
        .unwrap();
    assert_eq!(result(theirs), OwnerResult::Applied);
    let refused = form.send_pending().await.unwrap();
    assert_stale_refusal(&refused, &form);
    assert_eq!(other.controller.quit_settings().await.unwrap(), THEIRS);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tui_save_is_checked_against_its_own_read() {
    a_save_is_checked_against_its_own_read(OwnerClientKind::Tui).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_desktop_save_is_checked_against_its_own_read() {
    a_save_is_checked_against_its_own_read(OwnerClientKind::Desktop).await;
}

/// A save the form names its earlier read for, with `begin_mutation_on`, lands after an
/// unrelated change even though the controller read a newer revision of another query since:
/// the resend re-reads the form's query and compares the reply the form showed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_save_on_its_earlier_read_lands_after_an_unrelated_change() {
    let (_engine, mut form, mut other) = windows(OwnerClientKind::Desktop).await;
    let shown = form.read(OwnerRead::QuitSettings).await.unwrap();
    change_another_setting(&mut other, 30).await;
    let newer = form.read(OwnerRead::CleanupSettings).await.unwrap();
    assert!(newer.revision.value() > shown.revision.value());
    form.begin_mutation_on(
        OwnerMutation::SetQuitSettings { settings: MINE },
        1,
        (OwnerRead::QuitSettings, shown),
    )
    .unwrap();
    let first = form.pending().unwrap().request().clone();
    assert_eq!(first.expected_revision, newer.revision);
    change_another_setting(&mut other, 31).await;
    let saved = form.send_pending().await.unwrap();
    let OwnerResponse::Applied { operation } = saved else {
        panic!("the form's save must land after one resend: {saved:?}");
    };
    assert_eq!(operation.result, OwnerResult::Applied);
    assert_ne!(operation.provenance.operation_id, first.request_id);
    assert_eq!(
        form.resent().map(|(id, _)| id),
        Some(&operation.provenance.operation_id)
    );
    assert_eq!(other.controller.quit_settings().await.unwrap(), MINE);
}

/// Guard: a read newer than the form's fresh view, here the other window's, cannot be named
/// as the read a change was built from; nothing is retained and nothing is sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_save_on_a_read_newer_than_the_view_is_refused_before_sending() {
    let (_engine, mut form, mut other) = windows(OwnerClientKind::Tui).await;
    let defaults = form.read(OwnerRead::QuitSettings).await.unwrap();
    change_another_setting(&mut other, 30).await;
    let newer = other
        .controller
        .read(OwnerRead::QuitSettings)
        .await
        .unwrap();
    assert!(newer.revision.value() > defaults.revision.value());
    let code = form
        .begin_mutation_on(
            OwnerMutation::SetQuitSettings { settings: MINE },
            1,
            (OwnerRead::QuitSettings, newer),
        )
        .unwrap_err()
        .0;
    assert_eq!(code, "controller_stale_view");
    assert!(form.pending().is_none());
    assert_ne!(other.controller.quit_settings().await.unwrap(), MINE);
}
