use super::{Grants, IdeEvent, relative};
use crate::engine::EngineError;
use crate::engine::owner::{OwnerRuntimeHandle, failure};
use crate::landing::ChangeSetId;
use crate::protocol::{ProjectId, RequestId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::oneshot;

/// How long a reveal waits for the IDE extension to say it showed the file.
pub const REVEAL_WAIT: Duration = Duration::from_secs(5);
/// How long the desktop may still bring a project's IDE tab forward after a reveal.
pub const FOCUS_TTL_MS: u64 = 10_000;
const WAITING_MAX: usize = 16;

/// What the owner asked the desktop IDE to show: a file or folder by its path in the
/// project folder, or a change set's worktree, or one file or folder inside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RevealTarget {
    Project {
        path: String,
    },
    ChangeSet {
        id: ChangeSetId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
}

/// The project whose IDE tab the desktop brings forward, and the reveal that asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevealFocus {
    pub project: ProjectId,
    pub request: RequestId,
}

/// The reveals waiting for the extension's acknowledgement, at most `WAITING_MAX`, and the
/// newest reveal with the time it was sent, until the desktop takes it.
#[derive(Default)]
pub(crate) struct Reveals {
    waiting: Mutex<Vec<Waiting>>,
    focus: Mutex<Option<(RevealFocus, u64)>>,
}

/// One reveal sent to a project's IDE and the sender its acknowledgement completes.
struct Waiting {
    project: ProjectId,
    request: RequestId,
    acked: oneshot::Sender<()>,
}

impl Reveals {
    /// Registers a reveal before its event goes out, dropping waits that already ended.
    /// Inputs: the project and the reveal. Output: the receiver its acknowledgement
    /// completes, or `ide_reveal_busy` while `WAITING_MAX` reveals wait.
    pub(super) fn wait(
        &self,
        project: &ProjectId,
        request: &RequestId,
    ) -> Result<oneshot::Receiver<()>, EngineError> {
        let mut waiting = self
            .waiting
            .lock()
            .map_err(|_| failure("ide_unavailable"))?;
        waiting.retain(|wait| !wait.acked.is_closed());
        if waiting.len() >= WAITING_MAX {
            return Err(failure("ide_reveal_busy"));
        }
        let (acked, answer) = oneshot::channel();
        waiting.push(Waiting {
            project: project.clone(),
            request: request.clone(),
            acked,
        });
        Ok(answer)
    }

    /// Completes the reveal a project's IDE acknowledged. Inputs: the project whose
    /// capability sent the acknowledgement and the reveal. Output: Ok, or
    /// `ide_reveal_unknown` for a reveal this project is not waiting on.
    pub(super) fn ack(&self, project: &ProjectId, request: &RequestId) -> Result<(), EngineError> {
        let mut waiting = self
            .waiting
            .lock()
            .map_err(|_| failure("ide_unavailable"))?;
        let index = waiting
            .iter()
            .position(|wait| &wait.project == project && &wait.request == request)
            .ok_or_else(|| failure("ide_reveal_unknown"))?;
        let found = waiting.swap_remove(index);
        let _ = found.acked.send(());
        Ok(())
    }

    /// Stops waiting on a reveal that was answered or timed out. Input: the reveal.
    pub(super) fn forget(&self, request: &RequestId) {
        if let Ok(mut waiting) = self.waiting.lock() {
            waiting.retain(|wait| &wait.request != request);
        }
    }

    /// Marks the project's IDE tab for the desktop to bring forward, replacing an older
    /// mark. Inputs: the project, the reveal and the time in milliseconds.
    pub(super) fn mark(&self, project: &ProjectId, request: &RequestId, now_ms: u64) {
        if let Ok(mut focus) = self.focus.lock() {
            *focus = Some((
                RevealFocus {
                    project: project.clone(),
                    request: request.clone(),
                },
                now_ms,
            ));
        }
    }

    /// Takes the newest mark once, if it is at most `FOCUS_TTL_MS` old.
    /// Input: the time in milliseconds. Output: the project to bring forward, if any.
    fn take(&self, now_ms: u64) -> Option<RevealFocus> {
        let (focus, at_ms) = self.focus.lock().ok()?.take()?;
        (now_ms.saturating_sub(at_ms) <= FOCUS_TTL_MS).then_some(focus)
    }
}

impl Grants {
    /// Whether a project's IDE extension is subscribed to its events right now, which
    /// means the desktop has the project's IDE open. Input: the project.
    fn listening(&self, project: &ProjectId) -> bool {
        self.live.lock().is_ok_and(|live| {
            live.iter()
                .any(|grant| &grant.project == project && grant.events.receiver_count() > 0)
        })
    }
}

/// Shows a file or folder in the project's running desktop IDE: checks that the target
/// sits inside a folder the project's IDE workspace lists, sends one `reveal` event, marks
/// the project's IDE tab for the desktop, and waits up to `REVEAL_WAIT` for the extension
/// to acknowledge it. Inputs: the owner runtime, the project and the target. Output: the
/// reveal's id, or `ide_not_running`, `ide_reveal_unanswered` or a target refusal.
pub(crate) async fn reveal(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
    target: RevealTarget,
) -> Result<RequestId, EngineError> {
    let root = runtime.ide()?.root;
    let asked = project.clone();
    let path = tokio::task::spawn_blocking(move || resolve(&root, &asked, &target))
        .await
        .map_err(|_| failure("ide_reveal_unavailable"))??;
    let grants = runtime.ide_grants();
    if !grants.listening(project) {
        return Err(failure("ide_not_running"));
    }
    let random = crate::engine::owner::random_id()?;
    let request = RequestId::new(format!("reveal-{}", &random[..24]))
        .map_err(|_| failure("ide_reveal_unavailable"))?;
    let acked = grants.reveals.wait(project, &request)?;
    grants.publish(
        project,
        IdeEvent::Reveal {
            request: request.clone(),
            path,
        },
    );
    grants.reveals.mark(project, &request, now_ms());
    let answered = tokio::time::timeout(REVEAL_WAIT, acked).await;
    grants.reveals.forget(&request);
    match answered {
        Ok(Ok(())) => Ok(request),
        _ => Err(failure("ide_reveal_unanswered")),
    }
}

/// Answers `ide.reveal_ack`: the project's extension showed the reveal. Inputs: the owner
/// runtime, the capability's project and the reveal. Output: null data, or
/// `ide_reveal_unknown` for a reveal this project is not waiting on.
pub(super) fn acked(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
    request: &RequestId,
) -> Result<serde_json::Value, EngineError> {
    runtime.ide_grants().reveals.ack(project, request)?;
    Ok(serde_json::Value::Null)
}

/// Takes the project whose IDE tab the desktop should bring forward, once, while the
/// reveal that asked is at most `FOCUS_TTL_MS` old. Input: the owner runtime.
/// Output: the project and reveal, or none.
pub(crate) fn focus(runtime: &OwnerRuntimeHandle) -> Option<RevealFocus> {
    runtime.ide_grants().reveals.take(now_ms())
}

/// Finds the path the extension opens for a target: the folder the target names must be
/// one the project's workspace lists, and a path inside it must be relative, exist and
/// stay inside that folder after links resolve. Inputs: the engine root, project and
/// target. Output: the folder as the workspace names it joined with the path.
fn resolve(root: &Path, project: &ProjectId, target: &RevealTarget) -> Result<String, EngineError> {
    let db = crate::landing::open(root).map_err(|_| failure("ide_reveal_unavailable"))?;
    let sections = crate::ide::workspace::listed(&db, project)?;
    let (folder, inside) = match target {
        RevealTarget::Project { path } => (
            sections
                .first()
                .ok_or_else(|| failure("ide_reveal_not_listed"))?
                .path
                .clone(),
            Some(path.as_str()),
        ),
        RevealTarget::ChangeSet { id, path } => (worktree(&db, project, id)?, path.as_deref()),
    };
    let section = sections
        .iter()
        .find(|section| section.path == folder)
        .ok_or_else(|| failure("ide_reveal_not_listed"))?;
    let missing = |_| failure("ide_reveal_missing");
    let base = std::fs::canonicalize(&section.path).map_err(missing)?;
    let shown = match inside {
        None => section.path.clone(),
        Some(path) => {
            if !relative(path) {
                return Err(failure("ide_reveal_invalid"));
            }
            let found = std::fs::canonicalize(section.path.join(path)).map_err(missing)?;
            if !found.starts_with(&base) {
                return Err(failure("ide_reveal_outside"));
            }
            section.path.join(path)
        }
    };
    shown
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| failure("ide_path_unreadable"))
}

/// The worktree a project's change set was made in. Inputs: the engine database, the
/// project and the change set. Output: the worktree, or `changes_not_found`.
fn worktree(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
) -> Result<PathBuf, EngineError> {
    db.query_row(
        "SELECT worktree FROM landing_change_sets WHERE project=?1 AND id=?2",
        params![project.as_str(), id.as_str()],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(|_| failure("ide_reveal_unavailable"))?
    .map(PathBuf::from)
    .ok_or_else(|| failure("changes_not_found"))
}

/// The time in milliseconds since the Unix epoch, zero before it.
fn now_ms() -> u64 {
    u64::try_from(crate::team::now_ms()).unwrap_or(0)
}

#[cfg(all(test, unix))]
pub(super) mod tests;
