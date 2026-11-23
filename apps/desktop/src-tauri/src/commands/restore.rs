/// Reads what Settings shows about bringing back the default engine's 2.4 data (L5), without
/// the engine: until when its pinned backup can be restored and how the last restore ended.
/// Nothing to show while `CRAZYAGENT_DESKTOP_SOCKET` names another engine.
#[tauri::command]
async fn desktop_restore_status(webview: Webview) -> Result<restore::RestoreView, BridgeError> {
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
/// another engine, whose restore this window could not follow.
#[tauri::command]
async fn desktop_restore_pinned(
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
async fn desktop_restore_recovery(webview: Webview) -> Result<restore::RecoveryView, BridgeError> {
    main_window(&webview)?;
    let (layout, running) = restore::engine()?;
    restore::blocking(move || restore::recovery(layout.root(), running)).await
}

/// Runs one recovery action on the default engine: `put_back`, `continue` or `finish` (L5).
#[tauri::command]
async fn desktop_restore_recover(webview: Webview, action: String) -> Result<(), BridgeError> {
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
async fn desktop_restore_open_backups(webview: Webview) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let (layout, _) = restore::engine()?;
    let folder = crazyagent::restore::backups(layout.root());
    restore::blocking(move || crazyagent::platform::reveal(&folder)).await
}

#[tauri::command]
async fn desktop_log_compaction(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<crazyagent::log_compaction::LogCompactionView, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.log_compaction().await
}

#[tauri::command]
async fn desktop_set_log_compaction(
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
