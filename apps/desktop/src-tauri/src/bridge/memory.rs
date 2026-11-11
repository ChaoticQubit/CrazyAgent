use super::{ActionOutcome, BridgeError, IntentReceipt, NativeBridge, bounded, caused, failure};
use crazyagent::engine::owner::OwnerMutation;
use crazyagent::memory::library::{
    AgentWrites, ApprovedMemories, LibraryAction, MemoryOrigin, MemoryScope, SavedMemory,
    known_code,
};
use crazyagent::protocol::{ProjectId, RequestId};
use serde::Serialize;

/// One saved memory as the window shows it: plain scope and origin words, no cursors.
#[derive(Debug, Serialize)]
pub struct MemoryNote {
    pub id: String,
    pub text: String,
    pub scope: &'static str,
    pub project_id: Option<String>,
    pub origin: &'static str,
    pub from_project_id: Option<String>,
    pub source_path: Option<String>,
    pub agent: Option<String>,
}

impl From<SavedMemory> for MemoryNote {
    fn from(memory: SavedMemory) -> Self {
        let (scope, project_id) = match memory.scope {
            MemoryScope::Global => ("global", None),
            MemoryScope::Project { project_id } => {
                ("project", Some(project_id.as_str().to_owned()))
            }
        };
        let (origin, from_project_id, source_path, agent) = match memory.origin {
            MemoryOrigin::Written => ("written", None, None, None),
            MemoryOrigin::Promoted { project_id, .. } => {
                ("promoted", Some(project_id.as_str().to_owned()), None, None)
            }
            MemoryOrigin::Selected { path } => ("selected", None, Some(path), None),
            MemoryOrigin::Agent { agent } => ("agent", None, None, Some(agent)),
            MemoryOrigin::Requested { agent } => ("requested", None, None, Some(agent)),
            MemoryOrigin::Imported { file } => ("imported", None, Some(file), None),
        };
        Self {
            id: memory.id.as_str().to_owned(),
            text: memory.text,
            scope,
            project_id,
            origin,
            from_project_id,
            source_path,
            agent,
        }
    }
}

/// One page of saved memories, newest first; pass `next` back as `before`.
#[derive(Debug, Serialize)]
pub struct MemoryList {
    pub items: Vec<MemoryNote>,
    pub next: Option<u64>,
    pub agent_writes: AgentWrites,
}

/// How an import of a memory folder ended: memories saved, markdown files not saved, and
/// the receipt of the change that stopped it while its outcome is still unconfirmed.
#[derive(Debug, Serialize)]
pub struct MemoryImport {
    pub imported: u32,
    pub skipped: u32,
    pub stopped: Option<IntentReceipt>,
}

fn code(code: &str, fallback: &'static str) -> BridgeError {
    caused(
        known_code(code).unwrap_or(match code {
            "project_missing" => "project_missing",
            "research_section_invalid" => "research_section_invalid",
            "context_path_invalid" => "context_path_invalid",
            _ => fallback,
        }),
        code,
    )
}

fn scope(project: Option<&str>) -> Result<MemoryScope, BridgeError> {
    Ok(match project {
        None => MemoryScope::Global,
        Some(id) => MemoryScope::Project {
            project_id: ProjectId::new(id).map_err(|_| failure("invalid_project"))?,
        },
    })
}

fn memory_id(id: &str) -> Result<RequestId, BridgeError> {
    RequestId::new(id).map_err(|_| failure("invalid_memory"))
}

impl NativeBridge {
    /// Lists saved memories, global or for one project, newest first, with search.
    /// Takes the project id (none for global memory), search text and the `next`
    /// value of the previous page. Returns one bounded page.
    pub async fn memories(
        &mut self,
        project: Option<&str>,
        search: Option<&str>,
        before: Option<u64>,
    ) -> Result<MemoryList, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        let scope = scope(project)?;
        let page = self
            .controller()?
            .memories(scope, search.map(str::to_owned), before)
            .await
            .map_err(|error| code(&error.0, "owner_read_failed"))?;
        bounded(MemoryList {
            items: page.items.into_iter().map(MemoryNote::from).collect(),
            next: page.next,
            agent_writes: page.agent_writes,
        })
    }

    /// Reads one saved memory for its detail view.
    /// Takes the memory id. Returns the memory, or None once it was deleted.
    pub async fn memory(&mut self, id: &str) -> Result<Option<MemoryNote>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        let id = memory_id(id)?;
        let memory = self
            .controller()?
            .memory(id)
            .await
            .map_err(|error| code(&error.0, "owner_read_failed"))?;
        bounded(memory.map(MemoryNote::from))
    }

    /// Reads the approved memories every agent in a project starts with.
    /// Takes the project id. Returns the bounded set agents receive.
    pub async fn approved_memories(
        &mut self,
        project: &str,
    ) -> Result<ApprovedMemories, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        let project_id = ProjectId::new(project).map_err(|_| failure("invalid_project"))?;
        let memories = self
            .controller()?
            .approved_memories(project_id)
            .await
            .map_err(|error| code(&error.0, "owner_read_failed"))?;
        bounded(memories)
    }

    /// Saves a short memory the owner typed, globally or for one project.
    /// Takes the project id (none for global), the text and a fresh draft generation.
    /// Returns the intent receipt; a refused note is a plain rejection code.
    pub async fn add_memory(
        &mut self,
        project: Option<&str>,
        text: String,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let scope = scope(project)?;
        self.change_memory(LibraryAction::Add { scope, text }, generation)
            .await
    }

    /// Promotes one project memory to global memory with one owner click.
    /// Takes the project id, the project memory id, the version shown and the generation.
    /// Returns the intent receipt; a changed memory is a plain rejection.
    pub async fn promote_memory(
        &mut self,
        project: &str,
        memory: &str,
        version: u64,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let action = LibraryAction::Promote {
            project_id: ProjectId::new(project).map_err(|_| failure("invalid_project"))?,
            entry_id: memory_id(memory)?,
            version,
        };
        self.change_memory(action, generation).await
    }

    /// Deletes one saved memory for good.
    /// Takes the memory id and a fresh draft generation. Returns the intent receipt.
    pub async fn delete_memory(
        &mut self,
        id: &str,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let action = LibraryAction::Delete {
            memory_id: memory_id(id)?,
        };
        self.change_memory(action, generation).await
    }

    /// Approves one memory an agent asked to save, so agents get it from now on.
    /// Takes the memory id and a fresh draft generation. Returns the intent receipt.
    pub async fn approve_memory(
        &mut self,
        id: &str,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let action = LibraryAction::Approve {
            memory_id: memory_id(id)?,
        };
        self.change_memory(action, generation).await
    }

    /// Saves the owner's Agent memory writes setting: save directly or ask first.
    /// Takes the choice and a fresh draft generation. Returns the intent receipt.
    pub async fn set_agent_memory_writes(
        &mut self,
        writes: AgentWrites,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.change_memory(LibraryAction::AgentWrites { writes }, generation)
            .await
    }

    /// Deletes one project memory for good, with the saved source text only it used.
    /// Takes the project id, the project memory id, the version shown and a fresh draft
    /// generation. Returns the intent receipt; a memory changed since that version is
    /// refused with `memory_changed`, and a promoted global copy stays.
    pub async fn forget_memory(
        &mut self,
        project: &str,
        memory: &str,
        version: u64,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let action = LibraryAction::Forget {
            project_id: ProjectId::new(project).map_err(|_| failure("invalid_project"))?,
            entry_id: memory_id(memory)?,
            version,
        };
        self.change_memory(action, generation).await
    }

    /// Keeps a research report, or one of its sections, as an approved project memory
    /// with one owner click. Takes the project id, research kind, optional heading and
    /// a fresh draft generation. Returns the intent receipt.
    pub async fn remember_research(
        &mut self,
        project: &str,
        research: crazyagent::project_setup::ResearchKind,
        section: Option<String>,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let action = LibraryAction::Remember {
            project_id: ProjectId::new(project).map_err(|_| failure("invalid_project"))?,
            research,
            section,
        };
        self.change_memory(action, generation).await
    }

    /// Saves text the owner selected in a context file as an approved project memory,
    /// keeping the file path as its source. Takes the project id, the file path, the
    /// selected text and a fresh draft generation. Returns the intent receipt; a file
    /// that no longer holds the text is a plain rejection.
    pub async fn save_selection_to_memory(
        &mut self,
        id: &str,
        path: String,
        text: String,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let action = LibraryAction::SaveSelection {
            project_id: ProjectId::new(id).map_err(|_| failure("invalid_project"))?,
            path,
            text,
        };
        self.change_memory(action, generation).await
    }

    /// Imports the markdown memory files of a folder the owner chose into one project's
    /// memory, one memory per file, each as its own owner change. Takes the project id, the
    /// absolute folder and a fresh draft generation. Returns the counts; a duplicate or refused
    /// file is skipped, full memory skips the rest, and an unconfirmed change stops the import.
    pub async fn import_memory(
        &mut self,
        project: &str,
        folder: &str,
        generation: u64,
    ) -> Result<MemoryImport, BridgeError> {
        self.allow_change()?;
        let project_id = ProjectId::new(project).map_err(|_| failure("invalid_project"))?;
        let scan = crazyagent::memory::import::read_folder(std::path::Path::new(folder))
            .map_err(|error| code(&error.0, "memory_import_folder_invalid"))?;
        let count = scan.files.len();
        let total = count as u32 + scan.skipped;
        let mut report = MemoryImport {
            imported: 0,
            skipped: scan.skipped,
            stopped: None,
        };
        for (index, item) in scan.files.into_iter().enumerate() {
            let action = LibraryAction::Import {
                project_id: project_id.clone(),
                file: item.file,
                text: item.text,
            };
            let receipt = self.change_memory(action, generation).await?;
            match (receipt.outcome, receipt.pending) {
                (ActionOutcome::Applied, false) => report.imported += 1,
                (ActionOutcome::Rejected, false)
                    if receipt.rejection.as_deref() == Some("memory_full") =>
                {
                    report.skipped += (count - index) as u32;
                    break;
                }
                (ActionOutcome::Rejected, false) => report.skipped += 1,
                _ => {
                    report.stopped = Some(receipt);
                    break;
                }
            }
        }
        assert!(report.imported + report.skipped <= total);
        assert!(report.stopped.is_some() || report.imported + report.skipped == total);
        bounded(report)
    }

    async fn change_memory(
        &mut self,
        action: LibraryAction,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.allow_change()?;
        if self.blocked() {
            return Err(failure("pending_intent"));
        }
        if generation == 0 || generation > 9_007_199_254_740_991 {
            return Err(failure("draft_generation_limit"));
        }
        action
            .validate()
            .map_err(|error| code(&error.0, "owner_rejected"))?;
        self.controller()?
            .read(crazyagent::engine::owner::OwnerRead::OwnerProfile)
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))?;
        self.controller()?
            .begin_mutation(OwnerMutation::Memory { action }, generation)
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        assert!(self.status().pending);
        self.deliver_intent(false).await
    }
}
