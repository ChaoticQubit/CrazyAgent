use super::{BridgeError, IntentReceipt, NativeBridge, failure};
use crazyagent::protocol::ProjectId;
use crazyagent::team::TeamAction;

impl NativeBridge {
    /// Sends the owner's message to one team agent through the coordination layer
    /// (DEC-104), where its mailbox delivers it with a receipt. Takes the project id, the
    /// agent, the text of at most `crazyagent::coord::BODY_BYTES_MAX` bytes and the client
    /// generation. Returns the actual commit receipt; an agent outside the project or a
    /// bad text is a plain rejection.
    pub async fn coord_send(
        &mut self,
        id: &str,
        agent: i64,
        text: String,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = ProjectId::new(id).map_err(|_| failure("invalid_project"))?;
        self.team_action(
            TeamAction::MessageAgent {
                project_id,
                agent,
                text,
            },
            generation,
        )
        .await
    }
}
