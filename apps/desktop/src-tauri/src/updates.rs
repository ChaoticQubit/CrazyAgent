use crate::bridge::BridgeError;
use crazyagent::update::{self, Channel, UpdateError};
use serde::Serialize;
use std::path::PathBuf;
use std::time::Duration;
use tauri::ipc::Channel as Events;
use tauri::{AppHandle, Runtime};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::{Mutex, Notify};

const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
const PROGRESS_STEP_BYTES: u64 = 512 * 1024;
const NOTES_CHARS_MAX: usize = 4000;

/// The update the owner last checked for, kept until they install it or check again.
#[derive(Default)]
pub struct UpdateState(Mutex<Option<Update>>);

/// What Settings > Updates shows before any check: this app's version and the saved channel.
#[derive(Debug, Serialize)]
pub struct UpdateSettings {
    pub current_version: String,
    pub channel: &'static str,
}

/// A newer release found on the owner's channel.
#[derive(Debug, Serialize)]
pub struct AvailableUpdate {
    pub version: String,
    pub current_version: String,
    pub notes: Option<String>,
}

/// One step of Download and install, sent to the window as it happens.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpdateProgress {
    Downloading { downloaded: u64, total: Option<u64> },
    Verifying,
    StoppingEngine,
    Installing,
}

/// The result of an install: whether the engine service came back on the new version
/// before the restart.
#[derive(Debug, Serialize)]
pub struct InstallOutcome {
    pub engine_started: bool,
}

fn refused(code: &'static str) -> BridgeError {
    BridgeError { code }
}

fn bridged(error: UpdateError) -> BridgeError {
    refused(error.code)
}

fn root() -> Result<PathBuf, BridgeError> {
    update::data_root().ok_or_else(|| refused("update_no_data_root"))
}

/// Reads this app's version and the saved channel. Input: the app. Output: the settings, or
/// `update_channel_invalid` / `update_no_data_root`.
pub fn settings<R: Runtime>(app: &AppHandle<R>) -> Result<UpdateSettings, BridgeError> {
    let channel = update::read_channel(&root()?).map_err(bridged)?;
    Ok(UpdateSettings {
        current_version: app.package_info().version.to_string(),
        channel: channel.as_str(),
    })
}

/// Saves the owner's channel in `<root>/update.toml`, shared with `crazyagent update`, and
/// forgets an update found on the old channel. Inputs: the app, the state and "stable" or
/// "dev". Output: the new settings.
pub fn set_channel<R: Runtime>(
    app: &AppHandle<R>,
    state: &UpdateState,
    channel: &str,
) -> Result<UpdateSettings, BridgeError> {
    let channel = Channel::parse(channel).ok_or_else(|| refused("update_channel_invalid"))?;
    let mut found = state.0.try_lock().map_err(|_| refused("busy"))?;
    update::save_channel(&root()?, channel).map_err(bridged)?;
    *found = None;
    settings(app)
}

/// Checks the saved channel's signed manifest for a newer release and keeps what it finds
/// for Download and install. Inputs: the app and the state. Output: the newer release, None
/// when this app is current, or `update_check_failed`.
pub async fn check<R: Runtime>(
    app: &AppHandle<R>,
    state: &UpdateState,
) -> Result<Option<AvailableUpdate>, BridgeError> {
    let mut found = state.0.try_lock().map_err(|_| refused("busy"))?;
    let channel = update::read_channel(&root()?).map_err(bridged)?;
    let endpoint = channel
        .desktop_manifest_url()
        .parse()
        .map_err(|_| refused("update_check_failed"))?;
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .and_then(|builder| builder.timeout(CHECK_TIMEOUT).build())
        .map_err(|_| refused("update_check_failed"))?;
    *found = updater
        .check()
        .await
        .map_err(|_| refused("update_check_failed"))?;
    Ok(found.as_ref().map(|update| AvailableUpdate {
        version: update.version.clone(),
        current_version: update.current_version.clone(),
        notes: update
            .body
            .as_ref()
            .map(|notes| notes.chars().take(NOTES_CHARS_MAX).collect()),
    }))
}

fn sidecar() -> Result<PathBuf, BridgeError> {
    std::env::current_exe()
        .ok()
        .and_then(|executable| {
            executable
                .parent()
                .map(|folder| folder.join(format!("crazyagent{}", std::env::consts::EXE_SUFFIX)))
        })
        .filter(|path| path.is_file())
        .ok_or_else(|| refused("sidecar_missing"))
}

async fn service(sidecar: PathBuf, action: &'static str) -> Result<String, BridgeError> {
    tauri::async_runtime::spawn_blocking(move || update::service_command(&sidecar, action))
        .await
        .map_err(|_| refused("update_service_failed"))?
        .map_err(bridged)
}

fn download_failure(error: tauri_plugin_updater::Error) -> BridgeError {
    use tauri_plugin_updater::Error;
    match error {
        Error::Minisign(_)
        | Error::Base64(_)
        | Error::SignatureUtf8(_)
        | Error::SignedVersionMismatch { .. }
        | Error::MissingSignedVersion => refused("update_signature_invalid"),
        _ => refused("update_download_failed"),
    }
}

/// Downloads the checked release and verifies its signature, then stops the engine service
/// through the bundled `crazyagent`, installs, and starts the service on the new engine,
/// which migrates its data on start. Nothing changes when the download or signature fails;
/// a failed install starts the old engine again. Inputs: the state and the progress channel.
/// Output: whether the engine started, or the failure code.
pub async fn install(
    state: &UpdateState,
    events: Events<UpdateProgress>,
) -> Result<InstallOutcome, BridgeError> {
    let mut found = state.0.try_lock().map_err(|_| refused("busy"))?;
    let update = found
        .as_mut()
        .ok_or_else(|| refused("update_not_checked"))?;
    let sidecar = sidecar()?;
    update.timeout = Some(DOWNLOAD_TIMEOUT);
    let too_large = Notify::new();
    let mut downloaded: u64 = 0;
    let mut reported: u64 = 0;
    let download = update.download(
        |chunk, total| {
            downloaded = downloaded.saturating_add(chunk as u64);
            if downloaded > update::DOWNLOAD_BYTES_MAX
                || total.is_some_and(|total| total > update::DOWNLOAD_BYTES_MAX)
            {
                too_large.notify_one();
            }
            if downloaded - reported >= PROGRESS_STEP_BYTES || total == Some(downloaded) {
                reported = downloaded;
                let _ = events.send(UpdateProgress::Downloading { downloaded, total });
            }
        },
        || {
            let _ = events.send(UpdateProgress::Verifying);
        },
    );
    let bytes = tokio::select! {
        result = download => result.map_err(download_failure)?,
        () = too_large.notified() => return Err(refused("update_too_large")),
    };
    let _ = events.send(UpdateProgress::StoppingEngine);
    service(sidecar.clone(), "stop").await?;
    let _ = events.send(UpdateProgress::Installing);
    if update.install(&bytes).is_err() {
        let _ = service(sidecar, "start").await;
        return Err(refused("update_install_failed"));
    }
    Ok(InstallOutcome {
        engine_started: service(sidecar, "start").await.is_ok(),
    })
}

/// Links the bundled `crazyagent` onto the owner's PATH through
/// `crazyagent service link-cli`. Output: the tool's own one-line report.
pub async fn link_cli() -> Result<String, BridgeError> {
    service(sidecar()?, "link-cli").await
}
