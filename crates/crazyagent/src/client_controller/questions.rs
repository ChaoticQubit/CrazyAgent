use super::*;
use crate::protocol::ProjectId;
use crate::questions::{Answer, Page, Settlement};

impl OwnerController {
    fn questions_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads one page of a project's owner questions, pending ones first. Inputs: the
    /// project and the `next` of the page before, none for the first. Output: the page, or a
    /// `QUESTIONS_CODES` refusal.
    pub async fn questions_list(
        &self,
        project: ProjectId,
        after: Option<u64>,
    ) -> Result<Page, EngineError> {
        self.questions_client()?
            .questions_list(project, after)
            .await
    }

    /// Answers one owner question, naming the version the owner read. Inputs: the question
    /// id, the version and the answer. Output: Settled, Stale, or a `QUESTIONS_CODES` refusal.
    pub async fn questions_answer(
        &self,
        id: u64,
        version: u64,
        answer: Answer,
    ) -> Result<Settlement, EngineError> {
        self.questions_client()?
            .questions_answer(id, version, answer)
            .await
    }

    /// Dismisses one owner question, naming the version the owner read. Inputs: the question
    /// id and the version. Output: Settled, Stale, or a `QUESTIONS_CODES` refusal.
    pub async fn questions_dismiss(
        &self,
        id: u64,
        version: u64,
    ) -> Result<Settlement, EngineError> {
        self.questions_client()?
            .questions_dismiss(id, version)
            .await
    }

    /// Counts the questions waiting for the owner per project. Output: the projects that
    /// have any with their counts, or a `QUESTIONS_CODES` refusal.
    pub async fn questions_counts(&self) -> Result<Vec<(ProjectId, u32)>, EngineError> {
        self.questions_client()?.questions_counts().await
    }
}
