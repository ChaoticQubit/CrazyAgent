use super::{
    BridgeError, BridgeQueue, IntentReceipt, NativeBridge, Unlocked, bounded, caused, failure,
    native_project_id,
};
use crazyagent::activity::ActivityItem;
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
pub struct ProjectActivity {
    pub project_id: String,
    pub items: Vec<ActivityItem>,
    pub more: bool,
}

#[derive(Debug, Serialize)]
pub struct ResearchLog {
    pub lines: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct MemoryItem {
    pub id: String,
    pub version: u64,
    pub text: String,
    pub status: KnowledgeStatus,
    pub human_reviewed: bool,
}

/// One decision the IDE asked the owner to confirm: the memory at the asked version and
/// whether to approve or reject it.
#[derive(Debug, Serialize)]
pub struct IdeAskItem {
    #[serde(flatten)]
    pub memory: MemoryItem,
    pub decision: crazyagent::ide::bridge::Decision,
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

pub(super) fn read_failure(code: &str) -> BridgeError {
    caused(
        match code {
            "pool_missing" => "pool_missing",
            "research_duplicate" => "research_duplicate",
            "project_setup_missing" => "project_setup_missing",
            "context_path_invalid" => "context_path_invalid",
            "context_item_missing" => "context_item_missing",
            "context_offset_invalid" => "context_offset_invalid",
            "project_missing" => "project_missing",
            "research_job_missing" => "research_job_missing",
            "project_files_query_invalid" => "project_files_query_invalid",
            "project_files_folder_missing" => "project_files_folder_missing",
            "project_files_no_repository" => "project_files_no_repository",
            _ => "owner_read_failed",
        },
        code,
    )
}

fn project(id: &str) -> Result<ProjectId, BridgeError> {
    ProjectId::new(id).map_err(|_| failure("invalid_project"))
}

impl NativeBridge {
    pub(super) fn ready_for_intent(&mut self, generation: u64) -> Result<(), BridgeError> {
        self.allow_change()?;
        if self.blocked() {
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
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        assert!(self.status().pending);
        self.deliver_intent(false).await
    }

    /// Reads the stored setup of one project.
    /// Takes the project id.
    /// Returns the setup, or none for a project created without one.
    pub async fn project_setup(&self, id: &str) -> Result<Option<StoredSetup>, BridgeError> {
        let setup = self
            .reader()?
            .project_setup(project(id)?)
            .await
            .map_err(|e| read_failure(&e.0))?;
        bounded(setup)
    }

    /// Lists a project's research jobs with plain status labels.
    /// Takes the project id.
    /// Returns the jobs in creation order.
    pub async fn research_jobs(&self, id: &str) -> Result<ResearchJobs, BridgeError> {
        let jobs = self
            .reader()?
            .research_jobs(project(id)?)
            .await
            .map_err(|e| read_failure(&e.0))?;
        bounded(ResearchJobs {
            project_id: id.into(),
            jobs,
        })
    }

    /// Lists what happened in a project, newest first, as the Overview shows it.
    /// Takes the project id and the most events to return.
    /// Returns the events and whether older ones were left out.
    pub async fn project_activity(
        &self,
        id: &str,
        limit: usize,
    ) -> Result<ProjectActivity, BridgeError> {
        let listed = self
            .reader()?
            .project_activity(project(id)?, limit)
            .await
            .map_err(|e| read_failure(&e.0))?;
        bounded(ProjectActivity {
            project_id: id.into(),
            items: listed.items,
            more: listed.more,
        })
    }

    /// Lists the brief, documents and research reports of a project folder.
    /// Takes the project id.
    /// Returns the folder path and its context items.
    pub async fn project_context(&self, id: &str) -> Result<ProjectContext, BridgeError> {
        let context = self
            .reader()?
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
            .map_err(|error| caused("owner_not_ready", &error.0))?;
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
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        self.deliver_intent(false).await
    }

    /// Lists a project's memories, including research saved for approval.
    /// Takes the project id and the last memory id already shown, if any.
    /// Returns at most one engine batch with plain status names.
    pub async fn project_memories(
        &self,
        id: &str,
        after: Option<&str>,
    ) -> Result<Vec<MemoryItem>, BridgeError> {
        let project_id = project(id)?;
        let after = after
            .map(|after| RequestId::new(after).map_err(|_| failure("invalid_memory")))
            .transpose()?;
        let entries = self
            .reader()?
            .project_memories(project_id, after)
            .await
            .map_err(|e| read_failure(&e.0))?;
        Ok(entries.into_iter().map(MemoryItem::from).collect())
    }

    /// Lists the decisions the project's IDE asked the owner to confirm on memories still
    /// waiting at the asked version. The desktop draws the confirm, which the IDE cannot
    /// click, and decides through `review_memory` with Keep or Reject. Takes the project id.
    /// Returns the asks.
    pub async fn ide_asks(&self, id: &str) -> Result<Vec<IdeAskItem>, BridgeError> {
        let project_id = project(id)?;
        let asks = self
            .reader()?
            .ide_asks(project_id)
            .await
            .map_err(|e| read_failure(&e.0))?;
        Ok(asks
            .into_iter()
            .map(|ask| IdeAskItem {
                memory: MemoryItem::from(ask.entry),
                decision: ask.decision,
            })
            .collect())
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
            .map_err(|error| caused("owner_not_ready", &error.0))?;
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
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        self.deliver_intent(false).await
    }

    /// Reads the extra read-only folders a project's agents get beside its toolchains.
    /// Takes the project id.
    /// Returns the folders in saved order and the version a change sends back.
    pub async fn project_read_paths(
        &self,
        id: &str,
    ) -> Result<crazyagent::project_setup::ReadPaths, BridgeError> {
        let paths = self
            .reader()?
            .project_read_paths(project(id)?)
            .await
            .map_err(|e| read_failure(&e.0))?;
        bounded(paths)
    }

    /// Reads the folders a project's agents are kept out of (DEC-111 Q7).
    /// Takes the project id.
    /// Returns the folders in saved order.
    pub async fn project_hidden_paths(&self, id: &str) -> Result<Vec<String>, BridgeError> {
        let paths = self
            .reader()?
            .project_hidden_paths(project(id)?)
            .await
            .map_err(|e| read_failure(&e.0))?;
        bounded(paths)
    }

    /// Reads a project's agent access level and the toolchains its agents may use, with
    /// which levels this platform offers and which toolchains were found.
    /// Takes the project id. Returns the engine's view of the saved policy.
    pub async fn project_agent_policy(
        &mut self,
        id: &str,
    ) -> Result<crazyagent::sandbox::policy::PolicyView, BridgeError> {
        let policy = self
            .controller()?
            .project_agent_policy(project(id)?)
            .await
            .map_err(|e| read_failure(&e.0))?;
        bounded(policy)
    }

    /// Lists a set-up project's files for the console's `@` completion: tracked and
    /// untracked, not ignored paths relative to its folder that contain the query. The
    /// listing runs on a copy of the controller without the bridge and leaves the bridge
    /// controller's view as it was. Takes the project id and the query. Returns the
    /// listing, which gives at most 200 paths in git's order.
    pub fn project_files(
        &self,
        id: &str,
        query: &str,
    ) -> Result<Unlocked<Vec<String>>, BridgeError> {
        let mut controller = self
            .controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .reader()
            .map_err(|_| failure("not_connected"))?;
        let project_id = project(id)?;
        let query = query.to_owned();
        Ok(Box::pin(async move {
            let paths = controller
                .project_files(project_id, query, crazyagent::project_files::FILES_MAX)
                .await
                .map_err(|e| read_failure(&e.0))?;
            bounded(paths)
        }))
    }

    /// Replaces a project's extra read-only folders; the engine refuses a relative or missing
    /// path, the home folder, a credential location and the CrazyAgent data folder. Reads the
    /// folders first, so the change starts from the engine's current revision.
    /// Takes the project id, the version the screen showed, the folders and the client
    /// generation. Returns the actual commit receipt; folders changed since the screen read
    /// them are the plain rejection `stale_revision`.
    pub async fn set_project_read_paths(
        &mut self,
        id: &str,
        version: u64,
        paths: Vec<String>,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.ready_for_intent(generation)?;
        let project_id = project(id)?;
        self.controller()?
            .project_read_paths(project_id.clone())
            .await
            .map_err(|e| read_failure(&e.0))?;
        self.controller()?
            .begin_mutation(
                OwnerMutation::SetProjectReadPaths {
                    project_id,
                    version,
                    paths,
                },
                generation,
            )
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        self.deliver_intent(false).await
    }

    /// Replaces the folders a project's agents are kept out of (DEC-111 Q7); the engine refuses
    /// a path outside the project. Reads the folders first, so the change starts from the
    /// engine's current revision. Takes the project id, the folders and the client generation.
    /// Returns the actual commit receipt.
    pub async fn set_project_hidden_paths(
        &mut self,
        id: &str,
        paths: Vec<String>,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.ready_for_intent(generation)?;
        let project_id = project(id)?;
        self.controller()?
            .project_hidden_paths(project_id.clone())
            .await
            .map_err(|e| read_failure(&e.0))?;
        self.controller()?
            .begin_mutation(
                OwnerMutation::SetProjectHiddenPaths { project_id, paths },
                generation,
            )
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        self.deliver_intent(false).await
    }

    /// Replaces a project's agent access level and toolchain choices; the engine refuses a
    /// level this platform cannot enforce and a malformed policy.
    /// Takes the project id, the version the screen showed, the policy and the client
    /// generation. Returns the actual commit receipt; a policy changed since the screen read
    /// it is the plain rejection `stale_revision`.
    pub async fn set_project_agent_policy(
        &mut self,
        id: &str,
        version: u64,
        policy: crazyagent::sandbox::policy::AgentPolicy,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.ready_for_intent(generation)?;
        let project_id = project(id)?;
        self.controller()?
            .begin_mutation(
                OwnerMutation::SetProjectAgentPolicy {
                    project_id,
                    version,
                    policy,
                },
                generation,
            )
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        self.deliver_intent(false).await
    }

    /// Changes a project's name, description, stack, platforms, research, pool and team.
    /// Takes the project id, the new setup, an optional new name, the setup revision the screen
    /// showed (the current one when absent) and the client generation. Returns the actual
    /// commit receipt; a change made in between is the plain rejection `stale_revision`.
    pub async fn update_project_setup(
        &mut self,
        id: &str,
        setup: ProjectSetup,
        name: Option<String>,
        revision: Option<u64>,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.ready_for_intent(generation)?;
        let project_id = project(id)?;
        let renamed = name.as_ref().map(|name| ProjectSpec {
            name: name.clone(),
            repositories: Vec::new(),
        });
        if renamed.is_some_and(|spec| spec.validate().is_err()) {
            return Err(failure("project_spec_bounds"));
        }
        if !setup.documents.is_empty() {
            return Err(failure("project_setup_bounds"));
        }
        setup
            .validate()
            .map_err(|_| failure("project_setup_bounds"))?;
        let stored = self
            .controller()?
            .project_setup(project_id.clone())
            .await
            .map_err(|e| read_failure(&e.0))?
            .ok_or_else(|| failure("project_setup_missing"))?;
        self.controller()?
            .begin_mutation(
                OwnerMutation::UpdateProjectSetup {
                    project_id,
                    expected_revision: revision.unwrap_or(stored.revision),
                    name,
                    setup,
                },
                generation,
            )
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        self.deliver_intent(false).await
    }

    /// Removes a project from CrazyAgent. Without a name its folder stays on disk; with the
    /// name the owner typed, the engine stops its agents and moves its folders to the Trash,
    /// only when the name is the project's exact name. Takes the project id, the client
    /// generation and the typed name, if any. Returns the actual commit receipt.
    pub async fn remove_project(
        &mut self,
        id: &str,
        generation: u64,
        delete_name: Option<String>,
    ) -> Result<IntentReceipt, BridgeError> {
        self.ready_for_intent(generation)?;
        let project_id = project(id)?;
        self.controller()?
            .project_setup(project_id.clone())
            .await
            .map_err(|e| read_failure(&e.0))?;
        let mutation = match delete_name {
            Some(name) => OwnerMutation::DeleteProject { project_id, name },
            None => OwnerMutation::RemoveProject { project_id },
        };
        self.controller()?
            .begin_mutation(mutation, generation)
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        self.deliver_intent(false).await
    }

    /// Moves or copies a project kept where it is into the projects home: the engine waits
    /// for its team to stop, moves the folder and points the project and its agents' copies
    /// at the new place. Takes the project id, move or copy, the soft refusals the owner
    /// chose to go past and the client generation. Returns the actual commit receipt.
    pub async fn move_project_home(
        &mut self,
        id: &str,
        mode: crazyagent::projects_home::AddMode,
        overrides: Vec<crazyagent::projects_home::AddOverride>,
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
                OwnerMutation::MoveProjectHome {
                    project_id,
                    mode,
                    overrides,
                },
                generation,
            )
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        self.deliver_intent(false).await
    }

    /// Stops one research job; the runner stops a running agent's whole process group.
    /// Takes the project id, the job id and the client generation.
    /// Returns the actual commit receipt; a finished job is a plain rejection.
    pub async fn cancel_research(
        &mut self,
        id: &str,
        job: i64,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.research_change(
            OwnerMutation::CancelResearch {
                project_id: project_id.clone(),
                job,
            },
            project_id,
            generation,
        )
        .await
    }

    /// Queues a failed or cancelled research job again.
    /// Takes the project id, the job id and the client generation.
    /// Returns the actual commit receipt; a job that is not stopped is a plain rejection.
    pub async fn retry_research(
        &mut self,
        id: &str,
        job: i64,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.research_change(
            OwnerMutation::RetryResearch {
                project_id: project_id.clone(),
                job,
            },
            project_id,
            generation,
        )
        .await
    }

    /// Queues the project's roadmap and release process planning job now.
    /// Takes the project id and the client generation.
    /// Returns the actual commit receipt; `planning_busy` while a planning job of the
    /// project is queued or running.
    pub async fn plan_roadmap(
        &mut self,
        id: &str,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = project(id)?;
        self.research_change(
            OwnerMutation::PlanRoadmap {
                project_id: project_id.clone(),
            },
            project_id,
            generation,
        )
        .await
    }

    async fn research_change(
        &mut self,
        mutation: OwnerMutation,
        project_id: ProjectId,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.ready_for_intent(generation)?;
        self.controller()?
            .research_jobs(project_id)
            .await
            .map_err(|e| read_failure(&e.0))?;
        self.controller()?
            .begin_mutation(mutation, generation)
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        self.deliver_intent(false).await
    }

    /// Reads the last plain lines of one research job's agent output, with secrets hidden.
    /// Takes the project id and the job id.
    /// Returns the bounded lines, oldest first; none before the job first runs.
    pub async fn research_log(&mut self, id: &str, job: i64) -> Result<ResearchLog, BridgeError> {
        let lines = self
            .controller()?
            .research_log(project(id)?, job)
            .await
            .map_err(|e| read_failure(&e.0))?;
        bounded(ResearchLog { lines })
    }
}

impl BridgeQueue {
    /// Lists a set-up project's files for the console's `@` completion through the window's
    /// bridge queue. Takes a listing place, then the shared bridge only to start the listing,
    /// so a change asked while git runs does not wait for it. Takes the project id and the
    /// query. Returns at most 200 paths in git's order, or `busy` when no place or bridge
    /// comes free within the queue's wait.
    pub async fn project_files(&self, id: &str, query: &str) -> Result<Vec<String>, BridgeError> {
        let _place = self.listing().await?;
        let listing = self.read().await?.project_files(id, query)?;
        listing.await
    }
}
