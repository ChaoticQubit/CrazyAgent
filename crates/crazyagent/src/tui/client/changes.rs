use super::settings::connection_lost;
use super::*;
use crate::landing::{AcceptOutcome, ChangeDiff, ChangeSetId, ChangeSetView};

#[cfg(all(test, unix))]
mod tests;

/// One Changes request the TUI Changes view sends.
/// Reads a project's change sets or one diff page, or acts on one change set.
/// Runs on the owner worker; the reply arrives as `OwnerEvent::Changes` with this query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum ChangesQuery {
    List {
        project: ProjectId,
    },
    Diff {
        id: ChangeSetId,
        page: u16,
    },
    Accept {
        id: ChangeSetId,
        expected_head: String,
        message: String,
    },
    Discard {
        id: ChangeSetId,
        expected_head: String,
    },
    Restore {
        id: ChangeSetId,
    },
    Update {
        id: ChangeSetId,
    },
}

/// What the engine answered to one Changes request.
/// Reads carry the change sets or the diff page; actions carry what they did.
/// Diff text is plain text the view shows as text only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum ChangesView {
    List(Vec<ChangeSetView>),
    Diff(ChangeDiff),
    Accepted(AcceptOutcome),
    Discarded { restorable_until_ms: u64 },
    Restored,
    Updating,
}

/// One Changes reply for the screens.
/// Holds the query it answers and the view, or a bounded plain refusal code.
/// The Changes view applies it only when the query is the one it sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct ChangesEvent {
    pub query: ChangesQuery,
    pub result: Result<ChangesView, String>,
}

impl ChangesEvent {
    /// The reply for a request the worker could not run because the trusted connection is gone.
    /// Takes the query.
    /// Returns the event for that query.
    pub(in crate::tui) fn unavailable(query: ChangesQuery) -> Self {
        Self {
            query,
            result: Err("trusted_connection_unavailable".into()),
        }
    }
}

impl Connection {
    /// Queues one Changes request on the paired owner worker.
    /// Takes the query.
    /// Returns an error while another native request is pending.
    pub(in crate::tui) fn changes(&mut self, query: ChangesQuery) -> Result<(), String> {
        self.owner_job(Job::Changes(Box::new(query)))
    }
}

impl NativeOwner {
    /// Runs one Changes request through the trusted owner controller.
    /// Takes the query.
    /// Returns the event; a transport failure detaches the connection so the shell reconnects.
    pub(super) async fn changes(&mut self, query: ChangesQuery) -> ChangesEvent {
        let mut lost = false;
        let result = self.changes_view(query.clone(), &mut lost).await;
        if lost {
            self.detach();
        }
        ChangesEvent { query, result }
    }

    async fn changes_view(
        &mut self,
        query: ChangesQuery,
        lost: &mut bool,
    ) -> Result<ChangesView, String> {
        if !self.ready {
            return Err("trusted_connection_unavailable".into());
        }
        let controller = self
            .controller
            .as_ref()
            .ok_or("trusted_connection_unavailable")?;
        let problem = |error: crate::engine::EngineError| {
            if connection_lost(&error.0) {
                *lost = true;
            }
            bounded_problem(&error.0)
        };
        Ok(match query {
            ChangesQuery::List { project } => {
                ChangesView::List(controller.change_sets(project).await.map_err(problem)?)
            }
            ChangesQuery::Diff { id, page } => {
                ChangesView::Diff(controller.change_diff(id, page).await.map_err(problem)?)
            }
            ChangesQuery::Accept {
                id,
                expected_head,
                message,
            } => ChangesView::Accepted(
                controller
                    .accept_changes(id, expected_head, Some(message))
                    .await
                    .map_err(problem)?,
            ),
            ChangesQuery::Discard { id, expected_head } => ChangesView::Discarded {
                restorable_until_ms: controller
                    .discard_changes(id, expected_head)
                    .await
                    .map_err(problem)?,
            },
            ChangesQuery::Restore { id } => {
                controller.restore_changes(id).await.map_err(problem)?;
                ChangesView::Restored
            }
            ChangesQuery::Update { id } => {
                controller.update_changes(id).await.map_err(problem)?;
                ChangesView::Updating
            }
        })
    }
}
