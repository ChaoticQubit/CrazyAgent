use super::{BridgeError, NativeState, lock, main_window};
use crazyagent::engine::owner::{AppPluginsPage, PluginGrants, PluginUser, Preview, TurnOff};
use tauri::{State, Webview};

#[tauri::command]
pub(crate) async fn desktop_app_plugins(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<AppPluginsPage, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.app_plugins()?;
    call.await
}

#[tauri::command]
pub(crate) async fn desktop_app_plugin_grants(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<Vec<PluginGrants>, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.app_plugin_grants().await
}

#[tauri::command]
pub(crate) async fn desktop_app_plugin_preview(
    webview: Webview,
    state: State<'_, NativeState>,
    folder: String,
) -> Result<Preview, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.app_plugin_preview(folder)?;
    call.await
}

#[tauri::command]
pub(crate) async fn desktop_app_plugin_review(
    webview: Webview,
    state: State<'_, NativeState>,
    name: String,
) -> Result<Preview, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.app_plugin_review(name)?;
    call.await
}

#[tauri::command]
pub(crate) async fn desktop_app_plugin_add(
    webview: Webview,
    state: State<'_, NativeState>,
    folder: String,
    sha256: String,
    replace: bool,
) -> Result<AppPluginsPage, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state)
        .await?
        .app_plugin_add(folder, sha256, replace)?;
    call.await
}

#[tauri::command]
pub(crate) async fn desktop_app_plugin_trust(
    webview: Webview,
    state: State<'_, NativeState>,
    name: String,
    sha256: String,
) -> Result<AppPluginsPage, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.app_plugin_trust(name, sha256)?;
    call.await
}

#[tauri::command]
pub(crate) async fn desktop_app_plugin_set_enabled(
    webview: Webview,
    state: State<'_, NativeState>,
    name: String,
    enabled: bool,
    when_off: Option<TurnOff>,
) -> Result<AppPluginsPage, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state)
        .await?
        .app_plugin_set_enabled(name, enabled, when_off)?;
    call.await
}

#[tauri::command]
pub(crate) async fn desktop_app_plugin_users(
    webview: Webview,
    state: State<'_, NativeState>,
    name: String,
) -> Result<Vec<PluginUser>, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.app_plugin_users(name)?;
    call.await
}

#[tauri::command]
pub(crate) async fn desktop_app_plugin_remove(
    webview: Webview,
    state: State<'_, NativeState>,
    name: String,
) -> Result<AppPluginsPage, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.app_plugin_remove(name)?;
    call.await
}

#[tauri::command]
pub(crate) async fn desktop_app_plugin_bind_account(
    webview: Webview,
    state: State<'_, NativeState>,
    name: String,
    secret: String,
    account: String,
) -> Result<AppPluginsPage, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state)
        .await?
        .app_plugin_bind_account(name, secret, account)?;
    call.await
}

#[tauri::command]
pub(crate) async fn desktop_app_plugin_set_key(
    webview: Webview,
    state: State<'_, NativeState>,
    name: String,
    secret: String,
    key: String,
) -> Result<AppPluginsPage, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.app_plugin_set_key(name, secret, key)?;
    call.await
}

#[tauri::command]
pub(crate) async fn desktop_app_plugin_unbind(
    webview: Webview,
    state: State<'_, NativeState>,
    name: String,
    secret: String,
) -> Result<AppPluginsPage, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.app_plugin_unbind(name, secret)?;
    call.await
}

#[tauri::command]
pub(crate) async fn desktop_app_plugin_set_when_off(
    webview: Webview,
    state: State<'_, NativeState>,
    when_off: TurnOff,
) -> Result<AppPluginsPage, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.app_plugin_set_when_off(when_off)?;
    call.await
}
