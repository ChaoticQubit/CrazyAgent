use super::settings::{applied, problem};
use super::*;
use crate::learning::{LearningProgress, MarkStep};

/// One learning request Home or a page sends: read the Learn CrazyAgent steps, or set or
/// clear one mark (a page opened, the guide skipped or shown again). Runs on the owner
/// worker; the reply arrives as `OwnerEvent::Learning` with this query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum LearningQuery {
    Progress,
    Mark {
        step: MarkStep,
        on: bool,
        generation: u64,
    },
}

/// What the engine answered a learning request with: the steps, or that the mark now
/// stands as asked, also when it already did and nothing was sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum LearningReply {
    Progress(LearningProgress),
    Marked,
}

/// One learning reply for the screens: the query it answers and the engine's answer, or a
/// bounded plain code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct LearningEvent {
    pub query: LearningQuery,
    pub result: Result<LearningReply, String>,
}

impl LearningEvent {
    /// The reply for a request the worker could not run because the trusted connection is
    /// gone. Takes the query. Returns the event for that query.
    pub(in crate::tui) fn unavailable(query: LearningQuery) -> Self {
        Self::failed(query, "trusted_connection_unavailable")
    }

    /// The reply for a request that failed before it reached the engine.
    /// Takes the query and the problem code.
    /// Returns the event for that query.
    pub(in crate::tui) fn failed(query: LearningQuery, code: &str) -> Self {
        Self {
            query,
            result: Err(code.to_owned()),
        }
    }
}

impl Connection {
    /// Queues one learning request on the paired owner worker.
    /// Takes the query. Returns an error while another native request is pending.
    pub(in crate::tui) fn learning(&mut self, query: LearningQuery) -> Result<(), String> {
        self.owner_job(Job::Learning(Box::new(query)))
    }
}

impl NativeOwner {
    /// Runs one learning request through the trusted owner controller.
    /// Takes the query. Returns the event; a transport failure detaches the connection so
    /// the shell reconnects.
    pub(super) async fn learning(&mut self, query: LearningQuery) -> LearningEvent {
        let mut lost = false;
        let result = self.learning_reply(query.clone(), &mut lost).await;
        if lost {
            self.detach();
        }
        LearningEvent { query, result }
    }

    async fn learning_reply(
        &mut self,
        query: LearningQuery,
        lost: &mut bool,
    ) -> Result<LearningReply, String> {
        let controller = self.settings_controller()?;
        match query {
            LearningQuery::Progress => controller
                .learning_progress()
                .await
                .map(LearningReply::Progress)
                .map_err(|error| problem(&error, lost)),
            LearningQuery::Mark {
                step,
                on,
                generation,
            } => match controller.set_learning_mark(step, on, generation).await {
                Ok(None) => Ok(LearningReply::Marked),
                Ok(Some(response)) => applied(Ok(response), "learning_mark_outcome_unknown", lost)
                    .map(|()| LearningReply::Marked),
                Err(error) => Err(problem(&error, lost)),
            },
        }
    }
}
