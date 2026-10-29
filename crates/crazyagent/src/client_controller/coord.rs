use super::*;
use crate::protocol::ProjectId;
use crate::team::questions::Inbox;

impl OwnerController {
    /// Reads a project's Questions inbox for the owner: the open questions agents put to the
    /// owner, most urgent first, and the agents waiting on the owner in their own terminal.
    /// Takes the project id.
    /// Returns the bounded inbox, or the engine's plain rejection code.
    pub async fn coord_questions(&mut self, project_id: ProjectId) -> Result<Inbox, EngineError> {
        match self
            .read(OwnerRead::CoordQuestions { project_id })
            .await?
            .data
        {
            OwnerData::CoordQuestions { inbox } => Ok(*inbox),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }
}
