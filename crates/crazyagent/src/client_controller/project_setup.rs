use super::*;
use crate::project_setup::{
    ContextChunk, ProjectCatalog, ProjectContext, ProjectSetup, ResearchJob, ResearchKind,
    StoredSetup,
};
use crate::protocol::{CredentialRef, ProjectId, ProjectSpec};

impl OwnerController {
    /// Reads the New Project choices and the smart team defaults for a pool and toggles.
    /// Takes the chosen pool, if any, and the chosen research toggles.
    /// Returns the engine catalog, or the engine's plain rejection code.
    pub async fn project_catalog(
        &mut self,
        pool: Option<CredentialRef>,
        research: Vec<ResearchKind>,
    ) -> Result<ProjectCatalog, EngineError> {
        match self
            .read(OwnerRead::ProjectCatalog { pool, research })
            .await?
            .data
        {
            OwnerData::ProjectCatalog { catalog } => Ok(*catalog),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Creates a project with its folder, documents, team plan and research jobs.
    /// Takes the new project id, its name spec, the full setup and the client generation.
    /// Returns the actual owner response; a rejection carries a plain code.
    pub async fn setup_project(
        &mut self,
        project_id: ProjectId,
        spec: ProjectSpec,
        setup: ProjectSetup,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        self.project_catalog(setup.pool.clone(), setup.research.clone())
            .await?;
        self.begin_mutation(
            OwnerMutation::SetupProject {
                project_id,
                spec,
                setup,
            },
            generation,
        )?;
        self.send_pending().await
    }

    /// Reads the stored setup of one project.
    /// Takes the project id.
    /// Returns the setup, or none for a project created without one.
    pub async fn project_setup(
        &mut self,
        project_id: ProjectId,
    ) -> Result<Option<StoredSetup>, EngineError> {
        match self
            .read(OwnerRead::ProjectSetup { project_id })
            .await?
            .data
        {
            OwnerData::ProjectSetup { setup, .. } => Ok(setup.map(|setup| *setup)),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Lists a project's research jobs with plain status labels.
    /// Takes the project id.
    /// Returns the jobs in creation order.
    pub async fn research_jobs(
        &mut self,
        project_id: ProjectId,
    ) -> Result<Vec<ResearchJob>, EngineError> {
        match self
            .read(OwnerRead::ResearchJobs { project_id })
            .await?
            .data
        {
            OwnerData::ResearchJobs { jobs, .. } => Ok(jobs),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Lists the brief, documents and research reports of a project folder.
    /// Takes the project id.
    /// Returns the folder path and its context items.
    pub async fn project_context(
        &mut self,
        project_id: ProjectId,
    ) -> Result<ProjectContext, EngineError> {
        match self
            .read(OwnerRead::ProjectContext { project_id })
            .await?
            .data
        {
            OwnerData::ProjectContext { context } => Ok(context),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Reads one bounded chunk of a context item.
    /// Takes the project id, the item path relative to the project folder and a byte offset.
    /// Returns the chunk with the next offset when more text remains.
    pub async fn project_context_item(
        &mut self,
        project_id: ProjectId,
        path: String,
        offset: u64,
    ) -> Result<ContextChunk, EngineError> {
        let query = OwnerRead::ProjectContextItem {
            project_id,
            path,
            offset,
        };
        match self.read(query).await?.data {
            OwnerData::ProjectContextItem { chunk } => Ok(chunk),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Saves a research report, or one of its sections, as a pending memory for owner approval.
    /// Takes the project id, research kind, optional heading and the client generation.
    /// Returns the actual owner response; approval is the existing knowledge Review Keep.
    pub async fn save_research_to_memory(
        &mut self,
        project_id: ProjectId,
        research: ResearchKind,
        section: Option<String>,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        self.project_setup(project_id.clone()).await?;
        self.begin_mutation(
            OwnerMutation::SaveResearchToMemory {
                project_id,
                research,
                section,
            },
            generation,
        )?;
        self.send_pending().await
    }

    /// Copies more owner-picked documents into an existing project and refreshes its brief.
    /// Takes the project id, absolute source paths and the client generation.
    /// Returns the actual owner response; a duplicate or oversized document is a plain rejection.
    pub async fn add_project_documents(
        &mut self,
        project_id: ProjectId,
        documents: Vec<String>,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        self.project_setup(project_id.clone()).await?;
        self.begin_mutation(
            OwnerMutation::AddProjectDocuments {
                project_id,
                documents,
            },
            generation,
        )?;
        self.send_pending().await
    }

    /// Deletes one document or research report from a project folder; the brief is protected.
    /// Takes the project id, the item path relative to the project folder and the client generation.
    /// Returns the actual owner response; links, folders and absent items are plain rejections.
    pub async fn delete_project_context(
        &mut self,
        project_id: ProjectId,
        path: String,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        self.project_setup(project_id.clone()).await?;
        self.begin_mutation(
            OwnerMutation::DeleteProjectContext { project_id, path },
            generation,
        )?;
        self.send_pending().await
    }
}
