use super::team::read_failure;
use super::{BridgeError, IntentReceipt, NativeBridge, bounded, failure};
use crazyagent::protocol::ProjectId;
use crazyagent::team::TeamAction;
use crazyagent::team::questions::Inbox;

fn project(id: &str) -> Result<ProjectId, BridgeError> {
    ProjectId::new(id).map_err(|_| failure("invalid_project"))
}

impl NativeBridge {
    /// Reads a project's Questions inbox for the owner: the open questions agents put to the
    /// owner, most urgent first, and the agents waiting on the owner in their own terminal.
    /// Takes the project id. Returns the bounded inbox or a plain failure code.
    pub async fn coord_questions(&self, id: &str) -> Result<Inbox, BridgeError> {
        let project_id = project(id)?;
        let inbox = self
            .reader()?
            .coord_questions(project_id)
            .await
            .map_err(|e| read_failure(&e.0))?;
        bounded(inbox)
    }

    /// Answers one open question an agent put to the owner, as the owner's reply (Q14b).
    /// Takes the project id, the question's message id, the answer and the client
    /// generation. Returns the actual commit receipt; an answered or unknown question is a
    /// plain rejection.
    pub async fn coord_answer(
        &mut self,
        id: &str,
        message: i64,
        text: String,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        let action = TeamAction::AnswerQuestion {
            project_id,
            message,
            text,
        };
        self.team_action(action, generation).await
    }
}
