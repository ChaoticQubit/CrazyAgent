use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{CHANGES_CODES, OwnerClient};
use crazyagent::landing::{
    AcceptOutcome, ChangeDiff, ChangeSetId, ChangeSetView, OwnerDirty, TargetBranches,
};
use crazyagent::protocol::ProjectId;
use std::sync::Arc;

pub(super) fn changes_error(error: EngineError) -> BridgeError {
    if error.0 == "controller_disconnected" {
        return failure("not_connected");
    }
    caused(
        CHANGES_CODES
            .iter()
            .copied()
            .find(|known| *known == error.0)
            .unwrap_or("changes_outcome_unknown"),
        &error.0,
    )
}

fn change_set(id: String) -> Result<ChangeSetId, BridgeError> {
    ChangeSetId::new(id).map_err(changes_error)
}

impl NativeBridge {
    fn changes_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(changes_error)
    }

    /// Lists one project's change sets, newest first, run without the bridge lock.
    /// Input: the project id. Output: each change set with its agent, state and size.
    pub fn change_sets(
        &self,
        project_id: String,
    ) -> Result<Unlocked<Vec<ChangeSetView>>, BridgeError> {
        let project = ProjectId::new(project_id).map_err(|_| failure("changes_not_found"))?;
        let client = self.changes_client()?;
        Ok(Box::pin(async move {
            client.change_sets(project).await.map_err(changes_error)
        }))
    }

    /// Reads one page of a change set's diff as plain text at the head the owner is
    /// reading, run without the bridge lock. Inputs: the change set id, that head and the
    /// page. Output: the page, or `changes_stale` once the set moved to another head.
    pub fn change_diff(
        &self,
        id: String,
        head: String,
        page: u16,
    ) -> Result<Unlocked<ChangeDiff>, BridgeError> {
        let id = change_set(id)?;
        let client = self.changes_client()?;
        Ok(Box::pin(async move {
            client
                .change_diff(id, head, page)
                .await
                .map_err(changes_error)
        }))
    }

    /// Lands a change set on a local branch, the checked-out one when none is named, run
    /// without the bridge lock. Inputs: the change set id, the head the owner reviewed,
    /// the owner's edited message, None for the proposed one, and the target branch.
    /// Output: the landed commit and branch, or the conflicting files.
    pub fn accept_change(
        &self,
        id: String,
        expected_head: String,
        message: Option<String>,
        target: Option<String>,
    ) -> Result<Unlocked<AcceptOutcome>, BridgeError> {
        let id = change_set(id)?;
        let client = self.changes_client()?;
        Ok(Box::pin(async move {
            client
                .accept_changes_into(id, expected_head, message, target)
                .await
                .map_err(changes_error)
        }))
    }

    /// Lists the local branches a change set can be accepted onto, run without the bridge
    /// lock. Input: the change set id. Output: the branches and the checked-out one.
    pub fn change_branches(&self, id: String) -> Result<Unlocked<TargetBranches>, BridgeError> {
        let id = change_set(id)?;
        let client = self.changes_client()?;
        Ok(Box::pin(async move {
            client.change_branches(id).await.map_err(changes_error)
        }))
    }

    /// Lists the files in the project folder that keep a change set's Accept from landing,
    /// run without the bridge lock. Inputs: the change set id and the target branch, None
    /// for the Land on branch. Output: the tracked and untracked files, empty when none does.
    pub fn change_owner_dirty(
        &self,
        id: String,
        target: Option<String>,
    ) -> Result<Unlocked<OwnerDirty>, BridgeError> {
        let id = change_set(id)?;
        let client = self.changes_client()?;
        Ok(Box::pin(async move {
            client.owner_dirty(id, target).await.map_err(changes_error)
        }))
    }

    /// Commits only the listed project folder files that keep an Accept from landing, with
    /// the owner's message, run without the bridge lock. Inputs: the change set id, the
    /// files, the message and the target branch, None for the Land on branch. Output: the
    /// commit and its branch.
    pub fn commit_owner_files(
        &self,
        id: String,
        files: Vec<String>,
        message: String,
        target: Option<String>,
    ) -> Result<Unlocked<(String, String)>, BridgeError> {
        let id = change_set(id)?;
        let client = self.changes_client()?;
        Ok(Box::pin(async move {
            client
                .commit_owner_files(id, files, message, target)
                .await
                .map_err(changes_error)
        }))
    }

    /// Discards a change set, restorable for seven days, run without the bridge lock.
    /// Inputs: the change set id and the head the owner reviewed. Output: the time it
    /// stays restorable until, in milliseconds.
    pub fn discard_change(
        &self,
        id: String,
        expected_head: String,
    ) -> Result<Unlocked<u64>, BridgeError> {
        let id = change_set(id)?;
        let client = self.changes_client()?;
        Ok(Box::pin(async move {
            client
                .discard_changes(id, expected_head)
                .await
                .map_err(changes_error)
        }))
    }

    /// Brings a discarded change set back as pending, run without the bridge lock.
    /// Input: the change set id.
    pub fn restore_change(&self, id: String) -> Result<Unlocked<()>, BridgeError> {
        let id = change_set(id)?;
        let client = self.changes_client()?;
        Ok(Box::pin(async move {
            client.restore_changes(id).await.map_err(changes_error)
        }))
    }

    /// Undoes a change set its own Accept merged, run without the bridge lock. Input: the
    /// change set id. Output: the undo commit and branch, or the conflicting files.
    pub fn undo_change(&self, id: String) -> Result<Unlocked<AcceptOutcome>, BridgeError> {
        let id = change_set(id)?;
        let client = self.changes_client()?;
        Ok(Box::pin(async move {
            client.undo_changes(id).await.map_err(changes_error)
        }))
    }

    /// Asks the agent to update a conflicting change set onto the owner's branch, run
    /// without the bridge lock. Input: the change set id.
    pub fn update_change(&self, id: String) -> Result<Unlocked<()>, BridgeError> {
        let id = change_set(id)?;
        let client = self.changes_client()?;
        Ok(Box::pin(async move {
            client.update_changes(id).await.map_err(changes_error)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every changes code the engine answers reaches the window as itself, so the owner
    /// reads its sentence; anything else is an unknown outcome with the engine code behind.
    #[test]
    fn engine_changes_codes_pass_through_unchanged() {
        for code in CHANGES_CODES {
            assert_eq!(changes_error(EngineError((*code).into())).code, *code);
        }
        assert_eq!(
            changes_error(EngineError("controller_disconnected".into())).code,
            "not_connected"
        );
        let unlisted = changes_error(EngineError("changes_state_torn".into()));
        assert_eq!(unlisted.code, "changes_outcome_unknown");
        assert_eq!(unlisted.cause.as_deref(), Some("changes_state_torn"));
        let raw = changes_error(EngineError("open /Users/mira/repo: denied".into()));
        assert_eq!(raw.code, "changes_outcome_unknown");
        assert_eq!(
            raw.cause.as_deref(),
            Some(crazyagent::protocol::TEXT_WITHHELD)
        );
        assert_eq!(
            change_set("not-an-id".into()).unwrap_err().code,
            "changes_not_found"
        );
    }
}
