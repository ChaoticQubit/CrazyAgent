use super::settings::connection_lost;
use super::*;
use crate::questions::{Answer, Page, Settlement};

/// One Questions request the TUI sends: read a project's questions, answer or dismiss one
/// naming the version the owner read, or count the pending ones per project for the footer.
/// Runs on the owner worker; the reply arrives as `OwnerEvent::Questions` with this query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum QuestionsQuery {
    List {
        project: ProjectId,
    },
    Answer {
        project: ProjectId,
        id: u64,
        version: u64,
        answer: Answer,
    },
    Dismiss {
        project: ProjectId,
        id: u64,
        version: u64,
    },
    Counts,
}

/// What the engine answered a Questions request with: a page, how a settling write ended, or
/// the pending counts per project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum QuestionsReply {
    Page(Page),
    Settled(Settlement),
    Counts(Vec<(ProjectId, u32)>),
}

/// One Questions reply for the screens: the query it answers and the engine's answer, or a
/// bounded plain code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct QuestionsEvent {
    pub query: QuestionsQuery,
    pub result: Result<QuestionsReply, String>,
}

impl QuestionsEvent {
    /// The reply for a request the worker could not run because the trusted connection is
    /// gone. Takes the query. Returns the event for that query.
    pub(in crate::tui) fn unavailable(query: QuestionsQuery) -> Self {
        Self {
            query,
            result: Err("trusted_connection_unavailable".into()),
        }
    }
}

impl Connection {
    /// Queues one Questions request on the paired owner worker.
    /// Takes the query. Returns an error while another native request is pending.
    pub(in crate::tui) fn questions(&mut self, query: QuestionsQuery) -> Result<(), String> {
        self.owner_job(Job::Questions(Box::new(query)))
    }
}

impl NativeOwner {
    /// Runs one Questions request through the trusted owner controller.
    /// Takes the query. Returns the event; a transport failure detaches the connection so
    /// the shell reconnects.
    pub(super) async fn questions(&mut self, query: QuestionsQuery) -> QuestionsEvent {
        let mut lost = false;
        let result = self.questions_reply(query.clone(), &mut lost).await;
        if lost {
            self.detach();
        }
        QuestionsEvent { query, result }
    }

    async fn questions_reply(
        &mut self,
        query: QuestionsQuery,
        lost: &mut bool,
    ) -> Result<QuestionsReply, String> {
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
            QuestionsQuery::List { project } => controller
                .questions_list(project, None)
                .await
                .map(QuestionsReply::Page),
            QuestionsQuery::Answer {
                id,
                version,
                answer,
                ..
            } => controller
                .questions_answer(id, version, answer)
                .await
                .map(QuestionsReply::Settled),
            QuestionsQuery::Dismiss { id, version, .. } => controller
                .questions_dismiss(id, version)
                .await
                .map(QuestionsReply::Settled),
            QuestionsQuery::Counts => controller
                .questions_counts()
                .await
                .map(QuestionsReply::Counts),
        }
        .map_err(problem)
    }
}
