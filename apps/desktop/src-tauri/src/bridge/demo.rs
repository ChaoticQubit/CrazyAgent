use super::project_setup::read_failure;
use super::{BridgeError, IntentReceipt, NativeBridge, caused, failure};
use crazyagent::demo::{DemoView, PROJECT_ID};
use crazyagent::engine::owner::OwnerMutation;
use crazyagent::protocol::{CredentialRef, ProjectId};

impl NativeBridge {
    /// Reads the demo (DEC-153): the demo project while it exists, its steps, the goal words
    /// and every pool with whether the demo can run on it. Input: none. Output: the engine's
    /// demo view, or why it could not be read.
    pub async fn project_demo(&self) -> Result<DemoView, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.reader()?
            .project_demo()
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))
    }

    /// Makes the demo project on the pool the owner picked, as one retained owner change with
    /// the demo's one fixed id. The window gives the pool to the demo itself once the receipt
    /// is applied, as New Project does. Inputs: the pool id and the intent generation.
    /// Output: the receipt, carrying the demo's id; a refusal carries a demo code.
    pub async fn create_demo(
        &mut self,
        pool: String,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.ready_for_intent(generation)?;
        let pool = CredentialRef::new(pool).map_err(|_| failure("demo_pool_missing"))?;
        let project_id =
            ProjectId::new(PROJECT_ID).map_err(|_| failure("demo_project_id_invalid"))?;
        self.controller()?
            .project_demo()
            .await
            .map_err(|error| read_failure(&error.0))?;
        self.controller()?
            .begin_mutation(OwnerMutation::CreateDemo { project_id, pool }, generation)
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        assert!(self.status().pending);
        self.deliver_intent(false).await
    }
}
