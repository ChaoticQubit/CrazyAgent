use super::*;
use crate::engine::owner::{AppInstructionSettings, ProcessText, Proposal, SavedText};
use crate::protocol::{CredentialRef, ProjectId};

impl OwnerController {
    fn process_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads the owner's instructions. Input: none for the app-wide text, or the project.
    /// Output: the text, empty when none is saved, and its version, or a `PROCESS_CODES`
    /// refusal.
    pub async fn process_text(
        &self,
        project: Option<ProjectId>,
    ) -> Result<ProcessText, EngineError> {
        self.process_client()?.process_text(project).await
    }

    /// Saves the owner's instructions whole when no other save landed since they were read.
    /// Inputs: none for the app-wide text, or the project; the text; the version read; and,
    /// only for "Keep mine" after a stale reply, the older version the owner opened.
    /// Output: the saved version with what agents now receive beside what a Claude master is
    /// sure to take, the stale reply with the newer text, or a refusal such as
    /// `process_text_too_large`.
    pub async fn save_process_text(
        &self,
        project: Option<ProjectId>,
        text: String,
        version: u64,
        opened: Option<u64>,
    ) -> Result<SavedText, EngineError> {
        self.process_client()?
            .save_process_text(project, text, version, opened)
            .await
    }

    /// Asks for a cleaned version of the saved instructions from a model with tools off;
    /// nothing is saved. Inputs: none for the app-wide text, or the project; and the pool
    /// the owner picked for this run, needed for the app-wide text and never taken for a
    /// project. Output: the proposal with the version it was made from, or an
    /// `instructions_cleanup_*` refusal.
    pub async fn propose_process(
        &self,
        project: Option<ProjectId>,
        pool: Option<CredentialRef>,
    ) -> Result<Proposal, EngineError> {
        self.process_client()?.propose_process(project, pool).await
    }

    /// Reads the app-wide instruction settings. Output: the settings, on when never stored.
    pub async fn app_instruction_settings(&self) -> Result<AppInstructionSettings, EngineError> {
        self.process_client()?.app_instruction_settings().await
    }

    /// Stores the app-wide instruction settings whole. Input: the settings. Output: the
    /// settings as stored, or a refusal after which nothing changed.
    pub async fn save_app_instruction_settings(
        &self,
        settings: AppInstructionSettings,
    ) -> Result<AppInstructionSettings, EngineError> {
        self.process_client()?
            .save_app_instruction_settings(settings)
            .await
    }

    /// Reads the owner's own instructions for one team agent. Inputs: the project and the
    /// agent id. Output: the text, empty when none is saved, and its version.
    pub async fn agent_text(
        &self,
        project: ProjectId,
        agent: i64,
    ) -> Result<ProcessText, EngineError> {
        self.process_client()?.agent_text(project, agent).await
    }

    /// Saves the owner's own instructions for one team agent when no other save landed since
    /// they were read. Inputs: the project, the agent id, the text, the version read and,
    /// only for "Keep mine" after a stale reply, the older version the owner opened.
    /// Output: the saved version, the stale reply with the newer text, or a refusal.
    pub async fn save_agent_text(
        &self,
        project: ProjectId,
        agent: i64,
        text: String,
        version: u64,
        opened: Option<u64>,
    ) -> Result<SavedText, EngineError> {
        self.process_client()?
            .save_agent_text(project, agent, text, version, opened)
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

    /// Lists the moved instruction files whose move the project folder has not committed,
    /// with its checked-out branch. Input: the project. Output: the paths, empty when no
    /// move waits for a commit, and the branch, none for a detached HEAD.
    pub async fn uncommitted_instruction_import(
        &self,
        project: ProjectId,
    ) -> Result<crate::engine::owner::UncommittedImport, EngineError> {
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
    /// Output: the settings, the defaults when the owner never changed them, with their
    /// version.
    pub async fn instruction_settings(
        &self,
        project: ProjectId,
    ) -> Result<crate::engine::owner::InstructionView, EngineError> {
        self.process_client()?.instruction_settings(project).await
    }

    /// Stores one project's agent instruction settings whole. Inputs: the project, the
    /// settings and the version the screen read, none when it could not read the stored
    /// settings and replaces them. Output: the settings as stored with their new version, or
    /// `stale_revision` when they changed since that version.
    pub async fn save_instruction_settings(
        &self,
        project: ProjectId,
        settings: crate::engine::owner::InstructionSettings,
        version: Option<u64>,
    ) -> Result<crate::engine::owner::InstructionView, EngineError> {
        self.process_client()?
            .save_instruction_settings(project, settings, version)
            .await
    }

    /// Reads one project's workflow adoption: its nudges option, the counted tools and
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
