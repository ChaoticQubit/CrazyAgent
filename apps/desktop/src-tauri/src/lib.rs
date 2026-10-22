pub mod bridge;

#[cfg(feature = "native-webdriver")]
mod native_refresh_observation;

use bridge::{
    AccountProfileView, ActionReceipt, BaselineDetail, BaselinePage, BridgeError,
    ConfiguredAccountPlan, ConnectionStatus, HistoryPage, IntentReceipt, ManagedAccountPlan,
    ModelAccessDetail, ModelAccessPage, NativeAuthView, NativeBridge, PendingIntent, ProjectDetail,
    ProjectPage,
};
use crazyagent::config::ModelAccessEdit;
use crazyagent::engine::owner::{
    AccountAuthCapabilitiesReply, AccountSecret, ModelAccessCollection, NativeAuthMethod,
};
use crazyagent::protocol::ProjectSpec;
use std::sync::Arc;
use tauri::{State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tokio::sync::{Mutex, MutexGuard};

pub struct NativeState(Arc<Mutex<NativeBridge>>);

fn main_window(window: &WebviewWindow) -> Result<(), BridgeError> {
    if window.label() != "main" {
        return Err(BridgeError {
            code: "window_denied",
        });
    }
    Ok(())
}

fn lock(state: &NativeState) -> Result<MutexGuard<'_, NativeBridge>, BridgeError> {
    state.0.try_lock().map_err(|_| BridgeError { code: "busy" })
}

#[tauri::command]
async fn desktop_connect(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<ConnectionStatus, BridgeError> {
    main_window(&window)?;
    lock(&state)?.connect().await
}

#[tauri::command]
fn desktop_status(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<ConnectionStatus, BridgeError> {
    main_window(&window)?;
    Ok(lock(&state)?.status())
}

#[tauri::command]
async fn desktop_baselines(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    next: bool,
) -> Result<BaselinePage, BridgeError> {
    main_window(&window)?;
    lock(&state)?.baselines(next).await
}

#[tauri::command]
async fn desktop_inspect_baseline(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<BaselineDetail, BridgeError> {
    main_window(&window)?;
    lock(&state)?.inspect(&id).await
}

#[tauri::command]
async fn desktop_approve_baseline(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<ActionReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)?.approve(&ticket).await
}

#[tauri::command]
async fn desktop_revoke_baseline(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<ActionReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)?.revoke(&ticket).await
}

#[tauri::command]
async fn desktop_reconcile(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<ActionReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)?.reconcile().await
}

#[tauri::command]
async fn desktop_retry(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<ActionReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)?.retry().await
}

#[tauri::command]
async fn desktop_projects(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    next: bool,
) -> Result<ProjectPage, BridgeError> {
    main_window(&window)?;
    lock(&state)?.projects(next).await
}

#[tauri::command]
async fn desktop_create_project(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    spec: ProjectSpec,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)?.create_project(spec, generation).await
}

#[tauri::command]
async fn desktop_open_project(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<ProjectDetail, BridgeError> {
    main_window(&window)?;
    lock(&state)?.open_project(&id).await
}

#[tauri::command]
async fn desktop_history(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    next: bool,
) -> Result<HistoryPage, BridgeError> {
    main_window(&window)?;
    lock(&state)?.history(next).await
}

#[tauri::command]
async fn desktop_send_message(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
    text: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)?.send_message(&ticket, &text, generation).await
}

#[tauri::command]
async fn desktop_replace_brief(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
    text: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)?
        .replace_brief(&ticket, &text, generation)
        .await
}

#[tauri::command]
fn desktop_pending_intent(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<Option<PendingIntent>, BridgeError> {
    main_window(&window)?;
    lock(&state)?.pending_intent()
}

#[tauri::command]
async fn desktop_reconcile_intent(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)?.reconcile_intent().await
}

#[tauri::command]
async fn desktop_retry_intent(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)?.retry_intent().await
}

#[tauri::command]
async fn desktop_prepare_managed_account(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    provider: String,
    label: String,
) -> Result<ManagedAccountPlan, BridgeError> {
    main_window(&window)?;
    lock(&state)?
        .prepare_managed_account(&provider, &label)
        .await
}

#[tauri::command]
async fn desktop_prepare_configured_account(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    provider: String,
    label: String,
    endpoint: String,
    model: String,
) -> Result<ConfiguredAccountPlan, BridgeError> {
    main_window(&window)?;
    lock(&state)?
        .prepare_configured_account(&provider, &label, &endpoint, &model)
        .await
}

#[tauri::command]
async fn desktop_account_auth_capabilities(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<AccountAuthCapabilitiesReply, BridgeError> {
    main_window(&window)?;
    lock(&state)?.account_auth_capabilities(&ticket).await
}

#[tauri::command]
async fn desktop_configure_account_profile(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
    secret: Option<String>,
    generation: u64,
) -> Result<AccountProfileView, BridgeError> {
    main_window(&window)?;
    let secret = secret
        .map(|value| AccountSecret::new(value.into_bytes()))
        .transpose()
        .map_err(|_| BridgeError {
            code: "account_secret_invalid",
        })?;
    let mut bridge = Arc::clone(&state.0)
        .try_lock_owned()
        .map_err(|_| BridgeError { code: "busy" })?;
    tauri::async_runtime::spawn(async move {
        bridge
            .configure_account_profile(&ticket, secret, generation)
            .await
    })
    .await
    .map_err(|_| BridgeError {
        code: "account_profile_outcome_unknown_read_same_request",
    })?
}

#[tauri::command]
async fn desktop_read_account_profile(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<AccountProfileView, BridgeError> {
    main_window(&window)?;
    lock(&state)?.read_account_profile().await
}

#[tauri::command]
async fn desktop_model_access_page(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    collection: ModelAccessCollection,
    next: bool,
) -> Result<ModelAccessPage, BridgeError> {
    main_window(&window)?;
    lock(&state)?.model_access_page(collection, next).await
}

#[tauri::command]
async fn desktop_inspect_model_access(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    collection: ModelAccessCollection,
    id: String,
) -> Result<ModelAccessDetail, BridgeError> {
    main_window(&window)?;
    lock(&state)?.inspect_model_access(collection, &id).await
}

#[tauri::command]
async fn desktop_edit_model_access(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
    edit: ModelAccessEdit,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)?
        .edit_model_access(&ticket, edit, generation)
        .await
}

#[tauri::command]
async fn desktop_start_native_auth(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
    method: NativeAuthMethod,
) -> Result<NativeAuthView, BridgeError> {
    main_window(&window)?;
    lock(&state)?.start_native_auth(&ticket, method).await
}

#[tauri::command]
async fn desktop_read_native_auth(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<NativeAuthView, BridgeError> {
    main_window(&window)?;
    lock(&state)?.read_native_auth(&ticket).await
}

#[tauri::command]
async fn desktop_cancel_native_auth(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<NativeAuthView, BridgeError> {
    main_window(&window)?;
    lock(&state)?.cancel_native_auth(&ticket).await
}

#[tauri::command]
async fn desktop_submit_claude_auth_code(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
    code: String,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    let secret = NativeBridge::claude_auth_secret(code)?;
    let mut bridge = Arc::clone(&state.0)
        .try_lock_owned()
        .map_err(|_| BridgeError { code: "busy" })?;
    tauri::async_runtime::spawn(async move {
        bridge.submit_claude_auth_code(&ticket, secret).await
    })
    .await
    .map_err(|_| BridgeError { code: "native_auth_claude_input_outcome_unknown" })?
}

#[tauri::command]
async fn desktop_logout_native_auth(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<NativeAuthView, BridgeError> {
    main_window(&window)?;
    lock(&state)?.logout_native_auth(&ticket).await
}

pub fn run() {
    let socket = std::env::var_os("CRAZYAGENT_DESKTOP_SOCKET").map(Into::into);
    let builder = tauri::Builder::default()
        .manage(NativeState(Arc::new(Mutex::new(NativeBridge::new(socket)))))
        .invoke_handler(tauri::generate_handler![
            desktop_connect,
            desktop_status,
            desktop_baselines,
            desktop_inspect_baseline,
            desktop_approve_baseline,
            desktop_revoke_baseline,
            desktop_reconcile,
            desktop_retry,
            desktop_projects,
            desktop_create_project,
            desktop_open_project,
            desktop_history,
            desktop_send_message,
            desktop_replace_brief,
            desktop_pending_intent,
            desktop_reconcile_intent,
            desktop_retry_intent,
            desktop_prepare_managed_account,
            desktop_prepare_configured_account,
            desktop_account_auth_capabilities,
            desktop_configure_account_profile,
            desktop_read_account_profile,
            desktop_model_access_page,
            desktop_inspect_model_access,
            desktop_edit_model_access,
            desktop_start_native_auth,
            desktop_read_native_auth,
            desktop_cancel_native_auth,
            desktop_logout_native_auth,
            desktop_submit_claude_auth_code,
        ])
        .setup(|app| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("CrazyAgent")
                .inner_size(1180.0, 800.0)
                .min_inner_size(760.0, 560.0)
                .on_navigation(|url| {
                    (url.scheme() == "tauri" && url.host_str() == Some("localhost"))
                        || (url.scheme() == "http" && url.host_str() == Some("tauri.localhost"))
                        || (cfg!(debug_assertions)
                            && url.scheme() == "http"
                            && url.host_str() == Some("127.0.0.1")
                            && url.port() == Some(1420))
                })
                .build()?;
            Ok(())
        });
    #[cfg(feature = "native-webdriver")]
    let builder = {
        let observer = native_refresh_observation::NativeObserver::from_environment();
        builder
            .on_page_load(move |webview, payload| {
                if let Some(observer) = &observer {
                    let phase = match payload.event() {
                        tauri::webview::PageLoadEvent::Started => {
                            native_refresh_observation::Phase::Started
                        }
                        tauri::webview::PageLoadEvent::Finished => {
                            native_refresh_observation::Phase::Finished
                        }
                    };
                    observer.observe(webview.label(), payload.url().as_str(), phase);
                }
            })
            .plugin(tauri_plugin_wdio_webdriver::init())
    };
    builder
        .run(tauri::generate_context!())
        .expect("native desktop exited with an error");
}
