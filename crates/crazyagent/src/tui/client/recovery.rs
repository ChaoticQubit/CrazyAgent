use super::settings::connection_lost;
use super::*;
use crate::engine::owner::RecoveryBanner;
use crate::ledger::ActionKind;

/// One Recovery request the project page sends: read a project's interrupted work, start
/// an agent's failed resume again, or dismiss one notice. Runs on the owner worker; the
/// reply arrives as `OwnerEvent::Recovery` with this query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum RecoveryQuery {
    Read {
        project: ProjectId,
    },
    Retry {
        project: ProjectId,
        agent: i64,
    },
    Dismiss {
        project: ProjectId,
        kind: ActionKind,
        id: i64,
    },
}

impl RecoveryQuery {
    /// The project the request is about, so only that project's page applies its reply.
    pub(in crate::tui) fn project(&self) -> &ProjectId {
        match self {
            Self::Read { project }
            | Self::Retry { project, .. }
            | Self::Dismiss { project, .. } => project,
        }
    }
}

/// What the engine answered a Recovery request with: the project's banner, or that a retry
/// or dismiss was done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum RecoveryReply {
    Banner(RecoveryBanner),
    Done,
}

/// One Recovery reply for the screens: the query it answers and the engine's answer, or a
/// bounded plain code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct RecoveryEvent {
    pub query: RecoveryQuery,
    pub result: Result<RecoveryReply, String>,
}

impl RecoveryEvent {
    /// The reply for a request the worker could not run because the trusted connection is
    /// gone. Takes the query. Returns the event for that query.
    pub(in crate::tui) fn unavailable(query: RecoveryQuery) -> Self {
        Self {
            query,
            result: Err("trusted_connection_unavailable".into()),
        }
    }
}

impl Connection {
    /// Queues one Recovery request on the paired owner worker.
    /// Takes the query. Returns an error while another native request is pending.
    pub(in crate::tui) fn recovery(&mut self, query: RecoveryQuery) -> Result<(), String> {
        self.owner_job(Job::Recovery(Box::new(query)))
    }
}

impl NativeOwner {
    /// Runs one Recovery request through the trusted owner controller.
    /// Takes the query. Returns the event; a transport failure detaches the connection so
    /// the shell reconnects.
    pub(super) async fn recovery(&mut self, query: RecoveryQuery) -> RecoveryEvent {
        let mut lost = false;
        let result = self.recovery_reply(query.clone(), &mut lost).await;
        if lost {
            self.detach();
        }
        RecoveryEvent { query, result }
    }

    async fn recovery_reply(
        &mut self,
        query: RecoveryQuery,
        lost: &mut bool,
    ) -> Result<RecoveryReply, String> {
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
        match query {
            RecoveryQuery::Read { project } => controller
                .recovery_banner(project)
                .await
                .map(RecoveryReply::Banner),
            RecoveryQuery::Retry { agent, .. } => controller
                .recovery_retry(agent)
                .await
                .map(|()| RecoveryReply::Done),
            RecoveryQuery::Dismiss { kind, id, .. } => controller
                .recovery_dismiss(kind, id)
                .await
                .map(|()| RecoveryReply::Done),
        }
        .map_err(problem)
    }
}
