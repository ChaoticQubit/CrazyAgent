use super::*;
use crate::protocol::ProjectId;

impl OwnerController {
    fn process_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads the owner's instructions. Input: none for the app-wide text, or the project.
    /// Output: the text, empty when none is saved, or a `PROCESS_CODES` refusal.
    pub async fn process_text(&self, project: Option<ProjectId>) -> Result<String, EngineError> {
        self.process_client()?.process_text(project).await
    }

    /// Saves the owner's instructions whole. Inputs: none for the app-wide text, or the
    /// project, and the text. Output: what agents now receive beside what a Claude master is
    /// sure to take, or a refusal such as `process_text_too_large`.
    pub async fn save_process_text(
        &self,
        project: Option<ProjectId>,
        text: String,
    ) -> Result<crate::engine::owner::ProcessSaved, EngineError> {
        self.process_client()?
            .save_process_text(project, text)
            .await
    }

    /// Reads one file the owner picked for the app-wide instructions editor; nothing is
    /// saved. Input: the absolute path. Output: the text, or a refusal such as
    /// `process_import_unsafe`.
    pub async fn read_instruction_file(
        &self,
        path: std::path::PathBuf,
    ) -> Result<String, EngineError> {
        self.process_client()?.read_instruction_file(path).await
    }

    /// Lists the project's own instruction files an import would copy and move, changing
    /// nothing. Input: the project. Output: the files, empty when none.
    pub async fn instruction_import(
        &self,
        project: ProjectId,
    ) -> Result<Vec<crate::engine::owner::ImportFile>, EngineError> {
        self.process_client()?.instruction_import(project).await
    }

    /// Copies the project's own instruction files into its project instructions and moves
    /// them into `.crazyagent/imported/`. Input: the project. Output: the moved files, each
    /// marked `uncommitted` while its move still waits for a commit.
    pub async fn confirm_instruction_import(
        &self,
        project: ProjectId,
    ) -> Result<Vec<crate::engine::owner::ImportFile>, EngineError> {
        self.process_client()?
            .confirm_instruction_import(project)
            .await
    }

    /// Lists the moved instruction files whose move the project folder has not committed.
    /// Input: the project. Output: the paths, empty when no move waits for a commit.
    pub async fn uncommitted_instruction_import(
        &self,
        project: ProjectId,
    ) -> Result<Vec<String>, EngineError> {
        self.process_client()?
            .uncommitted_instruction_import(project)
            .await
    }

    /// Commits the instruction import's move as one local commit holding only the moved
    /// files, never pushed. Input: the project. Output: the commit, or a refusal such as
    /// `process_commit_staged`.
    pub async fn commit_instruction_import(
        &self,
        project: ProjectId,
    ) -> Result<crate::engine::owner::ImportCommit, EngineError> {
        self.process_client()?
            .commit_instruction_import(project)
            .await
    }

    /// Reads the CrazyAgent default process for a preview.
    pub async fn default_process(&self) -> Result<String, EngineError> {
        self.process_client()?.default_process().await
    }

    /// Reads one project's agent instruction settings. Input: the project.
    /// Output: the settings, the defaults when the owner never changed them.
    pub async fn instruction_settings(
        &self,
        project: ProjectId,
    ) -> Result<crate::engine::owner::InstructionSettings, EngineError> {
        self.process_client()?.instruction_settings(project).await
    }

    /// Stores one project's agent instruction settings whole. Inputs: the project and the
    /// settings. Output: the settings as stored.
    pub async fn save_instruction_settings(
        &self,
        project: ProjectId,
        settings: crate::engine::owner::InstructionSettings,
    ) -> Result<crate::engine::owner::InstructionSettings, EngineError> {
        self.process_client()?
            .save_instruction_settings(project, settings)
            .await
    }

    /// Reads one project's workflow adoption: whether nudges are on, the counted tools and
    /// each recent agent's counts, flags and reminders. Input: the project. Output: the view.
    pub async fn adoption(
        &self,
        project: ProjectId,
    ) -> Result<crate::engine::owner::Adoption, EngineError> {
        self.process_client()?.adoption(project).await
    }

    /// Reads what one team agent received at its last launch. Inputs: the project and the
    /// agent id, or none for the project's newest master. Output: the read-only view.
    pub async fn agent_instructions(
        &self,
        project: ProjectId,
        agent: Option<i64>,
    ) -> Result<crate::engine::owner::AgentInstructions, EngineError> {
        self.process_client()?
            .agent_instructions(project, agent)
            .await
    }
}
