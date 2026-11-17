use super::*;

impl App {
    pub fn new() -> Self {
        Self {
            declaration: Declaration::default(),
            navigation_generation: 1,
            declaration_pending: None,
            creation_job: None,
            snapshot_revision: None,
            model_access: model_access::State::default(),
            dispatch: dispatch::State::default(),
            master: master_session::State::default(),
            focus: Focus::Conversation,
            draft: String::new(),
            project_draft: String::new(),
            viewport: (0, 0),
            revision: Cursor::new(0),
            projects: Vec::new(),
            tasks: Vec::new(),
            config: None,
            plan: None,
            connected: false,
            status: "Connecting to engine".into(),
            scroll: 0,
            selected: None,
            task: 0,
            creating_project: false,
            command: None,
            draft_generation: 0,
            submitted_generation: None,
            pending_creation: None,
            owner_ready: false,
            accounts_wanted: false,
            owner_problem: None,
            owner_notice: "Connecting to the trusted engine endpoint".into(),
            evidence: None,
            owner_generation: 0,
            owner_step: None,
            owner_identity: None,
            drafts: Vec::new(),
            unassigned: false,
            conversation_generation: 0,
            conversation_stale: true,
            conversation_notice: "Waiting for protected project conversation".into(),
            conversation: None,
            conversation_stage: None,
            conversation_page: None,
            conversation_cursor: None,
            conversation_pending: None,
            conversation_job: None,
            edit_requested: None,
            brief_completion: None,
            setup_query: None,
            setup_view: None,
            setup_problem: None,
        }
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    pub fn draft(&self) -> &str {
        if self.focus == Focus::Settings
            && let Some(home) = self.model_access.home_draft()
        {
            return home;
        }
        if self.declaration.open {
            if self.declaration.field == 0 {
                &self.declaration.name
            } else {
                &self.declaration.repositories[self.declaration.field - 1]
            }
        } else if let Some(draft) = self.master.draft() {
            draft
        } else if self.creating_project {
            &self.project_draft
        } else if let Some(index) = self.draft_index() {
            let draft = &self.drafts[index];
            if draft.editing_brief {
                &draft.brief
            } else {
                &draft.message
            }
        } else if !self.unassigned
            && self.owner_identity.is_some()
            && self.selected_project().is_some()
        {
            ""
        } else {
            &self.draft
        }
    }

    fn active_draft(&mut self) -> &mut String {
        if self.creating_project {
            &mut self.project_draft
        } else if let Some(index) = self.draft_index() {
            let draft = &mut self.drafts[index];
            if draft.editing_brief {
                &mut draft.brief
            } else {
                &mut draft.message
            }
        } else {
            &mut self.draft
        }
    }

    pub fn viewport(&self) -> (u16, u16) {
        self.viewport
    }

    pub fn revision(&self) -> Cursor {
        self.revision.clone()
    }

    pub fn reduce(&mut self, event: UiEvent) -> Result<(), String> {
        self.sync_master()?;
        if self.declaration.open && self.reduce_declaration(&event)? {
            self.assert_consistent();
            return Ok(());
        }
        if !self.is_creating_project() && self.focus == Focus::Conversation {
            if matches!(event, UiEvent::ConfigureMaster) {
                self.master.open()?;
                self.scroll = 0;
                return Ok(());
            }
            if matches!(event, UiEvent::Character('h')) && self.master.is_open() {
                self.master
                    .configure_home(self.model_access.selected_homes());
                self.assert_consistent();
                return Ok(());
            }
            if matches!(event, UiEvent::Submit)
                && self.master.is_open()
                && (self.conversation_pending.is_some()
                    || self.declaration_pending.is_some()
                    || self.model_access.has_pending()
                    || self.pending_creation.is_some()
                    || self.command.is_some())
            {
                return Err(
                    "An owner or project intent is unresolved; master setup retained".into(),
                );
            }
            if self.master.reduce(&event)? {
                self.assert_consistent();
                return Ok(());
            }
        }
        if matches!(
            event,
            UiEvent::Submit | UiEvent::CancelMaster | UiEvent::CloseMaster
        ) && self.master.has_pending()
        {
            return Err("Master owner intent unresolved; fields and draft retained; Ctrl-R recovers its exact identity".into());
        }
        if matches!(event, UiEvent::Submit) && self.declaration_pending.is_some() {
            return Err("A creation owner intent is unresolved; input retained".into());
        }
        if matches!(event, UiEvent::Submit)
            && self.creating_project
            && (self.conversation_pending.is_some() || self.conversation_job.is_some())
        {
            return Err(
                "Conversation owner intent unresolved; ordinary project draft retained".into(),
            );
        }
        if matches!(
            event,
            UiEvent::FocusNext
                | UiEvent::FocusPrevious
                | UiEvent::SelectionNext
                | UiEvent::SelectionPrevious
                | UiEvent::BeginProject
        ) {
            self.advance_navigation()?;
        }
        if self.focus == Focus::Settings {
            let project = self
                .selected_project()
                .map(|project| (project.project_id.clone(), project.spec.name.clone()));
            self.model_access.select_assignment_project(project);
            if matches!(event, UiEvent::Submit) && self.conversation_pending.is_some() {
                return Err(
                    "A conversation owner intent is unresolved; metadata values retained".into(),
                );
            }
            if self.model_access.reduce(&event)? {
                let notice = self.model_access.notice.clone();
                self.notice(&notice);
                self.scroll = 0;
                self.assert_consistent();
                return Ok(());
            }
        }
        if matches!(event, UiEvent::Submit) && self.model_access.has_pending() {
            return Err("Metadata owner intent is unresolved; conversation draft retained".into());
        }
        let previous_project = self
            .selected_project()
            .map(|project| project.project_id.clone());
        match event {
            UiEvent::Resize(width, height) => self.viewport = (width.min(1000), height.min(1000)),
            UiEvent::FocusNext | UiEvent::FocusPrevious => {
                let index = FOCI.iter().position(|focus| *focus == self.focus).unwrap();
                let delta = if matches!(event, UiEvent::FocusNext) {
                    1
                } else {
                    FOCI.len() - 1
                };
                self.focus = FOCI[(index + delta) % FOCI.len()];
                self.scroll = 0;
            }
            UiEvent::Character(character) => {
                self.ensure_draft()?;
                if !character.is_control()
                    && self.draft().len() + character.len_utf8() <= DRAFT_BYTES_MAX
                {
                    self.active_draft().push(character);
                    self.advance_input_generation()?;
                }
            }
            UiEvent::Backspace => {
                self.ensure_draft()?;
                if self.active_draft().pop().is_some() {
                    self.advance_input_generation()?;
                }
            }
            UiEvent::SelectionNext | UiEvent::SelectionPrevious => {
                if !self.projects.is_empty() {
                    let delta = if matches!(event, UiEvent::SelectionNext) {
                        1
                    } else {
                        self.projects.len() - 1
                    };
                    self.selected =
                        Some((self.selected.unwrap_or(0) + delta) % self.projects.len());
                    self.task = 0;
                    self.config = None;
                    self.plan = None;
                    self.scroll = 0;
                }
            }
            UiEvent::TaskNext => {
                let count = self.project_tasks().count();
                self.task = if count == 0 {
                    0
                } else {
                    (self.task + 1) % count
                };
                self.config = None;
                self.plan = None;
                self.scroll = 0;
            }
            UiEvent::BeginProject => {
                if self.focus == Focus::Projects {
                    self.advance_generation()?;
                    self.creating_project = true;
                    self.project_draft.clear();
                    self.notice("Project name: type a name and press Enter; Esc cancels");
                }
            }
            UiEvent::BeginDeclaration => {
                self.advance_navigation()?;
                self.declaration.open = true;
                self.creating_project = false;
                self.scroll = 0;
                self.notice("Declare repositories | Tab fields | Ctrl-N add | Ctrl-D remove | Enter create | Esc retain");
            }
            UiEvent::DeclarationAdd | UiEvent::DeclarationRemove => {}
            UiEvent::ConfigureMaster => {
                return Err("Select the project Conversation for F2 Configure master".into());
            }
            UiEvent::CancelMaster | UiEvent::CloseMaster => {
                if self.focus != Focus::Conversation
                    || self.is_creating_project()
                    || self.unassigned
                {
                    return Err(
                        "Select the project Conversation to control its actual master".into(),
                    );
                }
                if self.conversation_pending.is_some()
                    || self.declaration_pending.is_some()
                    || self.model_access.has_pending()
                {
                    return Err("An owner intent is unresolved; master control retained".into());
                }
                self.master.control(matches!(event, UiEvent::CloseMaster))?;
            }
            UiEvent::Cancel => {
                if self.is_editing_brief() {
                    let index = self
                        .draft_index()
                        .expect("brief mode requires its project draft");
                    self.drafts[index].editing_brief = false;
                    self.notice("Brief edit closed; both drafts retained");
                } else if self.draft_index().is_some_and(|index| {
                    self.brief_completion
                        .as_ref()
                        .is_some_and(|(database, project)| {
                            database == &self.drafts[index].database
                                && project == &self.drafts[index].project
                        })
                }) {
                    self.brief_completion = None;
                    self.notice("Acknowledged brief edit closed; conversation draft retained");
                } else {
                    self.ensure_draft()?;
                    self.advance_input_generation()?;
                    self.active_draft().clear();
                    self.creating_project = false;
                    self.command = None;
                    self.submitted_generation = None;
                    self.edit_requested = None;
                    self.notice("Draft cleared");
                }
            }
            UiEvent::Submit => {
                if self.creating_project {
                    let name = self.project_draft.trim();
                    if name.is_empty() || name.len() > 256 {
                        return Err("Project name must contain 1 to 256 UTF-8 bytes".into());
                    }
                    if self.command.is_some() || self.pending_creation.is_some() {
                        return Err("A project command is already queued".into());
                    }
                    self.command = Some(Command::CreateProject {
                        project_id: ProjectId::new(format!(
                            "project-{}-{}",
                            std::process::id(),
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_err(|e| e.to_string())?
                                .as_nanos()
                        ))
                        .map_err(|e| e.to_string())?,
                        spec: ProjectSpec {
                            name: name.to_owned(),
                            repositories: Vec::new(),
                        },
                    });
                    self.submitted_generation = Some(self.draft_generation);
                } else if self.focus == Focus::Projects {
                    if self.selected_project().is_none() {
                        return Err("Select an actual project to open its Conversation".into());
                    }
                    self.advance_navigation()?;
                    self.unassigned = false;
                    self.focus = Focus::Conversation;
                    self.scroll = 0;
                    self.notice("Selected project Conversation; draft retained");
                } else {
                    self.queue_conversation()?;
                }
            }
            UiEvent::Scroll(delta) => self.scroll = self.scroll.saturating_add_signed(delta),
            UiEvent::ToggleBrief => self.toggle_brief()?,
            UiEvent::ToggleUnassigned => {
                self.unassigned = !self.unassigned;
                self.edit_requested = None;
                self.notice(if self.unassigned {
                    "Unassigned draft retained separately; Ctrl-U returns to the selected project"
                } else {
                    "Selected project draft; unassigned text remains separate"
                });
            }
            UiEvent::HistoryNext => {
                if let Some(cursor) = self.conversation_view().and_then(|v| v.page.next.clone()) {
                    self.start_conversation_read(Some(cursor))?;
                } else {
                    self.notice("No further protected history page is currently available");
                }
            }
            UiEvent::HistoryFirst => self.start_conversation_read(None)?,
        }
        if previous_project
            != self
                .selected_project()
                .map(|project| project.project_id.clone())
        {
            self.unassigned = false;
            self.edit_requested = None;
            self.invalidate_evidence()?;
        }
        self.sync_dispatch()?;
        self.sync_master()?;
        self.assert_consistent();
        Ok(())
    }

    pub fn apply_reply(&mut self, reply: WireReply) -> Result<(), String> {
        let previous_project = self
            .selected_project()
            .map(|project| project.project_id.clone());
        let completed = self
            .pending_creation
            .as_ref()
            .filter(|(id, _)| id == &reply.request_id)
            .map(|(_, generation)| *generation);
        if completed.is_some() {
            self.pending_creation = None;
        }
        if reply.revision.value() < self.revision.value() {
            self.notice("Stale engine reply ignored; retained current revision");
            return Ok(());
        }
        if reply.revision != self.revision {
            self.config = None;
            self.plan = None;
            self.invalidate_evidence()?;
        }
        self.revision = reply.revision;
        self.connected = true;
        match reply.outcome {
            Outcome::Snapshot { projects, tasks } => {
                if self.declaration_pending.is_some() && projects.len() > 128 {
                    return Err("Engine project snapshot exceeds the accepted bound".into());
                }
                self.snapshot_revision = Some(self.revision.clone());
                let selected = self.selected_project().map(|p| p.project_id.clone());
                let task = self.selected_task().cloned();
                self.projects = projects;
                self.tasks = tasks;
                self.selected = selected
                    .and_then(|id| self.projects.iter().position(|p| p.project_id == id))
                    .or_else(|| {
                        (self.declaration_pending.is_none() && !self.projects.is_empty())
                            .then_some(0)
                    });
                self.task = task
                    .as_ref()
                    .and_then(|old| {
                        self.project_tasks()
                            .position(|new| old.task_id == new.task_id)
                    })
                    .unwrap_or(0);
                self.join_creation()?;
                if self.selected_task() != task.as_ref() {
                    self.config = None;
                    self.plan = None;
                }
            }
            Outcome::Configuration { config } => {
                config.validate().map_err(|e| e.to_string())?;
                if self
                    .selected_task()
                    .is_some_and(|task| task.config_id == config.id)
                {
                    self.config = Some(config);
                    if !self.model_access.explicit {
                        self.model_access.pinned = true;
                    }
                }
            }
            Outcome::Coordination { state } => {
                if self.selected_task().is_some_and(|task| {
                    task.task_id == state.plan.scope.task_id
                        && task.project_id == state.plan.scope.project_id
                }) {
                    self.plan = Some(state);
                }
            }
            Outcome::Applied => {
                if let Some(generation) = completed {
                    if self.creating_project && generation == self.draft_generation {
                        self.creating_project = false;
                        self.project_draft.clear();
                    }
                    self.notice("Project created; refreshing durable engine state");
                }
            }
            Outcome::Rejected { code } => self.notice(&format!(
                "Engine rejected request: {code}. Refresh before retrying."
            )),
            Outcome::ResyncRequired => self.notice("Engine requests a fresh snapshot"),
            _ => self.notice("Unexpected outcome for the current TUI read"),
        }
        if previous_project
            != self
                .selected_project()
                .map(|project| project.project_id.clone())
        {
            self.invalidate_evidence()?;
        }
        self.sync_dispatch()?;
        self.sync_master()?;
        self.assert_consistent();
        Ok(())
    }

    fn queue_conversation(&mut self) -> Result<(), String> {
        if !self.owner_ready {
            self.notice("Leader conversation unavailable: authenticated owner API is required; draft retained");
            return Ok(());
        }
        if self.focus != Focus::Conversation || self.unassigned {
            return Err("Select the project Conversation to submit; draft retained".into());
        }
        if self.conversation_pending.is_some()
            || self.conversation_job.is_some()
            || self.master.has_pending()
        {
            return Err("An owner mutation is pending; draft retained. Ctrl-R reconnects and reconciles its exact identity".into());
        }
        self.ensure_draft()?;
        let index = self
            .draft_index()
            .ok_or("Select an actual project before sending owner input")?;
        let draft = &self.drafts[index];
        let text = if draft.editing_brief {
            &draft.brief
        } else {
            &draft.message
        };
        if text.trim().is_empty() {
            return Err("Owner input must contain non-whitespace text".into());
        }
        if !draft.editing_brief {
            let text = draft.message.clone();
            let generation = draft.message_generation;
            if self.master.submit(&text, generation)? {
                self.conversation_stale = true;
                self.notice("Submitting actual master turn; newer text remains a separate draft");
                return Ok(());
            }
        }
        let draft = &self.drafts[index];
        let (generation, mutation) = if draft.editing_brief {
            (
                draft.brief_generation,
                ConversationMutation::ReplaceBrief {
                    project_id: draft.project.clone(),
                    expected_version: draft
                        .expected_version
                        .ok_or("Explicit brief version is not loaded")?,
                    text: text.clone(),
                },
            )
        } else {
            (
                draft.message_generation,
                ConversationMutation::AppendMessage {
                    project_id: draft.project.clone(),
                    text: text.clone(),
                },
            )
        };
        assert!(generation > 0);
        let submission = ConversationSubmission {
            identity: self
                .owner_identity
                .as_ref()
                .expect("project draft has actual owner identity")
                .clone(),
            generation,
            mutation,
        };
        self.conversation_job = Some(ConversationJob::Mutation {
            submission: submission.clone(),
            delivery: MutationDelivery::Submit,
        });
        self.conversation_pending = Some(ConversationPending {
            submission,
            operation: None,
            retry_allowed: false,
        });
        self.conversation_stale = true;
        self.notice("Submitting durable owner input; newer text remains a separate draft");
        Ok(())
    }

    fn toggle_brief(&mut self) -> Result<(), String> {
        if !self.owner_ready || self.focus != Focus::Conversation || self.creating_project {
            return Err(
                "Brief editing requires the selected project's authenticated owner connection"
                    .into(),
            );
        }
        self.unassigned = false;
        self.ensure_draft()?;
        let index = self
            .draft_index()
            .ok_or("Select a project before editing its brief")?;
        if self.drafts[index].editing_brief {
            self.drafts[index].editing_brief = false;
            self.edit_requested = None;
            self.notice("Brief edit closed; both drafts retained");
        } else if self.conversation_view().is_some() && !self.conversation_stale {
            self.enter_brief()?;
        } else {
            self.edit_requested = Some((
                self.owner_identity.as_ref().unwrap().clone(),
                self.drafts[index].project.clone(),
            ));
            self.invalidate_conversation()?;
            self.notice("Loading the actual current brief for explicit versioned editing");
        }
        Ok(())
    }

    fn reduce_declaration(&mut self, event: &UiEvent) -> Result<bool, String> {
        match event {
            UiEvent::Character(character) => self.declaration.edit(Some(*character))?,
            UiEvent::Backspace => self.declaration.edit(None)?,
            UiEvent::FocusNext | UiEvent::FocusPrevious => {
                let count = self.declaration.repositories.len() + 1;
                let delta = if matches!(event, UiEvent::FocusNext) {
                    1
                } else {
                    count - 1
                };
                self.declaration.field = (self.declaration.field + delta) % count;
            }
            UiEvent::DeclarationAdd => {
                if self.declaration.repositories.len() == 64 {
                    return Err("At most 64 repository rows; draft retained".into());
                }
                self.declaration.advance()?;
                self.declaration.repositories.push(String::new());
                self.declaration.field = self.declaration.repositories.len();
            }
            UiEvent::DeclarationRemove => {
                if self.declaration.repositories.len() > 1 && self.declaration.field > 0 {
                    self.declaration.advance()?;
                    self.declaration
                        .repositories
                        .remove(self.declaration.field - 1);
                    self.declaration.field = self
                        .declaration
                        .field
                        .min(self.declaration.repositories.len());
                }
            }
            UiEvent::Cancel => {
                self.advance_navigation()?;
                self.declaration.open = false;
                self.notice("Declaration closed; literal draft and any pending intent retained");
            }
            UiEvent::Submit => self.queue_creation()?,
            UiEvent::SelectionNext | UiEvent::SelectionPrevious | UiEvent::Scroll(_) => {}
            _ => return Ok(false),
        }
        Ok(true)
    }
}
