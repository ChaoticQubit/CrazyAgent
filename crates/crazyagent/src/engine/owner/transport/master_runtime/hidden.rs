use crate::storage::PreparedMasterTurn;

/// The folders a headless master turn's sandbox keeps it out of, as the hidden folders list
/// records them: the native and the RCLI sandbox hide `hidden_paths`, and under Full access
/// the turn runs unconfined and hides none (SA-VID14 r1 sec F3). Input: the prepared turn.
/// Output: the folders.
pub(super) fn master_turn_hides(prepared: &PreparedMasterTurn) -> &[std::path::PathBuf] {
    if prepared.agent_policy.access == kernel_sandbox::policy::AgentAccess::Full {
        &[]
    } else {
        &prepared.hidden_paths
    }
}
