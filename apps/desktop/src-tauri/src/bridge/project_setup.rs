use super::{BridgeError, IntentReceipt, NativeBridge, bounded, failure, native_project_id};
use crazyagent::engine::owner::OwnerMutation;
use crazyagent::project_setup::{
    ContextChunk, ProjectCatalog, ProjectContext, ProjectSetup, ResearchJob, ResearchKind,
    StoredSetup,
};
use crazyagent::protocol::{CredentialRef, ProjectId, ProjectSpec};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ResearchJobs {
    pub project_id: String,
    pub jobs: Vec<ResearchJob>,
}

fn read_failure(code: &str) -> BridgeError {
    failure(match code {
        "pool_missing" => "pool_missing",
        "research_duplicate" => "research_duplicate",
        "project_setup_missing" => "project_setup_missing",
        "context_path_invalid" => "context_path_invalid",
        "context_item_missing" => "context_item_missing",
        "context_offset_invalid" => "context_offset_invalid",
        _ => "owner_read_failed",
    })
}

fn project(id: &str) -> Result<ProjectId, BridgeError> {
    ProjectId::new(id).map_err(|_| failure("invalid_project"))
}

impl NativeBridge {
    fn ready_for_intent(&self, generation: u64) -> Result<(), BridgeError> {
        self.allow_new_auth()?;
        if self.status().pending {
            return Err(failure("pending_intent"));
        }
        if generation == 0 || generation > 9_007_199_254_740_991 {
            return Err(failure("draft_generation_limit"));
        }
        Ok(())
    }

    /// Reads the New Project choices and smart team defaults.
    /// Takes an optional pool id and the chosen research toggles.
    /// Returns the engine catalog or a plain failure code.
    pub async fn project_catalog(
        &mut self,
        pool: Option<String>,
        research: Vec<ResearchKind>,
    ) -> Result<ProjectCatalog, BridgeError> {
        let pool = pool
            .map(CredentialRef::new)
            .transpose()
            .map_err(|_| failure("invalid_pool"))?;
        let catalog = self
            .controller()?
            .project_catalog(pool, research)
            .await
            .map_err(|e| read_failure(&e.0))?;
        bounded(catalog)
    }

    /// Creates a project with its folder, documents, team plan and research jobs.
    /// Takes the name spec, the full setup and the draft generation.
    /// Returns the retained intent receipt with any plain rejection.
    pub async fn setup_project(
        &mut self,
        spec: ProjectSpec,
        setup: ProjectSetup,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.ready_for_intent(generation)?;
        spec.validate()
            .map_err(|_| failure("project_spec_bounds"))?;
        setup
            .validate()
            .map_err(|_| failure("project_setup_bounds"))?;
        let project_id = native_project_id()?;
        self.controller()?
            .project_catalog(setup.pool.clone(), setup.research.clone())
            .await
            .map_err(|e| read_failure(&e.0))?;
        self.controller()?
            .begin_mutation(
                OwnerMutation::SetupProject {
                    project_id,
                    spec,
                    setup,
                },
                generation,
            )
            .map_err(|_| failure("owner_not_ready"))?;
        assert!(self.status().pending);
        self.deliver_intent(false).await
    }

    /// Reads the stored setup of one project.
    /// Takes the project id.
    /// Returns the setup, or none for a project created without one.
    pub async fn project_setup(&mut self, id: &str) -> Result<Option<StoredSetup>, BridgeError> {
        let setup = self
            .controller()?
            .project_setup(project(id)?)
            .await
            .map_err(|e| read_failure(&e.0))?;
        bounded(setup)
    }

    /// Lists a project's research jobs with plain status labels.
    /// Takes the project id.
    /// Returns the jobs in creation order.
    pub async fn research_jobs(&mut self, id: &str) -> Result<ResearchJobs, BridgeError> {
        let jobs = self
            .controller()?
            .research_jobs(project(id)?)
            .await
            .map_err(|e| read_failure(&e.0))?;
        bounded(ResearchJobs {
            project_id: id.into(),
            jobs,
        })
    }

    /// Lists the brief, documents and research reports of a project folder.
    /// Takes the project id.
    /// Returns the folder path and its context items.
    pub async fn project_context(&mut self, id: &str) -> Result<ProjectContext, BridgeError> {
        let context = self
            .controller()?
            .project_context(project(id)?)
            .await
            .map_err(|e| read_failure(&e.0))?;
        bounded(context)
    }

    /// Reads one bounded chunk of a context item.
    /// Takes the project id, the item path relative to the project folder and a byte offset.
    /// Returns the chunk with the next offset when more text remains.
    pub async fn project_context_item(
        &mut self,
        id: &str,
        path: String,
        offset: u64,
    ) -> Result<ContextChunk, BridgeError> {
        let chunk = self
            .controller()?
            .project_context_item(project(id)?, path, offset)
            .await
            .map_err(|e| read_failure(&e.0))?;
        bounded(chunk)
    }

    /// Saves a research report, or one section, as a pending memory for owner approval.
    /// Takes the project id, the research kind, an optional heading and the draft generation.
    /// Returns the retained intent receipt with any plain rejection.
    pub async fn save_research_to_memory(
        &mut self,
        id: &str,
        research: ResearchKind,
        section: Option<String>,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.ready_for_intent(generation)?;
        let project_id = project(id)?;
        self.controller()?
            .project_setup(project_id.clone())
            .await
            .map_err(|e| read_failure(&e.0))?;
        self.controller()?
            .begin_mutation(
                OwnerMutation::SaveResearchToMemory {
                    project_id,
                    research,
                    section,
                },
                generation,
            )
            .map_err(|_| failure("owner_not_ready"))?;
        self.deliver_intent(false).await
    }
}
