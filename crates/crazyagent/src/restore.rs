use std::path::Path;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;
use tokio::sync::Notify;

/// The code a client gives for an engine it does not start while a restore runs.
pub const ENGINE_RESTORING: &str = "engine_restoring";
/// The code a client gives for an engine it does not start after a restore brought the 2.4
/// data back.
pub const ENGINE_RESTORED: &str = "engine_restored";

/// Set once this engine process admitted the owner's restore of the pinned 2.4 backup. Only
/// this flag, never a file, makes the process restore as it ends.
static REQUESTED: AtomicBool = AtomicBool::new(false);
/// Wakes the engine's stop once a restore is admitted.
static STOP: LazyLock<Notify> = LazyLock::new(Notify::new);

/// The name prefix of the dated copy a restore keeps of the data it replaces.
const COPY_PREFIX: &str = "replaced-";

/// How the owner's last restore of the pinned 2.4 backup stands, as the record beside
/// engine.db says.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RestoreState {
    Pending,
    Restored { copy: String },
    Failed { code: String },
}

/// Asks this engine to stop and bring back the pinned 2.4 backup once everything it runs has
/// stopped: records the restore pending first, so no client starts the engine again while
/// it copies, then sets the flag and wakes the stop. Input: the engine root. Output: Ok once
/// the record is on disk.
#[cfg(unix)]
pub(crate) fn request(root: &Path) -> Result<(), crate::engine::EngineError> {
    crate::storage::setup::restore_pending(root)?;
    REQUESTED.store(true, Ordering::SeqCst);
    STOP.notify_one();
    assert!(REQUESTED.load(Ordering::SeqCst));
    Ok(())
}

/// Waits until a restore asks this engine to stop. A request made before the wait counts.
/// Output: once asked.
pub async fn stop_asked() {
    STOP.notified().await;
}

/// Brings back the pinned 2.4 backup as the engine process ends, after the engine and every
/// service it started have stopped, when this process admitted a restore; otherwise does
/// nothing. Inputs: the engine root and the settings files. Output: Ok, or the failure, so
/// the login service brings 2.5 back on its unchanged data.
pub fn after_stop(root: &Path, global: &Path, project: &Path) -> Result<(), String> {
    after_stop_when(REQUESTED.load(Ordering::SeqCst), root, global, project)
}

/// [`after_stop`] with the request given. Inputs: whether a restore was admitted, the engine
/// root and the settings files. Output: Ok, or the failure.
fn after_stop_when(
    requested: bool,
    root: &Path,
    global: &Path,
    project: &Path,
) -> Result<(), String> {
    if !requested {
        return Ok(());
    }
    #[cfg(unix)]
    return crate::storage::setup::restore_after_stop(root, global, project)
        .map(|copy| assert!(copy.starts_with(root)))
        .map_err(|cause| cause.0);
    #[cfg(not(unix))]
    {
        let _ = (root, global, project);
        Err("storage_restore_unavailable".into())
    }
}

/// How the last restore of the engine at `root` stands, read without an engine. Only a
/// private file of this user holding one known line counts. Input: the engine root.
/// Output: the state, or none.
pub fn state(root: &Path) -> Option<RestoreState> {
    #[cfg(unix)]
    return parse(&crate::storage::setup::restore_record(root)?);
    #[cfg(not(unix))]
    {
        let _ = root;
        None
    }
}

/// Reads one record line: `pending`, `restored replaced-<time>` or
/// `failed storage_restore_<reason>`. Input: the line. Output: the state, or none.
#[cfg(unix)]
fn parse(line: &str) -> Option<RestoreState> {
    use crate::storage::setup::{RECORD_FAILED, RECORD_PENDING, RECORD_RESTORED};
    let mut words = line.split(' ');
    let state = match (words.next()?, words.next(), words.next()) {
        (word, None, None) if word == RECORD_PENDING => RestoreState::Pending,
        (word, Some(copy), None) if word == RECORD_RESTORED => {
            let named = copy.strip_prefix(COPY_PREFIX).is_some_and(|time| {
                !time.is_empty() && time.bytes().all(|b| b.is_ascii_alphanumeric())
            });
            if !named {
                return None;
            }
            RestoreState::Restored { copy: copy.into() }
        }
        (word, Some(code), None) if word == RECORD_FAILED => {
            let coded = code.starts_with("storage_restore_")
                && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
            if !coded {
                return None;
            }
            RestoreState::Failed { code: code.into() }
        }
        _ => return None,
    };
    assert!(line.len() <= 512);
    Some(state)
}

/// Why a client must not start the engine at `root` by itself: a restore runs, or brought
/// the 2.4 data back and one 2.5 start would upgrade it again. A pending record whose engine
/// process is gone was left by a restore that died, and the start that follows clears it.
/// Inputs: the engine root and whether its login service still runs a process. Output:
/// `engine_restoring`, `engine_restored` or none.
pub fn blocks_start(root: &Path, running: bool) -> Option<&'static str> {
    match state(root)? {
        RestoreState::Pending if running => Some(ENGINE_RESTORING),
        RestoreState::Pending => None,
        RestoreState::Restored { .. } => Some(ENGINE_RESTORED),
        RestoreState::Failed { .. } => None,
    }
}

/// Whether the owner's explicit restart of the engine at `root` must wait: a restore still
/// runs, and a restart would stop its process in the middle of the copy. After a restore it
/// never waits; that restart is the way back to 2.5. Inputs: the engine root and whether its
/// login service still runs a process. Output: the answer.
pub fn holds_restart(root: &Path, running: bool) -> bool {
    let held = blocks_start(root, running) == Some(ENGINE_RESTORING);
    assert!(!held || running);
    held
}

/// Whether the engine's login service still runs a process, as a restore's pending record is
/// weighed. A status that can't be read counts as running, so a restore that may still copy
/// is never stopped. Input: the service. Output: the answer.
pub fn service_running(service: &crate::service::Service) -> bool {
    service.status().map_or(true, |status| status.running)
}

/// Until when the owner can restore the pinned 2.4 backup of the engine at `root`, read
/// without an engine and creating nothing. Input: the engine root. Output: the pin's end, or
/// none.
pub fn restorable_until(root: &Path) -> Option<SystemTime> {
    #[cfg(unix)]
    return crate::storage::setup::restorable_until(root);
    #[cfg(not(unix))]
    {
        let _ = root;
        None
    }
}

#[cfg(all(test, unix))]
mod tests;
