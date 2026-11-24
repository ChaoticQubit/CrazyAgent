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

/// Whether a confined master turn's sandbox no longer keeps it out of every folder its
/// project saves hidden now, as [`kernel_sandbox::hidden_paths::HiddenRun::covers_saved`]
/// reads the folders [`master_turn_hides`] gave its run list; a turn at Full access hides
/// nothing and never is. Inputs: the prepared turn and the saved folders, none when they
/// could not be read, which counts as not kept out. Output: the answer.
pub(super) fn unhid(prepared: &PreparedMasterTurn, saved: Option<&[std::path::PathBuf]>) -> bool {
    if prepared.agent_policy.access == kernel_sandbox::policy::AgentAccess::Full {
        return false;
    }
    let digests: Vec<String> = master_turn_hides(prepared)
        .iter()
        .map(|folder| kernel_sandbox::hidden_paths::digest(folder))
        .collect();
    saved.is_none_or(|saved| !kernel_sandbox::hidden_paths::covers(Some(&digests), saved))
}
