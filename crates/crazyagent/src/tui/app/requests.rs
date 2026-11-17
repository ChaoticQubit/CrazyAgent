use super::*;

impl App {
    /// Queues a New Project creation with its folder, documents, team plan and research toggles.
    /// Takes the name spec and the full setup collected by the client.
    /// Returns an error while another owner intent is unresolved.
    pub fn queue_project_setup(
        &mut self,
        spec: ProjectSpec,
        setup: crate::project_setup::ProjectSetup,
    ) -> Result<(), String> {
        spec.validate().map_err(|e| e.to_string())?;
        setup.validate().map_err(|e| e.0)?;
        self.submit_creation(spec, Some(setup))?;
        self.notice("Creating project, its folder and research jobs");
        Ok(())
    }

    /// Queues one New Project read for the owner worker.
    /// Takes the catalog, setup, jobs, context or context item query.
    /// Returns an error while another read is waiting.
    pub fn request_project_setup(&mut self, query: SetupQuery) -> Result<(), String> {
        if self.setup_query.is_some() {
            return Err("A project setup read is already waiting".into());
        }
        self.setup_query = Some(query);
        Ok(())
    }

    /// Queues one change to an existing project's setup: save research to memory, add documents or delete a context item.
    /// Takes the setup action; project creation stays on queue_project_setup.
    /// Returns an error in limited mode, for an invalid action or while another setup request waits.
    pub fn request_setup_change(
        &mut self,
        action: crate::project_setup::SetupAction,
    ) -> Result<(), String> {
        if !self.owner_ready {
            return Err("Project changes disabled in limited mode".into());
        }
        if matches!(action, crate::project_setup::SetupAction::Create { .. }) {
            return Err("Create projects with queue_project_setup".into());
        }
        action.validate().map_err(|e| e.0)?;
        self.request_project_setup(SetupQuery::Change {
            action,
            generation: self.declaration.generation,
        })
    }

    /// Queues one memory review; Keep is the one-click approval of saved research.
    /// Takes the project, memory entry id, the version shown and the decision.
    /// Returns an error in limited mode or while another setup request waits.
    pub fn request_memory_review(
        &mut self,
        project: ProjectId,
        entry_id: RequestId,
        version: u64,
        decision: crate::memory::ReviewDecision,
    ) -> Result<(), String> {
        if !self.owner_ready {
            return Err("Memory review disabled in limited mode".into());
        }
        self.request_project_setup(SetupQuery::ReviewMemory {
            project,
            entry_id,
            version,
            decision,
            generation: self.declaration.generation,
        })
    }

    /// Queues one team action: a message to the master, a task handed to a sub-agent,
    /// a cancel or the keep-warm switch. Team status reads use SetupQuery::Team.
    /// Takes the team action.
    /// Returns an error in limited mode, for an invalid action or while another setup request waits.
    pub fn request_team_change(&mut self, action: crate::team::TeamAction) -> Result<(), String> {
        if !self.owner_ready {
            return Err("Team changes are disabled in limited mode".into());
        }
        action.validate().map_err(|e| e.0)?;
        self.request_project_setup(SetupQuery::TeamChange {
            action,
            generation: self.declaration.generation,
        })
    }

    /// Queues one saved-memory read: a page of global or project memory, one memory,
    /// or the approved set a project's agents start with.
    /// Returns an error for a change, which goes through request_library_change.
    pub fn request_library(
        &mut self,
        query: crate::memory::library::LibraryQuery,
    ) -> Result<(), String> {
        if matches!(query, crate::memory::library::LibraryQuery::Change { .. }) {
            return Err("Use request_library_change to change memory".into());
        }
        self.request_project_setup(SetupQuery::Library(query))
    }

    /// Queues one owner change to saved memory: add a note, promote a project memory
    /// to global memory with one click, or delete a memory for good.
    /// Returns a plain error in limited mode, for an invalid note or while a request waits.
    pub fn request_library_change(
        &mut self,
        action: crate::memory::library::LibraryAction,
    ) -> Result<(), String> {
        if !self.owner_ready {
            return Err("Memory changes disabled in limited mode".into());
        }
        action.validate().map_err(|problem| {
            crate::memory::library::plain_problem(&problem.0).map_or(problem.0, str::to_owned)
        })?;
        self.request_project_setup(SetupQuery::Library(
            crate::memory::library::LibraryQuery::Change {
                action,
                generation: self.declaration.generation,
            },
        ))
    }
}
