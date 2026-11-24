use super::*;

#[cfg(unix)]
const PATH_BYTES_MAX: usize = 120;

#[cfg(unix)]
const TAIL_BYTES_MAX: usize = 32;

impl TeamRuntime {
    /// Tells who should act when a whole turn of an agent working in its own clone landed
    /// nothing new while the clone holds uncommitted edits: the lead of a worker, and the
    /// owner in a project note unless the lead lands that worker's work. The agent itself is
    /// not told. Inputs: whether the turn ended, the project, the workspace, the lead with its
    /// landing, what ended and whether a change set still waits. Output: any store failure.
    #[cfg(unix)]
    pub(super) fn note_uncommitted(
        &mut self,
        whole: bool,
        project: &ProjectId,
        workspace: &crate::landing::Workspace,
        (lead, lands): (Option<i64>, Option<crate::team::ChildLanding>),
        what: &str,
        waiting: bool,
    ) -> Result<(), EngineError> {
        if !whole {
            return Ok(());
        }
        let held = crate::landing::open(&self.root).and_then(|db| {
            crate::landing::retire::uncommitted(
                &db,
                &workspace.repository,
                &workspace.worktree,
                &workspace.git_dir,
            )
        });
        let held = match held {
            Ok(Some(held)) => held,
            Ok(None) => return Ok(()),
            Err(problem) => {
                eprintln!(
                    "landing_uncommitted_unread cause={}",
                    crate::protocol::plain_code(&problem.0)
                );
                return Ok(());
            }
        };
        let note = uncommitted_line(what, waiting, &held);
        if let Some(lead) = lead {
            self.note_lead(project, lead, note.clone())?;
        }
        if lands != Some(crate::team::ChildLanding::Lead) {
            self.note(project, &note)?;
        }
        Ok(())
    }

    /// Writes nothing: an agent works in its own clone only on unix. Inputs: as the unix
    /// form. Output: Ok.
    #[cfg(not(unix))]
    pub(super) fn note_uncommitted(
        &mut self,
        _: bool,
        _: &ProjectId,
        _: &crate::landing::Workspace,
        _: (Option<i64>, Option<crate::team::ChildLanding>),
        _: &str,
        _: bool,
    ) -> Result<(), EngineError> {
        Ok(())
    }
}

/// The note naming what an agent's clone holds uncommitted: as many paths as fit, each with
/// control characters replaced and cut to `PATH_BYTES_MAX`, then how many more. Inputs: what
/// ended, whether a change set still waits, so commits may still land, and the listing.
/// Output: the line, within `NOTE_BYTES_MAX`.
#[cfg(unix)]
fn uncommitted_line(
    what: &str,
    waiting: bool,
    held: &crate::landing::retire::Uncommitted,
) -> String {
    assert!(!held.paths.is_empty());
    let head = if waiting {
        format!("Uncommitted edits from {what} did not land and stay only in its worktree: ")
    } else {
        format!(
            "The edits from {what} are not committed, so nothing landed; they stay only in its worktree: "
        )
    };
    let mut line = crate::team::bound_text(&head, NOTE_BYTES_MAX / 2);
    let mut shown = 0_usize;
    for path in &held.paths {
        let name =
            crate::team::bound_text(&path.replace(char::is_control, "\u{fffd}"), PATH_BYTES_MAX);
        let gap = if shown == 0 { "" } else { ", " };
        if line.len() + gap.len() + name.len() + TAIL_BYTES_MAX > NOTE_BYTES_MAX {
            break;
        }
        line.push_str(gap);
        line.push_str(&name);
        shown += 1;
    }
    assert!(shown > 0);
    match held.total {
        Some(total) if total > shown => line.push_str(&format!(" and {} more", total - shown)),
        Some(_) => {}
        None => line.push_str(" and more"),
    }
    line.push('.');
    assert!(line.len() <= NOTE_BYTES_MAX);
    line
}
