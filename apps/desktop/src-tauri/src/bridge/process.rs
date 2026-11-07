use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{
    Adoption, AgentInstructions, ImportCommit, ImportFile, InstructionSettings, InstructionView,
    OwnerClient, PROCESS_CODES, ProcessText, SavedText, UncommittedImport,
};
use crazyagent::protocol::ProjectId;
use std::path::PathBuf;
use std::sync::Arc;

fn process_error(error: EngineError) -> BridgeError {
    if error.0 == "controller_disconnected" {
        return failure("not_connected");
    }
    caused(
        PROCESS_CODES
            .iter()
            .copied()
            .find(|known| *known == error.0)
            .unwrap_or("process_outcome_unknown"),
        &error.0,
    )
}

fn project(project_id: Option<String>) -> Result<Option<ProjectId>, BridgeError> {
    project_id
        .map(|id| ProjectId::new(id).map_err(|_| failure("process_project_unknown")))
        .transpose()
}

impl NativeBridge {
    fn process_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(process_error)
    }

    /// Reads the owner's instructions, run without the bridge lock. Input: none for the
    /// app-wide text, or the project id. Output: the text, empty when none is saved, and the
    /// version a save names.
    pub fn process_text(
        &self,
        project_id: Option<String>,
    ) -> Result<Unlocked<ProcessText>, BridgeError> {
        let project = project(project_id)?;
        let client = self.process_client()?;
        Ok(Box::pin(async move {
            client.process_text(project).await.map_err(process_error)
        }))
    }

    /// Saves the owner's instructions whole, run without the bridge lock, when no other save
    /// landed since they were read. Inputs: none for the app-wide text, or the project id;
    /// the text; the version read; and only for "Keep mine" after a stale reply, the older
    /// version the owner opened. Output: the saved version with, for app or project text,
    /// what agents now get beside what a Claude master takes, or the stale reply with the
    /// newer text.
    pub fn save_process_text(
        &self,
        project_id: Option<String>,
        text: String,
        version: u64,
        opened: Option<u64>,
    ) -> Result<Unlocked<SavedText>, BridgeError> {
        let project = project(project_id)?;
        let client = self.process_client()?;
        Ok(Box::pin(async move {
            client
                .save_process_text(project, text, version, opened)
                .await
                .map_err(process_error)
        }))
    }

    /// Reads one instructions file the owner picked in the native chooser, run without the
    /// bridge lock; nothing is saved. Input: the absolute path the chooser returned. Output:
    /// the text for the app-wide editor.
    pub fn read_instruction_file(&self, path: String) -> Result<Unlocked<String>, BridgeError> {
        let client = self.process_client()?;
        Ok(Box::pin(async move {
            client
                .read_instruction_file(PathBuf::from(path))
                .await
                .map_err(process_error)
        }))
    }

    /// Lists the project's own instruction files an import would copy into its instructions
    /// and move aside, run without the bridge lock; nothing changes. Input: the project id.
    /// Output: the files, each with why it would stay, empty when there is nothing to import.
    pub fn instruction_import(
        &self,
        project_id: String,
    ) -> Result<Unlocked<Vec<ImportFile>>, BridgeError> {
        let project =
            project(Some(project_id))?.ok_or_else(|| failure("process_project_unknown"))?;
        let client = self.process_client()?;
        Ok(Box::pin(async move {
            client
                .instruction_import(project)
                .await
                .map_err(process_error)
        }))
    }

    /// Copies the project's own instruction files into its instructions and moves them into
    /// `.crazyagent/imported/`, run without the bridge lock, on the owner's confirm only.
    /// Input: the project id. Output: the moved files.
    pub fn confirm_instruction_import(
        &self,
        project_id: String,
    ) -> Result<Unlocked<Vec<ImportFile>>, BridgeError> {
        let project =
            project(Some(project_id))?.ok_or_else(|| failure("process_project_unknown"))?;
        let client = self.process_client()?;
        Ok(Box::pin(async move {
            client
                .confirm_instruction_import(project)
                .await
                .map_err(process_error)
        }))
    }

    /// Lists the moved instruction files whose move the project folder has not committed,
    /// with its checked-out branch, run without the bridge lock. Input: the project id.
    /// Output: the paths, empty when no move waits for a commit, and the branch.
    pub fn uncommitted_instruction_import(
        &self,
        project_id: String,
    ) -> Result<Unlocked<UncommittedImport>, BridgeError> {
        let project =
            project(Some(project_id))?.ok_or_else(|| failure("process_project_unknown"))?;
        let client = self.process_client()?;
        Ok(Box::pin(async move {
            client
                .uncommitted_instruction_import(project)
                .await
                .map_err(process_error)
        }))
    }

    /// Commits the instruction import's move as one local commit holding only the moved
    /// files, never pushed, run without the bridge lock, on the owner's click only.
    /// Input: the project id. Output: the commit, its branch and the files it holds.
    pub fn commit_instruction_import(
        &self,
        project_id: String,
    ) -> Result<Unlocked<ImportCommit>, BridgeError> {
        let project =
            project(Some(project_id))?.ok_or_else(|| failure("process_project_unknown"))?;
        let client = self.process_client()?;
        Ok(Box::pin(async move {
            client
                .commit_instruction_import(project)
                .await
                .map_err(process_error)
        }))
    }

    /// Reads the CrazyAgent default process for a preview, run without the bridge lock.
    pub fn default_process(&self) -> Result<Unlocked<String>, BridgeError> {
        let client = self.process_client()?;
        Ok(Box::pin(async move {
            client.default_process().await.map_err(process_error)
        }))
    }

    /// Reads what a launch now gives every agent of the project, as the engine renders it
    /// for the project's mode, run without the bridge lock. Input: the project id. Output:
    /// the text, or the code of the part the engine cannot read.
    pub fn given_text(&self, project_id: String) -> Result<Unlocked<String>, BridgeError> {
        let project =
            project(Some(project_id))?.ok_or_else(|| failure("process_project_unknown"))?;
        let client = self.process_client()?;
        Ok(Box::pin(async move {
            client.given_text(project).await.map_err(process_error)
        }))
    }

    /// Reads what one team agent received at its last launch, run without the bridge lock.
    /// Inputs: the project id and the agent id, or none for the project's newest master.
    /// Output: the read-only view.
    pub fn agent_instructions(
        &self,
        project_id: String,
        agent: Option<i64>,
    ) -> Result<Unlocked<AgentInstructions>, BridgeError> {
        let project =
            project(Some(project_id))?.ok_or_else(|| failure("process_project_unknown"))?;
        let client = self.process_client()?;
        Ok(Box::pin(async move {
            client
                .agent_instructions(project, agent)
                .await
                .map_err(process_error)
        }))
    }

    /// Reads one project's agent instruction settings, run without the bridge lock.
    /// Input: the project id. Output: the settings, the defaults when never changed, with
    /// their version.
    pub fn instruction_settings(
        &self,
        project_id: String,
    ) -> Result<Unlocked<InstructionView>, BridgeError> {
        let project =
            project(Some(project_id))?.ok_or_else(|| failure("process_project_unknown"))?;
        let client = self.process_client()?;
        Ok(Box::pin(async move {
            client
                .instruction_settings(project)
                .await
                .map_err(process_error)
        }))
    }

    /// Reads one project's workflow adoption, run without the bridge lock. Input: the
    /// project id. Output: its nudges option, the counted tools and each recent agent's
    /// counts, flags and reminders.
    pub fn adoption(&self, project_id: String) -> Result<Unlocked<Adoption>, BridgeError> {
        let project =
            project(Some(project_id))?.ok_or_else(|| failure("process_project_unknown"))?;
        let client = self.process_client()?;
        Ok(Box::pin(async move {
            client.adoption(project).await.map_err(process_error)
        }))
    }

    /// Stores one project's agent instruction settings whole, run without the bridge lock.
    /// Inputs: the project id, the settings and the version the page read, none when it
    /// could not read the stored settings and replaces them. Output: the settings as stored
    /// with their new version, or `stale_revision` when they changed since that version.
    pub fn save_instruction_settings(
        &self,
        project_id: String,
        settings: InstructionSettings,
        version: Option<u64>,
    ) -> Result<Unlocked<InstructionView>, BridgeError> {
        let project =
            project(Some(project_id))?.ok_or_else(|| failure("process_project_unknown"))?;
        let client = self.process_client()?;
        Ok(Box::pin(async move {
            client
                .save_instruction_settings(project, settings, version)
                .await
                .map_err(process_error)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every Process code the engine answers reaches the window as itself, so the owner
    /// reads its sentence; anything else is an unknown outcome with the engine code behind.
    #[test]
    fn engine_process_codes_pass_through_unchanged() {
        for code in PROCESS_CODES {
            assert_eq!(process_error(EngineError((*code).into())).code, *code);
        }
        assert_eq!(
            process_error(EngineError("controller_disconnected".into())).code,
            "not_connected"
        );
        let unlisted = process_error(EngineError("process_state_torn".into()));
        assert_eq!(unlisted.code, "process_outcome_unknown");
        assert_eq!(unlisted.cause.as_deref(), Some("process_state_torn"));
        let raw = process_error(EngineError("open /Users/mira/notes: denied".into()));
        assert_eq!(raw.code, "process_outcome_unknown");
        assert_eq!(
            raw.cause.as_deref(),
            Some(crazyagent::protocol::TEXT_WITHHELD)
        );
        assert_eq!(
            project(Some("not an id".into())).unwrap_err().code,
            "process_project_unknown"
        );
        assert_eq!(project(None).unwrap(), None);
    }
}
