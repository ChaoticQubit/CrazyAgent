use crate::bridge::{BridgeError, IntentReceipt};
use crate::{NativeState, SOCKET_OVERRIDE, engine_failure, lock, main_window, restore};
use crazyagent::restore::RestoreState;
pub use crazyagent::restore::service_running;
use std::path::Path;
use std::time::SystemTime;
use tauri::{State, Webview};

/// What Settings shows about bringing back the 2.4 data (L5): until when the pinned backup
/// can be restored, in milliseconds since 1970, and how the last restore ended. Empty when
/// there is nothing to restore and no restore was asked.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct RestoreView {
    pub until_ms: Option<u64>,
    pub last: Option<RestoreState>,
}

/// Reads the view for the engine at `root` without the engine, from its backups folder and
/// its restore record. Input: the engine root. Output: the view.
pub fn view(root: &Path) -> RestoreView {
    assert!(root.is_absolute());
    let until_ms = crazyagent::restore::restorable_until(root).and_then(epoch_ms);
    let view = RestoreView {
        until_ms,
        last: crazyagent::restore::state(root),
    };
    assert!(view.until_ms.is_none_or(|until| until > 0));
    view
}

/// What the recovery screen shows for a restore that did not finish: the offer, none once
/// nothing needs recovery, and the backups folder it names.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct RecoveryView {
    pub offer: Option<crazyagent::restore::Recovery>,
    pub folder: String,
}

/// Reads the recovery offer of the engine at `root`, writing nothing. Inputs: the root and
/// whether its login service still runs a process. Output: the view, or the refusal's code.
pub fn recovery(root: &Path, running: bool) -> Result<RecoveryView, &'static str> {
    assert!(root.is_absolute());
    let offer = crazyagent::restore::recovery(root, running)
        .map_err(|text| crazyagent::restore::recovery_code(&text))?;
    let folder = crazyagent::restore::backups(root).display().to_string();
    Ok(RecoveryView { offer, folder })
}

/// Runs one recovery action on the engine at `root`: `put_back`, `continue` or `finish`.
/// Inputs: the root, whether its login service still runs a process, the action and the
/// settings files. Output: Ok, or the refusal's code.
pub fn recover(
    root: &Path,
    running: bool,
    action: &str,
    settings: (&Path, &Path),
) -> Result<(), &'static str> {
    assert!(root.is_absolute());
    let done = match action {
        "put_back" => crazyagent::restore::put_back(root, running),
        "continue" => crazyagent::restore::continue_2_5(root, running),
        "finish" => crazyagent::restore::finish(root, running, settings.0, settings.1),
        _ => return Err("storage_restore_unavailable"),
    };
    done.map_err(|text| crazyagent::restore::recovery_code(&text))
}

/// The default engine a recovery call works on and whether its login service still runs a
/// process. Refused `storage_restore_unavailable` while `CRAZYAGENT_DESKTOP_SOCKET` names
/// another engine. Input: none. Output: the layout.
pub fn engine() -> Result<(crazyagent::paths::Layout, bool), crate::BridgeError> {
    if std::env::var_os(crate::SOCKET_OVERRIDE).is_some_and(|socket| !socket.is_empty()) {
        return Err(crate::engine_failure("storage_restore_unavailable"));
    }
    let (layout, service) = crate::default_engine()?;
    let running = service_running(&service);
    Ok((layout, running))
}

/// Runs a recovery call off the async thread, since each checks engine.db. Input: the call.
/// Output: its result, a refusal as its code.
pub async fn blocking<T: Send + 'static>(
    call: impl FnOnce() -> Result<T, &'static str> + Send + 'static,
) -> Result<T, crate::BridgeError> {
    tokio::task::spawn_blocking(call)
        .await
        .map_err(|_| crate::engine_failure("storage_restore_failed"))?
        .map_err(crate::engine_failure)
}

/// A time as milliseconds since 1970. Input: the time. Output: the count, or none before
/// 1970 or past what a window number holds exactly.
fn epoch_ms(time: SystemTime) -> Option<u64> {
    let ms = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_millis();
    u64::try_from(ms)
        .ok()
        .filter(|ms| *ms > 0 && *ms <= 9_007_199_254_740_991)
}

/// Reads what Settings shows about bringing back the default engine's 2.4 data (L5), without
/// the engine: until when its pinned backup can be restored and how the last restore ended.
/// Nothing to show while `CRAZYAGENT_DESKTOP_SOCKET` names another engine.
#[tauri::command]
pub(crate) async fn desktop_restore_status(
    webview: Webview,
) -> Result<restore::RestoreView, BridgeError> {
    main_window(&webview)?;
    if std::env::var_os(SOCKET_OVERRIDE).is_some_and(|socket| !socket.is_empty()) {
        return Ok(restore::RestoreView::default());
    }
    let layout =
        crazyagent::paths::Layout::current().map_err(|_| engine_failure("not_configured"))?;
    Ok(restore::view(layout.root()))
}

/// Asks the default engine to stop and bring back its pinned 2.4 backup after the owner's
/// confirm (L5). Refused `storage_restore_unavailable` while `CRAZYAGENT_DESKTOP_SOCKET` names
/// another engine, whose restore this window could not follow. The bridge is held only for the
/// admit round trip, bounded by the client timeout; the copy runs after the engine stops.
#[tauri::command]
pub(crate) async fn desktop_restore_pinned(
    webview: Webview,
    state: State<'_, NativeState>,
    generation: u64,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    if std::env::var_os(SOCKET_OVERRIDE).is_some_and(|socket| !socket.is_empty()) {
        return Err(engine_failure("storage_restore_unavailable"));
    }
    lock(&state).await?.restore_pinned(generation).await
}

/// Reads what the recovery screen offers for the default engine's restore that did not
/// finish, writing nothing (L5).
#[tauri::command]
pub(crate) async fn desktop_restore_recovery(
    webview: Webview,
) -> Result<restore::RecoveryView, BridgeError> {
    main_window(&webview)?;
    let (layout, running) = restore::engine()?;
    restore::blocking(move || restore::recovery(layout.root(), running)).await
}

/// Runs one recovery action on the default engine: `put_back`, `continue` or `finish` (L5).
#[tauri::command]
pub(crate) async fn desktop_restore_recover(
    webview: Webview,
    action: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let (layout, running) = restore::engine()?;
    let settings = (layout.global_settings(), layout.project_settings());
    restore::blocking(move || {
        restore::recover(layout.root(), running, &action, (&settings.0, &settings.1))
    })
    .await
}

/// Opens the default engine's backups folder, which keeps the copies a restore left (L5).
#[tauri::command]
pub(crate) async fn desktop_restore_open_backups(webview: Webview) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let (layout, _) = restore::engine()?;
    let folder = crazyagent::restore::backups(layout.root());
    restore::blocking(move || crazyagent::platform::reveal(&folder)).await
}

#[tauri::command]
pub(crate) async fn desktop_log_compaction(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<crazyagent::log_compaction::LogCompactionView, BridgeError> {
    main_window(&webview)?;
    state.0.log_compaction().await
}

#[tauri::command]
pub(crate) async fn desktop_set_log_compaction(
    webview: Webview,
    state: State<'_, NativeState>,
    choice: crazyagent::log_compaction::LogCompaction,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .set_log_compaction(choice, generation)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_deadline_reaches_the_window_as_exact_milliseconds_and_nothing_else_does() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_millis(1_800_000_000_123);
        assert_eq!(epoch_ms(at), Some(1_800_000_000_123));
        assert_eq!(epoch_ms(SystemTime::UNIX_EPOCH), None);
        let before = SystemTime::UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(epoch_ms(before), None);
        let far = SystemTime::UNIX_EPOCH + Duration::from_millis(9_007_199_254_740_992);
        assert_eq!(epoch_ms(far), None);
    }

    #[test]
    fn an_unknown_recovery_action_is_refused_and_a_clean_engine_needs_no_recovery() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let settings = (root.join("global.toml"), root.join("project.toml"));
        let settings = (settings.0.as_path(), settings.1.as_path());
        assert_eq!(
            recover(&root, false, "delete", settings),
            Err("storage_restore_unavailable")
        );
        assert_eq!(
            recover(&root, true, "put_back", settings),
            Err("storage_putback_busy")
        );
        std::fs::write(root.join("engine.db"), b"").unwrap();
        let view = recovery(&root, false).unwrap();
        assert_eq!(view.offer, None);
        assert_eq!(view.folder, root.join("backups").display().to_string());
    }

    #[test]
    fn an_engine_folder_with_no_backup_and_no_record_shows_nothing_to_restore() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        assert_eq!(view(&root), RestoreView::default());
        let shown = serde_json::to_value(view(&root)).unwrap();
        assert_eq!(shown, serde_json::json!({ "until_ms": null, "last": null }));
    }
}
