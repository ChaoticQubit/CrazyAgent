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
/// The code a client gives for an engine it does not start because a restore did not finish:
/// its rollback failed, or it stopped part way with no process left. The client shows the
/// recovery screen in place of the start.
pub const ENGINE_RESTORE_BROKEN: &str = "engine_restore_broken";
/// The code every recovery action gives while the engine's login service still runs a
/// process.
pub const RECOVERY_BUSY: &str = "storage_putback_busy";

/// Set once this engine process admitted the owner's restore of the pinned 2.4 backup. Only
/// this flag, never a file, makes the process restore as it ends.
static REQUESTED: AtomicBool = AtomicBool::new(false);
/// Wakes the engine's stop once a restore is admitted.
static STOP: LazyLock<Notify> = LazyLock::new(Notify::new);

/// The name prefix of the dated copy a restore keeps of the data it replaces.
#[cfg(unix)]
const COPY_PREFIX: &str = "replaced-";

/// How the owner's last restore of the pinned 2.4 backup stands, as the record beside
/// engine.db says.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RestoreState {
    Pending,
    Restoring { copy: Option<String> },
    Restored { copy: String },
    Failed { code: String },
    Broken { copy: Option<String> },
}

/// What the recovery screen offers for a restore that did not finish: carry on with the
/// unchanged 2.5 data, finish a restore that wrote the 2.4 data, put the named copy back, or
/// nothing but the backups folder and the code.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Recovery {
    Continue,
    Finished { copy: String },
    PutBack { copy: String },
    Stuck { code: String },
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

/// Reads one record line: `pending`; `restoring` with the kept copy's name once there is
/// one; `restored replaced-<time>` with an optional `unverified_pin`;
/// `failed storage_restore_rollback_failed` with the copy's name; or
/// `failed storage_restore_<reason>`. A restoring or rollback-failed line reads as such
/// whatever follows, as the engine start refuses it, and anything but one copy's name
/// counts as no name. Input: the line. Output: the state, or none.
#[cfg(unix)]
fn parse(line: &str) -> Option<RestoreState> {
    use crate::storage::setup::{
        RECORD_FAILED, RECORD_PENDING, RECORD_RESTORED, RECORD_RESTORING, RECORD_UNVERIFIED_PIN,
    };
    use kernel_storage::restore::RESTORE_ROLLBACK_FAILED;
    let mut words = line.split(' ');
    let state = match (words.next()?, words.next(), words.next(), words.next()) {
        (word, None, None, None) if word == RECORD_PENDING => RestoreState::Pending,
        (word, copy, tail, _) if word == RECORD_RESTORING => RestoreState::Restoring {
            copy: named(copy, tail),
        },
        (word, Some(code), copy, tail)
            if word == RECORD_FAILED && code == RESTORE_ROLLBACK_FAILED =>
        {
            RestoreState::Broken {
                copy: named(copy, tail),
            }
        }
        (word, Some(copy), tail, None)
            if word == RECORD_RESTORED && tail.is_none_or(|tail| tail == RECORD_UNVERIFIED_PIN) =>
        {
            let named = copy.strip_prefix(COPY_PREFIX).is_some_and(|time| {
                !time.is_empty() && time.bytes().all(|b| b.is_ascii_alphanumeric())
            });
            if !named {
                return None;
            }
            RestoreState::Restored { copy: copy.into() }
        }
        (word, Some(code), None, None) if word == RECORD_FAILED => {
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

/// The kept copy's name a record line ends with: one word, `replaced-` and a time of letters
/// and digits, with nothing after it. Inputs: the word and what follows it. Output: the
/// name, or none.
#[cfg(unix)]
fn named(copy: Option<&str>, tail: Option<&str>) -> Option<String> {
    let copy = copy.filter(|_| tail.is_none())?;
    let time = copy.strip_prefix(COPY_PREFIX)?;
    let shaped = !time.is_empty() && time.bytes().all(|b| b.is_ascii_alphanumeric());
    shaped.then(|| copy.to_owned())
}

/// Why a client must not start the engine at `root` by itself: a restore runs, brought the
/// 2.4 data back and one 2.5 start would upgrade it again, or did not finish. A pending record
/// whose engine process is gone was left by a restore that died, and the start that follows
/// clears it; a restoring one whose process is gone, or a failed rollback, needs the
/// recovery screen. Inputs: the engine root and whether its login service still runs a
/// process. Output: `engine_restoring`, `engine_restored`, `engine_restore_broken` or none.
pub fn blocks_start(root: &Path, running: bool) -> Option<&'static str> {
    match state(root)? {
        RestoreState::Pending | RestoreState::Restoring { .. } if running => Some(ENGINE_RESTORING),
        RestoreState::Pending => None,
        RestoreState::Restoring { .. } | RestoreState::Broken { .. } => Some(ENGINE_RESTORE_BROKEN),
        RestoreState::Restored { .. } => Some(ENGINE_RESTORED),
        RestoreState::Failed { .. } => None,
    }
}

/// Why the owner's explicit restart of the engine at `root` must not start it: a restore
/// still runs, and a restart would stop its process in the middle of the copy, or a restore
/// did not finish, whose start the engine refuses, so the login service must not run it.
/// After a restore that brought the 2.4 data back it runs; that restart is the way back to
/// 2.5. Inputs: the root and whether its login service still runs a process. Output: the code.
pub fn restart_refusal(root: &Path, running: bool) -> Option<&'static str> {
    let refused = blocks_start(root, running).filter(|code| *code != ENGINE_RESTORED);
    assert!(refused != Some(ENGINE_RESTORING) || running);
    refused
}

/// [`restart_refusal`] for the engine's login service, as a result a restart path returns
/// at once. Inputs: the root and the service. Output: Ok, or the refusal's code.
pub fn refuse_restart(root: &Path, service: &crate::service::Service) -> Result<(), &'static str> {
    restart_refusal(root, service_running(service)).map_or(Ok(()), Err)
}

/// Whether an engine start ended on purpose: the engine refused to open data a restore left
/// unfinished, so the login service must not start it again. Input: the start's error text.
/// Output: the answer.
pub fn stops_on_purpose(error: &str) -> bool {
    error
        .split(|c: char| c != '_' && !c.is_ascii_lowercase())
        .any(|word| word == ENGINE_RESTORE_BROKEN)
}

/// The codes a recovery action refuses with, as a client shows them.
const RECOVERY_CODES: [&str; 6] = [
    RECOVERY_BUSY,
    "storage_putback_copy_missing",
    "storage_putback_copy_failed",
    "storage_putback_check_failed",
    "storage_restore_busy",
    "storage_restore_unavailable",
];

/// The code a recovery refusal reaches a client by: the known code its text starts with, else
/// the restore's plain failure. Input: the refusal's text. Output: the code.
pub fn recovery_code(text: &str) -> &'static str {
    let word = text
        .split(|c: char| c != '_' && !c.is_ascii_lowercase())
        .next();
    RECOVERY_CODES
        .into_iter()
        .find(|code| word == Some(*code))
        .unwrap_or("storage_restore_failed")
}

/// The folder beside engine.db that holds the backups and the kept copies of the engine at
/// `root`, which the recovery screen names. Input: the root. Output: the folder.
pub fn backups(root: &Path) -> std::path::PathBuf {
    root.join(kernel_storage::migrations::BACKUPS)
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

/// What the recovery screen offers for the engine at `root`, read without an engine and
/// written nowhere. Inputs: the root and whether its login service still runs a process.
/// Output: the offer, none when the record needs no recovery, or the refusal's text.
pub fn recovery(root: &Path, running: bool) -> Result<Option<Recovery>, String> {
    idle(running)?;
    #[cfg(unix)]
    {
        use kernel_storage::restore::recovery::Recovery as Offer;
        let database = root.join(kernel_storage::migrations::DATABASE);
        let offer =
            kernel_storage::restore::recovery::recovery(&database).map_err(|cause| cause.0)?;
        Ok(offer.map(|offer| match offer {
            Offer::Continue => Recovery::Continue,
            Offer::Finished { copy } => Recovery::Finished { copy },
            Offer::PutBack { copy } => Recovery::PutBack { copy },
            Offer::Stuck { code } => Recovery::Stuck { code },
        }))
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        Ok(None)
    }
}

/// Puts the copy the restore record names back over engine.db, checked before and after,
/// and clears the record. Inputs: the root and whether its login service still runs a
/// process. Output: Ok, or the refusal's text with the record left as it was.
pub fn put_back(root: &Path, running: bool) -> Result<(), String> {
    idle(running)?;
    #[cfg(unix)]
    return kernel_storage::restore::recovery::put_back(
        &root.join(kernel_storage::migrations::DATABASE),
    )
    .map(|saved| assert!(saved.starts_with(root)))
    .map_err(|cause| cause.0);
    #[cfg(not(unix))]
    {
        let _ = root;
        Err("storage_restore_unavailable".into())
    }
}

/// Carries on with the 2.5 data after a restore stopped before it changed anything, once
/// engine.db passes its check again. Inputs: the root and whether its login service still
/// runs a process. Output: Ok once the record is gone, or the refusal's text.
pub fn continue_2_5(root: &Path, running: bool) -> Result<(), String> {
    idle(running)?;
    #[cfg(unix)]
    return kernel_storage::restore::recovery::continue_2_5(
        &root.join(kernel_storage::migrations::DATABASE),
    )
    .map_err(|cause| cause.0);
    #[cfg(not(unix))]
    {
        let _ = root;
        Err("storage_restore_unavailable".into())
    }
}

/// Finishes a restore that wrote the 2.4 data and stopped before it recorded so: the key
/// sweep runs again over the named copy and the record says restored. Inputs: the root,
/// whether its login service still runs a process and the settings files. Output: Ok, or
/// the refusal's text.
pub fn finish(root: &Path, running: bool, global: &Path, project: &Path) -> Result<(), String> {
    idle(running)?;
    #[cfg(unix)]
    return crate::storage::setup::finish_restore(root, global, project)
        .map(|copy| assert!(!copy.is_empty()))
        .map_err(|cause| cause.0);
    #[cfg(not(unix))]
    {
        let _ = (root, global, project);
        Err("storage_restore_unavailable".into())
    }
}

/// Refuses a recovery action while the engine's login service still runs a process, which
/// may hold engine.db. Input: whether it runs. Output: Ok, or `storage_putback_busy`.
fn idle(running: bool) -> Result<(), String> {
    match running {
        true => Err(RECOVERY_BUSY.into()),
        false => Ok(()),
    }
}

#[cfg(all(test, unix))]
mod tests;
