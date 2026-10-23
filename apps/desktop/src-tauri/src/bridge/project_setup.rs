use super::{BridgeError, IntentReceipt, NativeBridge, bounded, failure, native_project_id};
use crazyagent::engine::owner::OwnerMutation;
use crazyagent::memory::{KnowledgeEntry, KnowledgeStatus, ReviewDecision};
use crazyagent::project_setup::{
    ContextChunk, ProjectCatalog, ProjectContext, ProjectSetup, ResearchJob, ResearchKind,
    StoredSetup,
};
use crazyagent::protocol::RequestId;
use crazyagent::protocol::{CredentialRef, ProjectId, ProjectSpec};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ResearchJobs {
    pub project_id: String,
    pub jobs: Vec<ResearchJob>,
}

#[derive(Debug, Serialize)]
pub struct MemoryItem {
    pub id: String,
    pub version: u64,
    pub text: String,
    pub status: KnowledgeStatus,
    pub human_reviewed: bool,
}

impl From<KnowledgeEntry> for MemoryItem {
    fn from(entry: KnowledgeEntry) -> Self {
        Self {
            id: entry.id.as_str().to_owned(),
            version: entry.version,
            text: entry.text,
            status: entry.status,
            human_reviewed: entry.human_reviewed,
        }
    }
}

fn read_failure(code: &str) -> BridgeError {
    failure(match code {
        "pool_missing" => "pool_missing",
        "research_duplicate" => "research_duplicate",
        "project_setup_missing" => "project_setup_missing",
        "context_path_invalid" => "context_path_invalid",
        "context_item_missing" => "context_item_missing",
        "context_offset_invalid" => "context_offset_invalid",
        "project_missing" => "project_missing",
        _ => "owner_read_failed",
    })
}

fn project(id: &str) -> Result<ProjectId, BridgeError> {
    ProjectId::new(id).map_err(|_| failure("invalid_project"))
}

impl NativeBridge {
    pub(super) fn ready_for_intent(&self, generation: u64) -> Result<(), BridgeError> {
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

    /// Copies more owner-picked documents into an existing project folder.
    /// Takes the project id, absolute source paths and the client generation.
    /// Returns the actual commit receipt; duplicates and oversized files are plain rejections.
    pub async fn add_project_documents(
        &mut self,
        id: &str,
        documents: Vec<String>,
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
                OwnerMutation::AddProjectDocuments {
                    project_id,
                    documents,
                },
                generation,
            )
            .map_err(|_| failure("owner_not_ready"))?;
        self.deliver_intent(false).await
    }

    /// Lists a project's memories, including research saved for approval.
    /// Takes the project id and the last memory id already shown, if any.
    /// Returns at most one engine batch with plain status names.
    pub async fn project_memories(
        &mut self,
        id: &str,
        after: Option<&str>,
    ) -> Result<Vec<MemoryItem>, BridgeError> {
        let project_id = project(id)?;
        let after = after
            .map(|after| RequestId::new(after).map_err(|_| failure("invalid_memory")))
            .transpose()?;
        let entries = self
            .controller()?
            .project_memories(project_id, after)
            .await
            .map_err(|e| read_failure(&e.0))?;
        Ok(entries.into_iter().map(MemoryItem::from).collect())
    }

    /// Approves, edits, rejects or removes one memory; Keep is the one-click approval.
    /// Takes the project id, memory id, the version shown, the decision and the client generation.
    /// Returns the actual commit receipt; a stale version is a plain rejection.
    pub async fn review_memory(
        &mut self,
        id: &str,
        memory: &str,
        version: u64,
        decision: ReviewDecision,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.ready_for_intent(generation)?;
        let project_id = project(id)?;
        let entry_id = RequestId::new(memory).map_err(|_| failure("invalid_memory"))?;
        self.controller()?
            .project_memories(project_id, None)
            .await
            .map_err(|e| read_failure(&e.0))?;
        self.controller()?
            .begin_mutation(
                OwnerMutation::Knowledge {
                    review: crazyagent::memory::HumanReview::Review {
                        entry_id,
                        version,
                        decision,
                    },
                },
                generation,
            )
            .map_err(|_| failure("owner_not_ready"))?;
        self.deliver_intent(false).await
    }

    /// Deletes one document or research report from a project folder; the brief stays.
    /// Takes the project id, the item path relative to the folder and the client generation.
    /// Returns the actual commit receipt; links, folders and absent items are plain rejections.
    pub async fn delete_project_context(
        &mut self,
        id: &str,
        path: String,
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
                OwnerMutation::DeleteProjectContext { project_id, path },
                generation,
            )
            .map_err(|_| failure("owner_not_ready"))?;
        self.deliver_intent(false).await
    }
}
