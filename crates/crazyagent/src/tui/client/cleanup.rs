use super::settings::connection_lost;
use super::*;
use crate::team::cleanup::{FinishedAgent, Removal};

/// One cleanup request the Team screen sends: read a project's finished agents, or remove
/// the ones the owner confirmed. Runs on the owner worker; the reply arrives as
/// `OwnerEvent::Cleanup` with this query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum CleanupQuery {
    Finished {
        project: ProjectId,
    },
    Remove {
        project: ProjectId,
        agents: Vec<i64>,
    },
}

/// What the engine answered a cleanup request with: the finished agents, or what a removal
/// did, with the code of the agent it kept when it stopped part way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum CleanupReply {
    Finished(Vec<FinishedAgent>),
    Removed(Removal),
}

/// One cleanup reply for the screens: the query it answers and the engine's answer, or a
/// bounded plain code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct CleanupEvent {
    pub query: CleanupQuery,
    pub result: Result<CleanupReply, String>,
}

impl CleanupEvent {
    /// The reply for a request the worker could not run because the trusted connection is
    /// gone. Takes the query. Returns the event for that query.
    pub(in crate::tui) fn unavailable(query: CleanupQuery) -> Self {
        Self {
            query,
            result: Err("trusted_connection_unavailable".into()),
        }
    }
}

impl Connection {
    /// Queues one cleanup request on the paired owner worker.
    /// Takes the query. Returns an error while another native request is pending.
    pub(in crate::tui) fn cleanup(&mut self, query: CleanupQuery) -> Result<(), String> {
        self.owner_job(Job::Cleanup(Box::new(query)))
    }
}

impl NativeOwner {
    /// Runs one cleanup request through the trusted owner controller.
    /// Takes the query. Returns the event; a transport failure detaches the connection so
    /// the shell reconnects.
    pub(super) async fn cleanup(&mut self, query: CleanupQuery) -> CleanupEvent {
        let mut lost = false;
        let result = self.cleanup_reply(query.clone(), &mut lost).await;
        if lost {
            self.detach();
        }
        CleanupEvent { query, result }
    }

    async fn cleanup_reply(
        &mut self,
        query: CleanupQuery,
        lost: &mut bool,
    ) -> Result<CleanupReply, String> {
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
            CleanupQuery::Finished { project } => controller
                .finished_agents(project)
                .await
                .map(CleanupReply::Finished),
            CleanupQuery::Remove { project, agents } => controller
                .remove_finished_agents(project, agents)
                .await
                .map(CleanupReply::Removed),
        }
        .map_err(problem)
    }
}
