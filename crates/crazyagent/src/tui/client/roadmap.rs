use super::settings::connection_lost;
use super::*;
use crate::roadmap_status::RoadmapStatus;

/// One Roadmap request the project page sends: read a project's roadmap status, or ask for
/// a check now. Runs on the owner worker; the reply arrives as `OwnerEvent::Roadmap` with
/// this query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum RoadmapQuery {
    Read { project: ProjectId },
    Refresh { project: ProjectId },
}

impl RoadmapQuery {
    /// The project the request is about, so only that project's page applies its reply.
    pub(in crate::tui) fn project(&self) -> &ProjectId {
        match self {
            Self::Read { project } | Self::Refresh { project } => project,
        }
    }
}

/// What the engine answered a Roadmap request with: the project's status, or that a check
/// was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum RoadmapReply {
    Status(Box<RoadmapStatus>),
    Done,
}

/// One Roadmap reply for the screens: the query it answers and the engine's answer, or a
/// bounded plain code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct RoadmapEvent {
    pub query: RoadmapQuery,
    pub result: Result<RoadmapReply, String>,
}

impl RoadmapEvent {
    /// The reply for a request the worker could not run because the trusted connection is
    /// gone. Takes the query. Returns the event for that query.
    pub(in crate::tui) fn unavailable(query: RoadmapQuery) -> Self {
        Self {
            query,
            result: Err("trusted_connection_unavailable".into()),
        }
    }
}

impl Connection {
    /// Queues one Roadmap request on the paired owner worker.
    /// Takes the query. Returns an error while another native request is pending.
    pub(in crate::tui) fn roadmap(&mut self, query: RoadmapQuery) -> Result<(), String> {
        self.owner_job(Job::Roadmap(Box::new(query)))
    }
}

impl NativeOwner {
    /// Runs one Roadmap request through the trusted owner controller.
    /// Takes the query. Returns the event; a transport failure detaches the connection so
    /// the shell reconnects.
    pub(super) async fn roadmap(&mut self, query: RoadmapQuery) -> RoadmapEvent {
        let mut lost = false;
        let result = self.roadmap_reply(query.clone(), &mut lost).await;
        if lost {
            self.detach();
        }
        RoadmapEvent { query, result }
    }

    async fn roadmap_reply(
        &mut self,
        query: RoadmapQuery,
        lost: &mut bool,
    ) -> Result<RoadmapReply, String> {
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
            RoadmapQuery::Read { project } => controller
                .roadmap_status(project)
                .await
                .map(|status| RoadmapReply::Status(Box::new(status))),
            RoadmapQuery::Refresh { project } => controller
                .roadmap_refresh(project)
                .await
                .map(|()| RoadmapReply::Done),
        }
        .map_err(problem)
    }
}
