use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{CHANGES_CODES, OwnerClient};
use crazyagent::landing::{AcceptOutcome, ChangeDiff, ChangeSetId, ChangeSetView};
use crazyagent::protocol::ProjectId;
use std::sync::Arc;

fn changes_error(error: EngineError) -> BridgeError {
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

    /// Reads one page of a change set's diff as plain text, run without the bridge lock.
    /// Inputs: the change set id and the page. Output: the page.
    pub fn change_diff(&self, id: String, page: u16) -> Result<Unlocked<ChangeDiff>, BridgeError> {
        let id = change_set(id)?;
        let client = self.changes_client()?;
        Ok(Box::pin(async move {
            client.change_diff(id, page).await.map_err(changes_error)
        }))
    }

    /// Lands a change set on the owner's checked-out branch, run without the bridge lock.
    /// Inputs: the change set id and the head the owner reviewed. Output: the landed
    /// commit and branch, or the conflicting files.
    pub fn accept_change(
        &self,
        id: String,
        expected_head: String,
    ) -> Result<Unlocked<AcceptOutcome>, BridgeError> {
        let id = change_set(id)?;
        let client = self.changes_client()?;
        Ok(Box::pin(async move {
            client
                .accept_changes(id, expected_head)
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
