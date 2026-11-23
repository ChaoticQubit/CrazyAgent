use crate::bridge::BridgeError;
use crazyagent::service::StartWait;
use kernel_processes::update::{self, Channel, UpdateError};
use serde::Serialize;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tauri::ipc::Channel as Events;
use tauri::{AppHandle, Runtime};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::{Mutex, MutexGuard, Notify};

const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
const PROGRESS_STEP_BYTES: u64 = 512 * 1024;
const STARTING_TICK: Duration = Duration::from_secs(5);
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
    StartingEngine { waited_s: u64, answering: bool },
}

/// The result of an install: where the engine service stands on the new version before
/// the restart.
#[derive(Debug, Serialize)]
pub struct InstallOutcome {
    pub engine: EngineAfterUpdate,
}

/// Where the engine stands when an install returns: answering on the new version, still
/// starting under its service manager past the wait, or not running at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineAfterUpdate {
    Ready,
    Starting,
    Failed,
}

/// Names where the engine stands after an install's wait. Input: how the wait ended, none
/// when it failed. Output: Ready when it answered, Starting when it ran out while the
/// service manager still ran its first process, else Failed.
fn engine_after(waited: Option<StartWait>) -> EngineAfterUpdate {
    match waited {
        Some(StartWait::Answered) => EngineAfterUpdate::Ready,
        Some(StartWait::StillStarting) => EngineAfterUpdate::Starting,
        None => EngineAfterUpdate::Failed,
    }
}

fn refused(code: &'static str) -> BridgeError {
    BridgeError {
        code,
        cause: None,
        account: None,
    }
}

fn bridged(error: UpdateError) -> BridgeError {
    refused(error.code)
}

/// Takes the update lock without waiting, so a check, a channel switch, an install and an
/// engine restart never overlap: an install stops and replaces the engine a restart would
/// start. Input: the state. Output: the guard to keep for the whole step, or `busy`.
pub fn hold(state: &UpdateState) -> Result<MutexGuard<'_, Option<Update>>, BridgeError> {
    state.0.try_lock().map_err(|_| refused("busy"))
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
    let mut found = hold(state)?;
    crazyagent::update::save_channel(&root()?, channel).map_err(bridged)?;
    *found = None;
    settings(app)
}

/// Tells a channel with no release apart from a failing server, which the updater plugin both
/// reports as `ReleaseNotFound`, by asking for the channel's manifest once more with the
/// system curl. Input: the channel. Output: `update_<channel>_not_published` when the server
/// answers 404, else `update_check_failed`.
async fn unpublished(channel: Channel) -> BridgeError {
    let probe = tauri::async_runtime::spawn_blocking(move || {
        update::download(
            channel.desktop_manifest_url(),
            &mut std::io::sink(),
            update::MANIFEST_BYTES_MAX,
            CHECK_TIMEOUT,
        )
    });
    match probe.await {
        Ok(Err(error)) if error.code == "update_download_missing" => {
            bridged(update::not_published(channel))
        }
        _ => refused("update_check_failed"),
    }
}

/// Checks the saved channel's signed manifest for a newer release and keeps what it finds
/// for Download and install. Inputs: the app and the state. Output: the newer release, None
/// when this app is current, `update_<channel>_not_published` when the channel has no
/// release yet, or `update_check_failed`.
pub async fn check<R: Runtime>(
    app: &AppHandle<R>,
    state: &UpdateState,
) -> Result<Option<AvailableUpdate>, BridgeError> {
    let mut found = hold(state)?;
    let channel = update::read_channel(&root()?).map_err(bridged)?;
    let endpoint = channel
        .desktop_manifest_url()
        .parse()
        .map_err(|_| refused("update_check_failed"))?;
    check_at(app, &mut found, channel, endpoint).await
}

/// Checks one manifest endpoint for a newer release and keeps what it finds in the held
/// state, as `check` does for the channel's published endpoint. Inputs: the app, the held
/// update lock, the channel and the endpoint. Output: as `check`.
async fn check_at<R: Runtime>(
    app: &AppHandle<R>,
    found: &mut MutexGuard<'_, Option<Update>>,
    channel: Channel,
    endpoint: tauri::Url,
) -> Result<Option<AvailableUpdate>, BridgeError> {
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .and_then(|builder| builder.timeout(CHECK_TIMEOUT).build())
        .map_err(|_| refused("update_check_failed"))?;
    **found = match updater.check().await {
        Ok(found) => found,
        Err(tauri_plugin_updater::Error::ReleaseNotFound) => return Err(unpublished(channel).await),
        Err(_) => return Err(refused("update_check_failed")),
    };
    Ok((**found).as_ref().map(|update| AvailableUpdate {
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
/// Output: where the engine stands, or the failure code.
pub async fn install(
    state: &UpdateState,
    events: Events<UpdateProgress>,
) -> Result<InstallOutcome, BridgeError> {
    install_with(state, events, sidecar).await
}

/// Runs `install` with the bundled `crazyagent` found by `sidecar`, looked up after the lock
/// and the checked update, as `install` does. Inputs: the state, the progress channel and
/// the sidecar lookup. Output: as `install`.
async fn install_with(
    state: &UpdateState,
    events: Events<UpdateProgress>,
    sidecar: impl FnOnce() -> Result<PathBuf, BridgeError>,
) -> Result<InstallOutcome, BridgeError> {
    let mut found = hold(state)?;
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
        engine: start_engine(sidecar, &events).await,
    })
}

/// Starts the engine service through the new `crazyagent`, then waits for the engine as app
/// launch does, past that tool's own wait, while its service manager still runs the process
/// it started, telling the window the time waited every `STARTING_TICK`. Inputs: the new
/// sidecar and the progress channel. Output: where the engine stands.
async fn start_engine(sidecar: PathBuf, events: &Events<UpdateProgress>) -> EngineAfterUpdate {
    let watched = crazyagent::paths::Layout::current()
        .ok()
        .and_then(|layout| {
            crazyagent::service::Service::current_user(
                crazyagent::paths::Platform::CURRENT,
                sidecar.clone(),
                &layout,
            )
            .ok()
            .map(|engine| (layout, engine))
        });
    let Some((layout, engine)) = watched else {
        return engine_after(
            service(sidecar, "start")
                .await
                .is_ok()
                .then_some(StartWait::Answered),
        );
    };
    let since = crazyagent::service::log_length(&layout);
    let begun = Instant::now();
    let started = async {
        let _ = service(sidecar, "start").await;
        crazyagent::service::wait_start(&layout, &engine, crazyagent::service::READY_TIMEOUT, since)
            .await
            .ok()
    };
    tokio::pin!(started);
    let mut tick = tokio::time::interval(STARTING_TICK);
    let waited = loop {
        tokio::select! {
            waited = &mut started => break waited,
            _ = tick.tick() => {
                let _ = events.send(UpdateProgress::StartingEngine {
                    waited_s: begun.elapsed().as_secs(),
                    answering: crazyagent::service::answering(&layout.socket()),
                });
            }
        }
    };
    engine_after(waited)
}

/// Links the bundled `crazyagent`, or its private copy when the app runs from an AppImage
/// mount, onto the owner's PATH through `crazyagent service link-cli`. Output: the tool's
/// own one-line report, or `update_link_cli_failed` / `sidecar_missing`.
pub async fn link_cli() -> Result<String, BridgeError> {
    service(sidecar()?, "link-cli")
        .await
        .map_err(|_| refused("update_link_cli_failed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use std::io::{Read, Write};

    fn key_pair() -> (minisign::KeyPair, String) {
        let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let key = STANDARD.encode(pair.pk.to_box().unwrap().into_string());
        (pair, key)
    }

    fn signed(pair: &minisign::KeyPair, bytes: &[u8], trusted: &str) -> String {
        let signature = minisign::sign(
            Some(&pair.pk),
            &pair.sk,
            std::io::Cursor::new(bytes),
            Some(trusted),
            None,
        )
        .unwrap();
        STANDARD.encode(signature.into_string())
    }

    fn answer(listener: &std::net::TcpListener, body: &[u8]) {
        let (mut stream, _) = kernel_processes::spawn_gate::accept_tcp_std(listener).unwrap();
        let mut request = Vec::new();
        let mut byte = [0_u8; 1];
        while !request.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
            request.push(byte[0]);
        }
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body);
    }

    /// The desktop app installs only a release the updater verified: a manifest and archive
    /// signed with a key other than the app's are found by the check, then refused at
    /// download with `update_signature_invalid`, and the sidecar never stops the engine.
    #[test]
    fn desktop_install_refuses_a_bad_signature_through_the_updater() {
        let (_release, release_key) = key_pair();
        let (other, _) = key_pair();
        let archive = b"desktop release archive".to_vec();
        let trusted = "timestamp:1761400000\tfile:CrazyAgent.app.tar.gz\tversion:0.2.0";
        let listener = kernel_processes::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let target = tauri_plugin_updater::target().expect("an updater target for this build");
        let mut platforms = serde_json::Map::new();
        platforms.insert(
            target,
            serde_json::json!({
                "url": format!("{base}/archive"),
                "signature": signed(&other, &archive, trusted),
            }),
        );
        let manifest = serde_json::json!({ "version": "0.2.0", "platforms": platforms });
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let served = archive.clone();
        let server = std::thread::spawn(move || {
            answer(&listener, &manifest);
            answer(&listener, &served);
        });

        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context.config_mut().plugins.0.insert(
            "updater".to_owned(),
            serde_json::json!({ "pubkey": release_key, "dangerousInsecureTransportProtocol": true }),
        );
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(context)
            .unwrap();
        let state = UpdateState::default();
        let folder = tempfile::tempdir().unwrap();
        let stub = folder.path().join("crazyagent");
        let ran = folder.path().join("service-ran");
        std::fs::write(&stub, format!("#!/bin/sh\ntouch '{}'\n", ran.display())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let outcome = tauri::async_runtime::block_on(async {
            let endpoint: tauri::Url = format!("{base}/latest.json").parse().unwrap();
            let mut found = hold(&state).unwrap();
            let available = check_at(app.handle(), &mut found, Channel::Stable, endpoint)
                .await
                .unwrap()
                .expect("the check finds the newer release");
            assert_eq!(available.version, "0.2.0");
            drop(found);
            let events = Events::new(|_| Ok(()));
            install_with(&state, events, || Ok(stub.clone())).await
        });
        server.join().unwrap();
        assert_eq!(
            outcome.err().map(|refusal| refusal.code),
            Some("update_signature_invalid")
        );
        assert!(!ran.exists(), "the sidecar ran a service step");
        assert_eq!(
            std::fs::read_dir(folder.path()).unwrap().count(),
            1,
            "nothing but the stub sidecar is in its folder"
        );
    }

    #[test]
    fn an_engine_restart_and_an_update_step_never_hold_the_update_lock_together() {
        let state = UpdateState::default();
        let restarting = hold(&state).expect("a free lock is taken");
        assert_eq!(hold(&state).err().map(|refusal| refusal.code), Some("busy"));
        drop(restarting);
        let installing = hold(&state).expect("a released lock is taken again");
        assert_eq!(hold(&state).err().map(|refusal| refusal.code), Some("busy"));
        drop(installing);
        assert!(hold(&state).is_ok());
    }

    #[test]
    fn an_engine_its_service_manager_still_runs_past_the_wait_reads_starting_not_failed() {
        assert_eq!(
            engine_after(Some(StartWait::Answered)),
            EngineAfterUpdate::Ready
        );
        assert_eq!(
            engine_after(Some(StartWait::StillStarting)),
            EngineAfterUpdate::Starting
        );
    }

    #[test]
    fn an_engine_that_stopped_or_was_relaunched_while_starting_reads_failed_not_starting() {
        assert_eq!(engine_after(None), EngineAfterUpdate::Failed);
    }

    #[test]
    fn the_install_outcome_and_the_starting_step_read_as_the_window_expects() {
        let outcome = InstallOutcome {
            engine: EngineAfterUpdate::Starting,
        };
        assert_eq!(
            serde_json::to_value(&outcome).unwrap(),
            serde_json::json!({ "engine": "starting" })
        );
        let step = UpdateProgress::StartingEngine {
            waited_s: 65,
            answering: true,
        };
        assert_eq!(
            serde_json::to_value(&step).unwrap(),
            serde_json::json!({ "kind": "starting_engine", "waited_s": 65, "answering": true })
        );
    }
}
