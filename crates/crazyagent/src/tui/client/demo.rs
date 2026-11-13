use super::settings::connection_lost;
use super::*;
use crate::demo::DemoView;
use crate::engine::owner::OwnerResult;
use crate::protocol::CredentialRef;

/// The code a Create the demo reply carries when the engine's answer could not be read, so
/// the demo page reads the demo again to learn whether it was made.
pub(in crate::tui) const OUTCOME_UNKNOWN: &str = "demo_change_outcome_unknown";

/// One demo request the demo page, Home and the project page send: read the demo, or make
/// it on the pool the owner picked. Runs on the owner worker; the reply arrives as
/// `OwnerEvent::Demo` with this query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum DemoQuery {
    Read,
    Create {
        pool: CredentialRef,
        generation: u64,
    },
}

/// What the engine answered a demo request with: the demo read, or that the demo was made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum DemoReply {
    View(Box<DemoView>),
    Created,
}

/// One demo reply for the screens: the query it answers and the engine's answer, or a
/// bounded plain code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct DemoEvent {
    pub query: DemoQuery,
    pub result: Result<DemoReply, String>,
}

impl DemoEvent {
    /// The reply for a request the worker could not run because the trusted connection is
    /// gone. Takes the query. Returns the event for that query.
    pub(in crate::tui) fn unavailable(query: DemoQuery) -> Self {
        Self {
            query,
            result: Err("trusted_connection_unavailable".into()),
        }
    }
}

impl Connection {
    /// Queues one demo request on the paired owner worker.
    /// Takes the query. Returns an error while another native request is pending.
    pub(in crate::tui) fn demo(&mut self, query: DemoQuery) -> Result<(), String> {
        self.owner_job(Job::Demo(Box::new(query)))
    }
}

impl NativeOwner {
    /// Runs one demo request through the trusted owner controller.
    /// Takes the query. Returns the event; a transport failure detaches the connection so
    /// the shell reconnects.
    pub(super) async fn demo(&mut self, query: DemoQuery) -> DemoEvent {
        let mut lost = false;
        let result = self.demo_reply(query.clone(), &mut lost).await;
        if lost {
            self.detach();
        }
        DemoEvent { query, result }
    }

    async fn demo_reply(&mut self, query: DemoQuery, lost: &mut bool) -> Result<DemoReply, String> {
        if !self.ready {
            return Err("trusted_connection_unavailable".into());
        }
        let controller = self
            .controller
            .as_mut()
            .ok_or("trusted_connection_unavailable")?;
        let problem = |error: crate::engine::EngineError| {
            if connection_lost(&error.0) {
                *lost = true;
            }
            bounded_problem(&error.0)
        };
        match query {
            DemoQuery::Read => controller
                .project_demo()
                .await
                .map(|view| DemoReply::View(Box::new(view)))
                .map_err(problem),
            DemoQuery::Create { pool, generation } => created(
                controller
                    .create_demo(pool, generation)
                    .await
                    .map_err(problem)?,
            ),
        }
    }
}

fn created(response: OwnerResponse) -> Result<DemoReply, String> {
    match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            match operation.result {
                OwnerResult::Applied => Ok(DemoReply::Created),
                OwnerResult::Rejected { code } => Err(bounded_problem(&code)),
                _ => Err(OUTCOME_UNKNOWN.into()),
            }
        }
        OwnerResponse::Rejected { code } => Err(bounded_problem(&code)),
        _ => Err(OUTCOME_UNKNOWN.into()),
    }
}
