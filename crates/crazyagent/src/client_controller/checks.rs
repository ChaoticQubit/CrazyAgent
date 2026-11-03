use super::*;
use crate::engine::owner::{ChecksReply, ChecksRequest};

impl OwnerController {
    /// Sends one Checks request as the owner. Input: the request. Output: the engine's
    /// reply, or a `CHECKS_CODES` refusal.
    pub async fn checks(&self, request: ChecksRequest) -> Result<ChecksReply, EngineError> {
        todo!()
    }
}
