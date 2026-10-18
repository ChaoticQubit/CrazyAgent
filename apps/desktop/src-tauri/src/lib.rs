pub mod bridge;

use bridge::{
    ActionReceipt, BaselineDetail, BaselinePage, BridgeError, ConnectionStatus, NativeBridge,
};
use tauri::{State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tokio::sync::{Mutex, MutexGuard};

pub struct NativeState(Mutex<NativeBridge>);

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

pub fn run() {
    let socket = std::env::var_os("CRAZYAGENT_DESKTOP_SOCKET").map(Into::into);
    let builder = tauri::Builder::default()
        .manage(NativeState(Mutex::new(NativeBridge::new(socket))))
        .invoke_handler(tauri::generate_handler![
            desktop_connect,
            desktop_status,
            desktop_baselines,
            desktop_inspect_baseline,
            desktop_approve_baseline,
            desktop_revoke_baseline,
            desktop_reconcile,
            desktop_retry,
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
    let builder = builder.plugin(tauri_plugin_wdio_webdriver::init());
    builder
        .run(tauri::generate_context!())
        .expect("native desktop exited with an error");
}
