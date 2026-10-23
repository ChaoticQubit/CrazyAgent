use super::*;
use crate::memory::library::{
    ApprovedMemories, LibraryAction, MemoryPage, MemoryScope, PAGE_ITEMS_MAX, SavedMemory,
    validate_note,
};
use crate::protocol::ProjectId;

impl OwnerController {
    /// Lists saved memories in one scope, newest first, with optional search.
    /// Takes the scope, search text (blank means no search) and the `next` cursor
    /// of the previous page. Returns one bounded page of memories.
    pub async fn memories(
        &mut self,
        scope: MemoryScope,
        search: Option<String>,
        before: Option<u64>,
    ) -> Result<MemoryPage, EngineError> {
        let search = search
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty());
        let query = OwnerRead::Memories {
            scope,
            search,
            before,
            limit: PAGE_ITEMS_MAX,
        };
        match self.read(query).await?.data {
            OwnerData::Memories { page } => Ok(page),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Reads one saved memory for its detail view.
    /// Takes the memory id. Returns the memory, or None once it was deleted.
    pub async fn memory(
        &mut self,
        memory_id: RequestId,
    ) -> Result<Option<SavedMemory>, EngineError> {
        match self.read(OwnerRead::Memory { memory_id }).await?.data {
            OwnerData::Memory { memory } => Ok(memory),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Reads the approved memories every agent in a project starts with.
    /// Takes the project id. Returns the same bounded set the engine gives agents.
    pub async fn approved_memories(
        &mut self,
        project_id: ProjectId,
    ) -> Result<ApprovedMemories, EngineError> {
        match self
            .read(OwnerRead::ApprovedMemories { project_id })
            .await?
            .data
        {
            OwnerData::ApprovedMemories { memories } => Ok(memories),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Saves a short memory the owner typed, globally or for one project.
    /// Takes the scope, the note text and a nonzero client generation.
    /// Returns the owner response; a refused note is a plain rejection.
    pub async fn add_memory(
        &mut self,
        scope: MemoryScope,
        text: String,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        validate_note(&text)?;
        self.change_memory(LibraryAction::Add { scope, text }, generation)
            .await
    }

    /// Copies one project memory into global memory with one owner click,
    /// keeping it in the project first when it was still waiting for review.
    /// Takes the project, entry id, the version shown and the client generation.
    pub async fn promote_memory(
        &mut self,
        project_id: ProjectId,
        entry_id: RequestId,
        version: u64,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        let action = LibraryAction::Promote {
            project_id,
            entry_id,
            version,
        };
        self.change_memory(action, generation).await
    }

    /// Deletes one saved memory for good, global or project.
    /// Takes the memory id and the client generation.
    /// Returns the owner response; an absent memory is a plain rejection.
    pub async fn delete_memory(
        &mut self,
        memory_id: RequestId,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        self.change_memory(LibraryAction::Delete { memory_id }, generation)
            .await
    }

    /// Deletes one project memory for good, with the saved source text only it used.
    /// Takes the project and entry id and the client generation. A copy already
    /// promoted to global memory stays until it is deleted there.
    pub async fn forget_memory(
        &mut self,
        project_id: ProjectId,
        entry_id: RequestId,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        let action = LibraryAction::Forget {
            project_id,
            entry_id,
        };
        self.change_memory(action, generation).await
    }

    /// Keeps a research report, or one of its sections, as an approved project
    /// memory with one owner click, saving it first when it was not saved yet.
    /// Takes the project, research kind, optional heading and the client generation.
    pub async fn remember_research(
        &mut self,
        project_id: ProjectId,
        research: crate::project_setup::ResearchKind,
        section: Option<String>,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        let action = LibraryAction::Remember {
            project_id,
            research,
            section,
        };
        self.change_memory(action, generation).await
    }

    /// Saves text the owner selected in a context file as an approved project memory
    /// with the file path as its source. Takes the project, the path relative to the
    /// project folder, the selected text and the client generation. The engine refuses
    /// the text when the file no longer holds it.
    pub async fn save_selection_to_memory(
        &mut self,
        project_id: ProjectId,
        path: String,
        text: String,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        let action = LibraryAction::SaveSelection {
            project_id,
            path,
            text,
        };
        self.change_memory(action, generation).await
    }

    /// Sends one validated saved-memory change as a retained owner mutation.
    /// Takes the action and the client generation. Returns the owner response.
    pub async fn change_memory(
        &mut self,
        action: LibraryAction,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        action.validate()?;
        if self.pending.is_some() || self.pending_account_profile.is_some() {
            return Err(EngineError("controller_pending_intent".into()));
        }
        self.read(OwnerRead::OwnerProfile).await?;
        self.begin_mutation(OwnerMutation::Memory { action }, generation)?;
        self.send_pending().await
    }
}
