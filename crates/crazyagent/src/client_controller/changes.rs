use super::*;
use crate::landing::{
    AcceptOutcome, ChangeDiff, ChangeSetId, ChangeSetView, TargetBranches, WaitingChanges,
};
use crate::protocol::ProjectId;

impl OwnerController {
    fn changes_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Lists one project's change sets, newest first, at most `landing::LIST_MAX`.
    /// Input: the project. Output: each change set with its agent, state, line counts
    /// and restore window, or a `CHANGES_CODES` refusal.
    pub async fn change_sets(&self, project: ProjectId) -> Result<Vec<ChangeSetView>, EngineError> {
        self.changes_client()?.change_sets(project).await
    }

    /// Counts the change sets waiting for the owner in Changes, across the projects the engine
    /// has, and the given project's among them. Input: the project, if any. Output: the
    /// counts of change sets ready to review or conflicting, or a `CHANGES_CODES` refusal; an
    /// older engine refuses it.
    pub async fn waiting_changes(
        &self,
        project: Option<ProjectId>,
    ) -> Result<WaitingChanges, EngineError> {
        self.changes_client()?.waiting_changes(project).await
    }

    /// Reads one page of a change set's diff at the head the owner is reading; callers ask
    /// for the next page only when the owner reaches it. Inputs: the change set, that head
    /// and the page, 0 first. Output: the page's files with their marks, its plain text and
    /// the next page, or `changes_stale` once the set moved to another head.
    pub async fn change_diff(
        &self,
        id: ChangeSetId,
        head: String,
        page: u16,
    ) -> Result<ChangeDiff, EngineError> {
        self.changes_client()?.change_diff(id, head, page).await
    }

    /// Lands a change set as one squash commit on the owner's checked-out branch.
    /// Inputs: the change set, the head the owner reviewed and the owner's edited
    /// message, None for the proposed one. Output: the landed commit and branch, or the
    /// files it conflicts on.
    pub async fn accept_changes(
        &self,
        id: ChangeSetId,
        expected_head: String,
        message: Option<String>,
    ) -> Result<AcceptOutcome, EngineError> {
        self.changes_client()?
            .accept_changes(id, expected_head, message)
            .await
    }

    /// Lands a change set as one squash commit on a chosen local branch; a branch not
    /// checked out moves as a ref and the owner's files stay as they are. Inputs: the
    /// change set, the reviewed head, the edited message and the target branch, None for
    /// the checked-out one. Output: the landed commit and branch, or the conflicting files.
    pub async fn accept_changes_into(
        &self,
        id: ChangeSetId,
        expected_head: String,
        message: Option<String>,
        target: Option<String>,
    ) -> Result<AcceptOutcome, EngineError> {
        self.changes_client()?
            .accept_changes_into(id, expected_head, message, target)
            .await
    }

    /// Lists the local branches a change set can be accepted onto and the checked-out
    /// one. Input: the change set. Output: the branches, or a `CHANGES_CODES` refusal.
    pub async fn change_branches(&self, id: ChangeSetId) -> Result<TargetBranches, EngineError> {
        self.changes_client()?.change_branches(id).await
    }

    /// Discards a change set, keeping it restorable. Inputs: the change set and the head
    /// the owner reviewed. Output: the time it stays restorable until, in milliseconds.
    pub async fn discard_changes(
        &self,
        id: ChangeSetId,
        expected_head: String,
    ) -> Result<u64, EngineError> {
        self.changes_client()?
            .discard_changes(id, expected_head)
            .await
    }

    /// Brings a discarded change set back as pending. Input: the change set.
    /// Output: Ok once pending, or a refusal such as `changes_restore_expired`.
    pub async fn restore_changes(&self, id: ChangeSetId) -> Result<(), EngineError> {
        self.changes_client()?.restore_changes(id).await
    }

    /// Undoes a change set its own Accept merged, with one revert commit. Input: the
    /// change set. Output: the undo commit and branch, the conflicting files, or a refusal
    /// such as `changes_undo_unavailable`.
    pub async fn undo_changes(&self, id: ChangeSetId) -> Result<AcceptOutcome, EngineError> {
        self.changes_client()?.undo_changes(id).await
    }

    /// Asks the agent to update a conflicting change set: the engine merges the owner's
    /// branch into the agent's worktree and starts the resolve task. Input: the change
    /// set. Output: Ok once merged, or a refusal such as `changes_agent_busy`.
    pub async fn update_changes(&self, id: ChangeSetId) -> Result<(), EngineError> {
        self.changes_client()?.update_changes(id).await
    }
}
