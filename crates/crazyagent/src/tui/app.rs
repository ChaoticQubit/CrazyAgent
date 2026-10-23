use super::client::{
    ConversationContext, ConversationEvent, ConversationJob, ConversationQuery,
    ConversationSubmission, CreationEvent, CreationJob, CreationSubmission, Evidence,
    MutationDelivery, OwnerEvent, OwnerProgress, SetupQuery, SetupView,
};
use super::{dispatch, master_session, model_access};
use crate::config::EffectiveConfig;
use crate::conversation::{
    ConversationContent, ConversationCursor, ConversationEntry, ConversationMutation,
    ConversationPage,
};
use crate::coordination::PlanState;
use crate::engine::bootstrap::OwnerConnectionIdentity;
use crate::engine::owner::{OwnerData, OwnerReadReply, OwnerResponse, OwnerResult};
use crate::protocol::{
    Command, Cursor, ExecutionContext, Outcome, ProjectId, ProjectSpec, RequestId, StoredProject,
    WireReply,
};

pub const DRAFT_BYTES_MAX: usize = 4096;

const FOCI: [Focus; 7] = [
    Focus::Conversation,
    Focus::Projects,
    Focus::Settings,
    Focus::Plan,
    Focus::Supervision,
    Focus::Memory,
    Focus::Evidence,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Conversation,
    Projects,
    Settings,
    Plan,
    Supervision,
    Memory,
    Evidence,
}

pub enum UiEvent {
    FocusNext,
    FocusPrevious,
    Character(char),
    Backspace,
    Resize(u16, u16),
    SelectionNext,
    SelectionPrevious,
    TaskNext,
    BeginProject,
    BeginDeclaration,
    DeclarationAdd,
    DeclarationRemove,
    Submit,
    Cancel,
    Scroll(i16),
    ToggleBrief,
    ToggleUnassigned,
    HistoryNext,
    HistoryFirst,
    ConfigureMaster,
    CancelMaster,
    CloseMaster,
}

struct ProjectDraft {
    database: String,
    project: ProjectId,
    message: String,
    message_generation: u64,
    brief: String,
    brief_generation: u64,
    expected_version: Option<u64>,
    editing_brief: bool,
}

pub(super) struct ConversationView {
    pub context: ConversationContext,
    pub page: ConversationPage,
    pub brief: Option<ConversationEntry>,
    pub revision: Cursor,
}

struct ConversationPending {
    submission: ConversationSubmission,
    operation: Option<(RequestId, String)>,
    retry_allowed: bool,
}

pub(super) struct Declaration {
    pub name: String,
    pub repositories: Vec<String>,
    pub field: usize,
    pub generation: u64,
    pub open: bool,
}

impl Default for Declaration {
    fn default() -> Self {
        Self {
            name: String::new(),
            repositories: vec![String::new()],
            field: 0,
            generation: 1,
            open: false,
        }
    }
}

impl Declaration {
    fn active(&mut self) -> &mut String {
        if self.field == 0 {
            &mut self.name
        } else {
            &mut self.repositories[self.field - 1]
        }
    }

    pub(super) fn bytes(&self) -> usize {
        self.name.len() + self.repositories.iter().map(String::len).sum::<usize>()
    }

    fn advance(&mut self) -> Result<(), String> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("Declaration generation exhausted")?;
        Ok(())
    }

    fn edit(&mut self, character: Option<char>) -> Result<(), String> {
        if let Some(character) = character {
            let limit = if self.field == 0 { 256 } else { 4096 };
            if character.is_control()
                || self.active().len() + character.len_utf8() > limit
                || self.bytes() + character.len_utf8() > 65536
            {
                return Err(
                    "Declaration field or total UTF-8 byte limit reached; input retained".into(),
                );
            }
            self.advance()?;
            self.active().push(character);
        } else if !self.active().is_empty() {
            self.advance()?;
            self.active().pop();
        }
        Ok(())
    }

    pub(super) fn spec(&self) -> Result<ProjectSpec, String> {
        let spec = ProjectSpec {
            name: self.name.clone(),
            repositories: self.repositories.clone(),
        };
        spec.validate().map_err(|e| e.to_string())?;
        if self.bytes() > 65536
            || spec.repositories.is_empty()
            || spec
                .repositories
                .iter()
                .any(|path| !path.starts_with('/') || path.trim().is_empty())
        {
            return Err(
                "Declare 1 to 64 absolute local repository paths; literal input retained".into(),
            );
        }
        Ok(spec)
    }

    fn assert_consistent(&self) {
        assert!((1..=64).contains(&self.repositories.len()));
        assert!(self.field <= self.repositories.len());
        assert!(self.generation > 0);
        assert!(self.name.len() <= 256);
        assert!(self.repositories.iter().all(|path| path.len() <= 4096));
        assert!(self.bytes() <= 65536);
    }
}

struct CreationPending {
    submission: CreationSubmission,
    operation: Option<(RequestId, String)>,
    retry_allowed: bool,
    committed: Option<Cursor>,
}

pub struct App {
    pub(super) declaration: Declaration,
    navigation_generation: u64,
    declaration_pending: Option<CreationPending>,
    creation_job: Option<CreationJob>,
    snapshot_revision: Option<Cursor>,
    pub(super) model_access: model_access::State,
    pub(super) dispatch: dispatch::State,
    pub(super) master: master_session::State,
    focus: Focus,
    draft: String,
    project_draft: String,
    viewport: (u16, u16),
    revision: Cursor,
    pub projects: Vec<StoredProject>,
    pub tasks: Vec<ExecutionContext>,
    pub config: Option<Box<EffectiveConfig>>,
    pub plan: Option<Box<PlanState>>,
    pub connected: bool,
    pub status: String,
    pub scroll: u16,
    selected: Option<usize>,
    task: usize,
    creating_project: bool,
    command: Option<Command>,
    draft_generation: u64,
    submitted_generation: Option<u64>,
    pending_creation: Option<(RequestId, u64)>,
    pub(super) owner_ready: bool,
    pub(super) owner_problem: Option<String>,
    pub(super) owner_notice: String,
    pub(super) evidence: Option<Evidence>,
    owner_generation: u64,
    owner_step: Option<bool>,
    owner_identity: Option<OwnerConnectionIdentity>,
    drafts: Vec<ProjectDraft>,
    unassigned: bool,
    conversation_generation: u64,
    pub(super) conversation_stale: bool,
    pub(super) conversation_notice: String,
    conversation: Option<ConversationView>,
    conversation_stage: Option<ConversationQuery>,
    conversation_page: Option<OwnerReadReply>,
    conversation_cursor: Option<ConversationCursor>,
    conversation_pending: Option<ConversationPending>,
    conversation_job: Option<ConversationJob>,
    edit_requested: Option<(OwnerConnectionIdentity, ProjectId)>,
    brief_completion: Option<(String, ProjectId)>,
    setup_query: Option<SetupQuery>,
    pub(super) setup_view: Option<(SetupQuery, SetupView)>,
    pub(super) setup_problem: Option<String>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

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
            let project = self.selected_project()
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

    pub(super) fn owner_label(&self) -> &'static str {
        if self.owner_ready {
            "Owner: connected"
        } else if self.owner_problem.is_some() {
            "Owner: limited"
        } else {
            "Owner: connecting"
        }
    }

    fn invalidate_evidence(&mut self) -> Result<(), String> {
        self.owner_generation = self
            .owner_generation
            .checked_add(1)
            .ok_or("Owner view generation exhausted")?;
        self.evidence = None;
        self.owner_step = Some(true);
        self.owner_notice = "Evidence is stale; waiting for a current protected read".into();
        self.invalidate_conversation()?;
        Ok(())
    }

    pub(super) fn reconnect_owner(&mut self) -> Result<(), String> {
        self.snapshot_revision = None;
        self.model_access.access(None, false);
        self.invalidate_evidence()?;
        self.owner_ready = false;
        self.sync_master()?;
        self.master.access();
        self.owner_problem = None;
        self.owner_notice = "Reconnecting to the original configured engine endpoint".into();
        self.sync_dispatch()?;
        Ok(())
    }

    fn sync_dispatch(&mut self) -> Result<(), String> {
        let selected = self
            .owner_identity
            .as_ref()
            .zip(self.selected_task())
            .zip(self.plan.as_deref())
            .filter(|((_, task), plan)| &plan.plan.scope == *task);
        let (selection, attempts) = if let Some(((identity, task), plan)) = selected {
            let attempts = dispatch::reserved_attempts(task, plan)?;
            let selection = dispatch::Selection {
                database: identity.database_instance().to_owned(),
                epoch: identity.engine_epoch().to_owned(),
                task: task.clone(),
                plan_id: plan.plan.id.clone(),
            };
            (Some(selection), attempts)
        } else {
            (None, Vec::new())
        };
        self.dispatch
            .select(selection, attempts, self.owner_ready, self.revision.clone())
    }

    pub(super) fn dispatch_request(&mut self) -> Result<Option<dispatch::Query>, String> {
        self.sync_dispatch()?;
        if !matches!(self.focus, Focus::Plan | Focus::Supervision)
            || self.is_creating_project()
            || self.snapshot_revision.as_ref() != Some(&self.revision)
        {
            return Ok(None);
        }
        Ok(self.dispatch.request())
    }

    pub(super) fn owner_continuation(&self) -> bool {
        self.owner_step == Some(false)
            || (self.focus == Focus::Conversation && self.conversation_stage.is_some())
    }

    pub(super) fn evidence_request(&mut self) -> Result<Option<(ProjectId, u64, bool)>, String> {
        if !self.owner_ready || self.focus != Focus::Evidence {
            return Ok(None);
        }
        let Some(project) = self
            .selected_project()
            .map(|project| project.project_id.clone())
        else {
            return Ok(None);
        };
        if self.owner_step.is_none() {
            self.invalidate_evidence()?;
        }
        let first = self
            .owner_step
            .take()
            .expect("evidence scheduling prepares one step");
        if first {
            self.evidence = None;
            self.owner_notice = "Loading actual acceptance baseline".into();
        }
        Ok(Some((project, self.owner_generation, first)))
    }

    pub(super) fn apply_owner(&mut self, event: OwnerEvent) -> Result<(), String> {
        match event {
            OwnerEvent::Master(event) => {
                if let Some(submission) = self.master.apply(*event)? {
                    self.complete_master_draft(&submission)?;
                    self.invalidate_evidence()?;
                }
                if self.master.disconnected() {
                    self.owner_ready = false;
                    self.owner_problem = Some("master_owner_connection_unavailable".into());
                }
                let notice = self.master.notice.clone();
                self.notice(&notice);
            }
            OwnerEvent::Dispatch(event) => {
                self.sync_dispatch()?;
                if let Some(problem) = self.dispatch.apply(*event) {
                    self.owner_ready = false;
                    self.owner_problem = Some(problem.clone());
                    self.notice(&format!(
                        "Dispatch readback unavailable; limited mode. {problem}"
                    ));
                }
            }
            OwnerEvent::Creation(event) => self.apply_creation(*event)?,
            OwnerEvent::Setup(event) => match event.result {
                Ok(view) => {
                    self.setup_view = Some((event.query, view));
                    self.setup_problem = None;
                }
                Err(problem) => {
                    self.notice(&format!("Project setup read unavailable. {problem}"));
                    self.setup_problem = Some(problem);
                }
            },
            OwnerEvent::ModelAccess(event) => {
                self.model_access.apply(*event)?;
                if self.model_access.disconnected() {
                    self.owner_ready = false;
                    self.owner_problem = Some("metadata_owner_connection_unavailable".into());
                }
                let notice = self.model_access.notice.clone();
                self.notice(&notice);
            }
            OwnerEvent::Access {
                ready,
                problem,
                identity,
            } => {
                self.model_access.access(identity.clone(), ready);
                self.snapshot_revision = None;
                if let Some(identity) = identity {
                    if self
                        .owner_identity
                        .as_ref()
                        .is_some_and(|old| old.database_instance() != identity.database_instance())
                    {
                        self.conversation = None;
                    }
                    self.owner_identity = Some(identity);
                }
                self.invalidate_evidence()?;
                self.owner_ready = ready;
                self.sync_master()?;
                self.master.access();
                self.owner_problem = (!ready).then_some(problem);
                if !ready {
                    self.owner_notice =
                        "Evidence and approvals are disabled; Ctrl-R reconnects".into();
                } else if let Some(pending) = self
                    .declaration_pending
                    .as_ref()
                    .filter(|p| p.operation.is_some() && p.committed.is_none())
                {
                    if self.owner_identity.as_ref().is_some_and(|identity| {
                        identity.database_instance()
                            == pending.submission.identity.database_instance()
                    }) {
                        self.creation_job = Some(CreationJob {
                            submission: pending.submission.clone(),
                            delivery: MutationDelivery::Reconcile,
                        });
                    }
                } else if let Some(pending) = &self.conversation_pending {
                    self.conversation_job = Some(ConversationJob::Mutation {
                        submission: pending.submission.clone(),
                        delivery: MutationDelivery::Reconcile,
                    });
                }
            }
            OwnerEvent::Conversation(event) => self.apply_conversation(*event)?,
            OwnerEvent::Read {
                project,
                generation,
                result,
            } => {
                let progress = match result {
                    Ok(progress) => progress,
                    Err(problem) => {
                        self.invalidate_evidence()?;
                        self.owner_ready = false;
                        self.owner_problem = Some(problem);
                        self.owner_notice =
                            "Evidence and approvals are disabled; Ctrl-R reconnects".into();
                        self.sync_dispatch()?;
                        return Ok(());
                    }
                };
                if generation != self.owner_generation
                    || !self
                        .selected_project()
                        .is_some_and(|selected| selected.project_id == project)
                    || !self.owner_ready
                {
                    return Ok(());
                }
                match progress {
                    OwnerProgress::Continue => self.owner_step = Some(false),
                    OwnerProgress::Refresh => self.invalidate_evidence()?,
                    OwnerProgress::Empty { revision } => {
                        self.owner_notice = format!(
                            "No acceptance baseline for this project at owner revision {}",
                            revision.value()
                        );
                    }
                    OwnerProgress::Incomplete { revision } => {
                        self.owner_notice = format!(
                            "Baseline discovery incomplete at owner revision {}: four-page limit reached; absence is not established",
                            revision.value()
                        );
                    }
                    OwnerProgress::Ready(evidence) => {
                        if evidence.project != project || evidence.revision != self.revision {
                            self.invalidate_evidence()?;
                            return Ok(());
                        }
                        self.owner_notice =
                            "Actual protected baseline detail; approval is a separate owner action"
                                .into();
                        self.evidence = Some(evidence);
                    }
                }
            }
        }
        self.sync_dispatch()?;
        self.sync_master()?;
        Ok(())
    }

    fn sync_master(&mut self) -> Result<(), String> {
        self.master.select(
            self.owner_identity.clone(),
            self.selected_project()
                .map(|project| project.project_id.clone()),
            self.owner_ready,
        )
    }

    pub(super) fn master_request(&mut self) -> Result<Option<master_session::Job>, String> {
        self.sync_master()?;
        if self.conversation_pending.is_some()
            || self.declaration_pending.is_some()
            || self.model_access.has_pending()
        {
            return Ok(None);
        }
        Ok(self
            .master
            .request(self.focus == Focus::Conversation && !self.is_creating_project()))
    }

    fn complete_master_draft(
        &mut self,
        submission: &master_session::Submission,
    ) -> Result<(), String> {
        let crate::master_session::MasterOwnerAction::Submit {
            project_id, text, ..
        } = &submission.action
        else {
            return Ok(());
        };
        let draft = self
            .drafts
            .iter_mut()
            .find(|draft| {
                draft.database == submission.identity.database_instance()
                    && &draft.project == project_id
            })
            .ok_or("Submitted master draft disappeared")?;
        if draft.message_generation == submission.generation && &draft.message == text {
            draft.message.clear();
            draft.message_generation = draft
                .message_generation
                .checked_add(1)
                .ok_or("Master input generation exhausted")?;
        }
        Ok(())
    }

    fn draft_index(&self) -> Option<usize> {
        if self.creating_project || self.unassigned {
            return None;
        }
        let identity = self.owner_identity.as_ref()?;
        let project = &self.selected_project()?.project_id;
        self.drafts
            .iter()
            .position(|d| d.database == identity.database_instance() && &d.project == project)
    }

    fn ensure_draft(&mut self) -> Result<(), String> {
        if self.creating_project || self.unassigned || self.draft_index().is_some() {
            return Ok(());
        }
        let Some(identity) = &self.owner_identity else {
            return Ok(());
        };
        let Some(project) = self.selected_project() else {
            return Ok(());
        };
        if self.drafts.len() >= 128 {
            return Err("Project draft capacity reached; retained drafts were not evicted".into());
        }
        self.drafts.push(ProjectDraft {
            database: identity.database_instance().to_owned(),
            project: project.project_id.clone(),
            message: String::new(),
            message_generation: 1,
            brief: String::new(),
            brief_generation: 1,
            expected_version: None,
            editing_brief: false,
        });
        Ok(())
    }

    fn advance_input_generation(&mut self) -> Result<(), String> {
        if self.creating_project {
            return self.advance_generation();
        }
        if let Some(index) = self.draft_index() {
            let draft = &mut self.drafts[index];
            let generation = if draft.editing_brief {
                &mut draft.brief_generation
            } else {
                &mut draft.message_generation
            };
            *generation = generation
                .checked_add(1)
                .ok_or("Conversation draft generation exhausted")?;
        }
        Ok(())
    }

    pub(super) fn is_editing_brief(&self) -> bool {
        self.draft_index()
            .is_some_and(|i| self.drafts[i].editing_brief)
    }

    pub(super) fn draft_label(&self) -> String {
        if self.focus == Focus::Settings && self.model_access.home_draft().is_some() {
            return "SDK home | Enter selects transient intent | Esc retains input".into();
        }
        if self.declaration.open {
            return if self.declaration.field == 0 {
                "Declared project name".into()
            } else {
                format!(
                    "Repository {} of {} | literal absolute path",
                    self.declaration.field,
                    self.declaration.repositories.len()
                )
            };
        }
        if self.master.is_open() {
            return self.master.draft_label().into();
        }
        if self.creating_project {
            return "Project name".into();
        }
        if let Some(index) = self.draft_index() {
            let draft = &self.drafts[index];
            if draft.editing_brief {
                return format!(
                    "Brief edit | expected version {}",
                    draft.expected_version.unwrap()
                );
            }
            return format!(
                "Owner message | {} | unassigned retained: {} bytes",
                draft.project.as_str(),
                self.draft.len()
            );
        }
        if self.owner_identity.is_some() && self.selected_project().is_some() && !self.unassigned {
            return format!(
                "Owner message | selected project | unassigned retained: {} bytes",
                self.draft.len()
            );
        }
        "Unassigned leader draft | Ctrl-U returns to project; text is retained separately".into()
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

    fn enter_brief(&mut self) -> Result<(), String> {
        if self.conversation_stale || !self.owner_ready {
            return Err("Current protected brief is stale; draft retained".into());
        }
        let view = self
            .conversation_view()
            .ok_or("Current protected brief is not loaded")?;
        let (version, text) = match &view.brief {
            Some(entry) => match entry.content {
                ConversationContent::Brief { version } => (version, entry.text.clone()),
                _ => return Err("Current protected brief has invalid content kind".into()),
            },
            None => (0, String::new()),
        };
        self.ensure_draft()?;
        self.brief_completion = None;
        let index = self.draft_index().ok_or("Project draft unavailable")?;
        let draft = &mut self.drafts[index];
        let cannot_prefill = text.len() > DRAFT_BYTES_MAX || text.chars().any(char::is_control);
        if draft.expected_version.is_none() && draft.brief.is_empty() && !cannot_prefill {
            draft.brief = text;
        }
        draft.expected_version = Some(version);
        draft.brief_generation = draft
            .brief_generation
            .checked_add(1)
            .ok_or("Brief draft generation exhausted")?;
        draft.editing_brief = true;
        self.edit_requested = None;
        self.notice(if cannot_prefill {
            "Current brief cannot be copied into this bounded editor; enter an explicit replacement. Original history is retained"
        } else {
            "Explicit brief edit: Enter replaces the pinned version; Esc retains both drafts"
        });
        Ok(())
    }

    fn start_conversation_read(
        &mut self,
        cursor: Option<ConversationCursor>,
    ) -> Result<(), String> {
        self.conversation_generation = self
            .conversation_generation
            .checked_add(1)
            .ok_or("Conversation view generation exhausted")?;
        self.conversation_cursor = cursor.clone();
        self.conversation_stage = Some(ConversationQuery::History { cursor });
        self.conversation_page = None;
        self.conversation_stale = true;
        Ok(())
    }

    fn invalidate_conversation(&mut self) -> Result<(), String> {
        self.start_conversation_read(None)?;
        if self
            .edit_requested
            .as_ref()
            .is_some_and(|(identity, project)| {
                self.owner_identity.as_ref() != Some(identity)
                    || !self
                        .selected_project()
                        .is_some_and(|p| &p.project_id == project)
            })
        {
            self.edit_requested = None;
        }
        self.conversation_notice =
            "Protected conversation is stale; waiting for a current read".into();
        Ok(())
    }

    pub(super) fn conversation_view(&self) -> Option<&ConversationView> {
        let identity = self.owner_identity.as_ref()?;
        let project = &self.selected_project()?.project_id;
        self.conversation.as_ref().filter(|v| {
            v.context.identity.database_instance() == identity.database_instance()
                && &v.context.project == project
        })
    }

    pub(super) fn conversation_request(&mut self) -> Result<Option<ConversationJob>, String> {
        if !self.owner_ready || self.focus != Focus::Conversation || self.creating_project {
            return Ok(None);
        }
        let Some(identity) = self.owner_identity.clone() else {
            return Ok(None);
        };
        let Some(project) = self.selected_project().map(|p| p.project_id.clone()) else {
            return Ok(None);
        };
        if self.conversation_stage.is_none() {
            self.start_conversation_read(self.conversation_cursor.clone())?;
        }
        let query = self
            .conversation_stage
            .take()
            .expect("conversation read stage was prepared");
        Ok(Some(ConversationJob::Read {
            context: ConversationContext {
                identity,
                project,
                generation: self.conversation_generation,
            },
            query,
        }))
    }

    pub(super) fn take_conversation_job(&mut self) -> Option<ConversationJob> {
        self.conversation_job.take()
    }

    pub(super) fn model_access_request(&mut self) -> Result<Option<model_access::Job>, String> {
        if self.conversation_pending.is_some()
            || self.declaration_pending.is_some()
            || self.master.has_pending()
            || self.declaration.open
        {
            return Ok(None);
        }
        self.model_access.request(self.focus == Focus::Settings)
    }

    pub(super) fn retry_conversation(&mut self) -> bool {
        let Some(pending) = self
            .conversation_pending
            .as_mut()
            .filter(|p| p.retry_allowed)
        else {
            return false;
        };
        if !self.owner_ready || self.conversation_job.is_some() {
            return false;
        }
        pending.retry_allowed = false;
        self.conversation_job = Some(ConversationJob::Mutation {
            submission: pending.submission.clone(),
            delivery: MutationDelivery::Retry,
        });
        self.notice("Retrying the exact retained owner request without changing revision or text");
        true
    }

    pub(super) fn has_unresolved_conversation(&self) -> bool {
        self.conversation_pending
            .as_ref()
            .is_some_and(|p| p.operation.is_some())
    }

    pub(super) fn pending_conversation_label(&self) -> Option<String> {
        self.conversation_pending.as_ref().map(|p| match &p.operation {
            Some((id, _)) => format!("Owner operation {} unresolved; Ctrl-R reconnects/reconciles, or retries after an actual not-recorded barrier", id.as_str()),
            None => "Owner input queued or running; another mutation is blocked, drafts remain editable".into(),
        })
    }

    fn apply_conversation(&mut self, event: ConversationEvent) -> Result<(), String> {
        match event {
            ConversationEvent::Read { context, result } => {
                let state = match result {
                    Ok(state) => *state,
                    Err(problem) => {
                        self.invalidate_evidence()?;
                        self.owner_ready = false;
                        self.owner_problem = Some(problem);
                        self.conversation_notice = "Protected reads and input disabled; drafts retained; Ctrl-R reconnects".into();
                        return Ok(());
                    }
                };
                if self.owner_identity.as_ref() != Some(&context.identity)
                    || context.generation != self.conversation_generation
                    || !self
                        .selected_project()
                        .is_some_and(|p| p.project_id == context.project)
                    || !self.owner_ready
                {
                    return Ok(());
                }
                self.collect_conversation(context, state)?;
            }
            ConversationEvent::Mutation {
                submission,
                operation,
                result,
                retained,
            } => {
                let pending = self
                    .conversation_pending
                    .as_mut()
                    .expect("owner mutation reply requires retained UI submission");
                assert_eq!(pending.submission, submission);
                if pending
                    .operation
                    .as_ref()
                    .is_some_and(|id| operation.as_ref() != Some(id))
                {
                    return Err(
                        "Owner completion identity changed; retained the original intent".into(),
                    );
                }
                pending.operation = operation.clone();
                match result {
                    Ok(response) => match *response {
                        OwnerResponse::Applied { operation: applied }
                        | OwnerResponse::Committed { operation: applied } => {
                            if operation.as_ref() != Some(&(applied.provenance.operation_id.clone(),
                                applied.provenance.request_hash.clone())) || retained
                            {
                                return Err("Owner completion provenance mismatch".into());
                            }
                            match applied.result {
                                OwnerResult::Applied => {
                                    self.complete_conversation(&submission)?;
                                    self.notice("Owner input durably recorded; refreshing actual history");
                                }
                                OwnerResult::Rejected { code } => self.notice(&format!(
                                    "Owner rejected input: {code}; draft retained. Ctrl-B closes/reopens the explicit brief version")),
                                _ => return Err("Unexpected conversation mutation result".into()),
                            }
                            self.conversation_pending = None;
                            self.invalidate_evidence()?;
                        }
                        OwnerResponse::NotRecordedAtBarrier { .. } => {
                            assert!(retained);
                            self.conversation_pending.as_mut().unwrap().retry_allowed = true;
                            self.notice("Original owner operation not recorded at actual barrier; Ctrl-R explicitly retries that exact request");
                        }
                        OwnerResponse::OutcomeUnknown { .. } => {
                            assert!(retained);
                            self.notice("Owner outcome unknown; original identity and drafts retained; Ctrl-R reconnects and reconciles");
                        }
                        OwnerResponse::Rejected { code } => self.notice(&format!(
                            "Owner admission rejected: {code}; original intent retained; Ctrl-R reconnects and reconciles")),
                        OwnerResponse::Read { .. } => return Err("Unexpected read for owner mutation".into()),
                    },
                    Err(problem) => {
                        if !retained {
                            self.conversation_pending = None;
                        }
                        self.invalidate_evidence()?;
                        self.owner_ready = false;
                        self.owner_problem = Some(problem);
                        self.notice("Owner input unavailable or outcome uncertain; draft and any original intent retained; Ctrl-R reconnects");
                    }
                }
            }
        }
        Ok(())
    }

    fn collect_conversation(
        &mut self,
        context: ConversationContext,
        state: OwnerReadReply,
    ) -> Result<(), String> {
        if state.revision != self.revision || matches!(state.data, OwnerData::ResyncRequired) {
            self.invalidate_conversation()?;
            return Ok(());
        }
        match state.data {
            OwnerData::ConversationHistory { .. } => {
                self.conversation_page = Some(state);
                self.conversation_stage = Some(ConversationQuery::Brief);
            }
            OwnerData::CurrentBrief { brief, .. } => {
                let Some(history) = self.conversation_page.take() else {
                    return Err("Current brief arrived without its bounded history page".into());
                };
                if history.revision != state.revision || history.config_id != state.config_id {
                    self.invalidate_conversation()?;
                    return Ok(());
                }
                let OwnerData::ConversationHistory { page } = history.data else {
                    return Err("Protected history staging kind mismatch".into());
                };
                self.conversation_notice = if page.next.is_some() {
                    "Partial protected history page; Ctrl-N reads next, Ctrl-P returns to first"
                } else {
                    "Actual protected history; leader consumption and replies are not integrated"
                }
                .into();
                self.conversation = Some(ConversationView {
                    context: context.clone(),
                    page,
                    brief,
                    revision: state.revision,
                });
                self.conversation_stale = false;
                self.conversation_stage = None;
                if self
                    .edit_requested
                    .as_ref()
                    .is_some_and(|(id, p)| id == &context.identity && p == &context.project)
                {
                    self.enter_brief()?;
                }
            }
            _ => return Err("Unexpected protected conversation data".into()),
        }
        Ok(())
    }

    fn complete_conversation(&mut self, submission: &ConversationSubmission) -> Result<(), String> {
        let (project, text, brief) = match &submission.mutation {
            ConversationMutation::AppendMessage { project_id, text } => (project_id, text, false),
            ConversationMutation::ReplaceBrief {
                project_id, text, ..
            } => (project_id, text, true),
        };
        let Some(draft) = self.drafts.iter_mut().find(|d| {
            d.database == submission.identity.database_instance() && &d.project == project
        }) else {
            return Err("Submitted project draft disappeared".into());
        };
        let (buffer, generation) = if brief {
            (&mut draft.brief, &mut draft.brief_generation)
        } else {
            (&mut draft.message, &mut draft.message_generation)
        };
        if *generation == submission.generation && buffer.as_str() == text.as_str() {
            buffer.clear();
            *generation = generation
                .checked_add(1)
                .ok_or("Conversation draft generation exhausted")?;
            if brief {
                if draft.editing_brief {
                    self.brief_completion = Some((draft.database.clone(), draft.project.clone()));
                }
                draft.editing_brief = false;
                draft.expected_version = None;
            }
        }
        Ok(())
    }

    fn advance_navigation(&mut self) -> Result<(), String> {
        self.navigation_generation = self
            .navigation_generation
            .checked_add(1)
            .ok_or("Navigation generation exhausted")?;
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

    pub(super) fn queue_creation(&mut self) -> Result<(), String> {
        let spec = self.declaration.spec()?;
        self.submit_creation(spec, None)?;
        self.notice("Submitting declared project; first brief remains a separate Ctrl-B action");
        Ok(())
    }

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

    pub(super) fn take_setup_query(&mut self) -> Option<SetupQuery> {
        self.setup_query.take()
    }

    /// Gives the latest New Project read with the query that produced it.
    /// Takes the app state.
    /// Returns the query and view, or none before the first read.
    pub fn setup_view(&self) -> Option<&(SetupQuery, SetupView)> {
        self.setup_view.as_ref()
    }

    fn submit_creation(
        &mut self,
        spec: ProjectSpec,
        setup: Option<crate::project_setup::ProjectSetup>,
    ) -> Result<(), String> {
        if !self.owner_ready {
            return Err("Owner creation disabled in limited mode; declaration retained".into());
        }
        if self.declaration_pending.is_some()
            || self.conversation_pending.is_some()
            || self.master.has_pending()
            || self.conversation_job.is_some()
            || self.model_access.has_pending()
            || self.pending_creation.is_some()
            || self.command.is_some()
        {
            return Err("An owner or project intent is unresolved; declaration retained".into());
        }
        let submission = CreationSubmission {
            identity: self
                .owner_identity
                .clone()
                .ok_or("Owner identity unavailable")?,
            generation: self.declaration.generation,
            navigation: self.navigation_generation,
            project_id: ProjectId::new(format!(
                "project-{}",
                crate::engine::owner::random_id().map_err(|e| e.to_string())?
            ))
            .map_err(|e| e.to_string())?,
            spec,
            setup,
        };
        self.creation_job = Some(CreationJob {
            submission: submission.clone(),
            delivery: MutationDelivery::Submit,
        });
        self.declaration_pending = Some(CreationPending {
            submission,
            operation: None,
            retry_allowed: false,
            committed: None,
        });
        Ok(())
    }

    pub(super) fn take_creation_job(&mut self) -> Option<CreationJob> {
        self.creation_job.take()
    }

    pub(super) fn has_unresolved_creation(&self) -> bool {
        self.declaration_pending.is_some()
    }

    pub(super) fn creation_pending_label(&self) -> Option<String> {
        self.declaration_pending
            .as_ref()
            .map(|pending| match &pending.operation {
                Some((id, hash)) => format!("Creation operation {} {}", id.as_str(), hash),
                None => {
                    "Creation queued; immutable project and repository membership retained".into()
                }
            })
    }

    pub(super) fn retry_creation(&mut self) -> bool {
        let Some(pending) = self.declaration_pending.as_mut() else {
            return false;
        };
        if !pending.retry_allowed
            || !self.owner_ready
            || self.creation_job.is_some()
            || pending.committed.is_some()
        {
            return false;
        }
        pending.retry_allowed = false;
        self.creation_job = Some(CreationJob {
            submission: pending.submission.clone(),
            delivery: MutationDelivery::Retry,
        });
        self.notice("Retrying exact declared-project intent after an actual not-recorded barrier");
        true
    }

    pub(super) fn apply_creation(&mut self, event: CreationEvent) -> Result<(), String> {
        let pending = self
            .declaration_pending
            .as_mut()
            .ok_or("Creation completion has no retained submission")?;
        assert_eq!(pending.submission, event.submission);
        if pending
            .operation
            .as_ref()
            .is_some_and(|op| event.operation.as_ref() != Some(op))
        {
            return Err("Creation operation identity changed; original intent retained".into());
        }
        pending.operation = event.operation.clone();
        pending.retry_allowed = false;
        match event.result {
            Ok(response) => self.creation_receipt(*response, event.retained)?,
            Err(problem) => {
                if !event.retained {
                    self.declaration_pending = None;
                }
                self.owner_ready = false;
                self.owner_problem = Some(problem.clone());
                if event.retained {
                    self.notice("Creation outcome uncertain; exact intent and draft retained; Ctrl-R reconnects");
                } else {
                    self.notice(&format!("Creation not submitted: {problem}; literal draft retained; Ctrl-R reconnects"));
                }
            }
        }
        Ok(())
    }

    fn creation_receipt(&mut self, response: OwnerResponse, retained: bool) -> Result<(), String> {
        match response {
            OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
                let pending = self.declaration_pending.as_mut().unwrap();
                if retained
                    || pending.operation.as_ref()
                        != Some(&(
                            operation.provenance.operation_id,
                            operation.provenance.request_hash,
                        ))
                {
                    return Err("Creation receipt provenance mismatch; intent retained".into());
                }
                match operation.result {
                    OwnerResult::Applied => {
                        pending.committed = Some(operation.provenance.revision_after);
                        self.notice(
                            "Project recorded; waiting for exact identity in full snapshot",
                        );
                        self.join_creation()?;
                    }
                    OwnerResult::Rejected { code } => {
                        self.declaration_pending = None;
                        self.notice(&format!(
                            "Creation recorded rejection: {code}; draft retained"
                        ));
                    }
                    _ => return Err("Unexpected creation result".into()),
                }
            }
            OwnerResponse::NotRecordedAtBarrier { .. } => {
                if !retained {
                    return Err("Creation barrier lost its retained intent".into());
                }
                self.declaration_pending.as_mut().unwrap().retry_allowed = true;
                self.notice("Creation not recorded at actual barrier; Ctrl-R explicitly retries exact intent");
            }
            OwnerResponse::OutcomeUnknown { operation_id } => {
                let pending = self.declaration_pending.as_ref().unwrap();
                if !retained
                    || !pending
                        .operation
                        .as_ref()
                        .is_some_and(|op| op.0 == operation_id)
                {
                    return Err("Creation unknown-outcome identity mismatch".into());
                }
                self.notice(
                    "Creation outcome unknown; Ctrl-R reconnects and reconciles exact intent",
                );
            }
            OwnerResponse::Rejected { code } => {
                if !retained {
                    return Err("Creation admission rejection lost its intent".into());
                }
                self.notice(&format!(
                    "Creation admission rejected: {code}; uncertain intent retained"
                ));
            }
            OwnerResponse::Read { .. } => return Err("Unexpected read for creation".into()),
        }
        Ok(())
    }

    fn creation_matches(&self, submission: &CreationSubmission) -> bool {
        self.declaration.open
            && self.focus == Focus::Projects
            && self.declaration.generation == submission.generation
            && self.navigation_generation == submission.navigation
            && self.declaration.spec().as_ref() == Ok(&submission.spec)
            && self.owner_ready
            && self.owner_identity.as_ref().is_some_and(|identity| {
                identity.database_instance() == submission.identity.database_instance()
            })
    }

    fn join_creation(&mut self) -> Result<(), String> {
        let Some(pending) = self.declaration_pending.as_ref() else {
            return Ok(());
        };
        let Some(revision) = pending.committed.as_ref() else {
            return Ok(());
        };
        if !self
            .snapshot_revision
            .as_ref()
            .is_some_and(|snapshot| snapshot.value() >= revision.value())
            || !self.owner_ready
            || !self.owner_identity.as_ref().is_some_and(|identity| {
                identity.database_instance() == pending.submission.identity.database_instance()
            })
        {
            return Ok(());
        }
        let Some(index) = self
            .projects
            .iter()
            .position(|project| project.project_id == pending.submission.project_id)
        else {
            return Ok(());
        };
        if self.projects[index].spec != pending.submission.spec {
            return Err(
                "Created project identity has different membership; intent retained".into(),
            );
        }
        let select = self.creation_matches(&pending.submission);
        if select {
            self.declaration.advance()?;
            self.declaration = Declaration {
                generation: self.declaration.generation,
                ..Declaration::default()
            };
            self.selected = Some(index);
            self.task = 0;
            self.config = None;
            self.plan = None;
            self.unassigned = false;
            self.edit_requested = None;
            self.focus = Focus::Conversation;
            self.scroll = 0;
            self.invalidate_evidence()?;
        }
        self.declaration_pending = None;
        self.notice("Declared project created; first brief is a separate Ctrl-B action");
        Ok(())
    }

    pub fn selected_project(&self) -> Option<&StoredProject> {
        self.selected.and_then(|index| self.projects.get(index))
    }

    pub fn project_tasks(&self) -> impl Iterator<Item = &ExecutionContext> {
        self.tasks.iter().filter(|task| {
            self.selected_project()
                .is_some_and(|p| p.project_id == task.project_id)
        })
    }

    pub fn selected_task(&self) -> Option<&ExecutionContext> {
        self.project_tasks().nth(self.task)
    }

    pub fn is_creating_project(&self) -> bool {
        self.creating_project || self.declaration.open
    }

    pub fn take_command(&mut self) -> Option<Command> {
        self.command.take()
    }

    pub fn bind_project_request(&mut self, request_id: RequestId) {
        assert!(self.pending_creation.is_none());
        assert!(self.command.is_none());
        let generation = self
            .submitted_generation
            .take()
            .expect("project dispatch requires submitted generation");
        self.pending_creation = Some((request_id, generation));
    }

    fn advance_generation(&mut self) -> Result<(), String> {
        self.draft_generation = self
            .draft_generation
            .checked_add(1)
            .ok_or("Draft generation exhausted")?;
        Ok(())
    }

    pub fn has_command(&self) -> bool {
        self.command.is_some()
    }

    pub fn refresh_command(&self) -> Command {
        if let Some(task) = self.selected_task() {
            if self.focus == Focus::Settings && self.config.is_none() {
                return Command::TaskConfig {
                    task_id: task.task_id.clone(),
                };
            }
            if matches!(self.focus, Focus::Plan | Focus::Supervision) && self.plan.is_none() {
                return Command::TaskPlan {
                    task_id: task.task_id.clone(),
                };
            }
        }
        Command::Snapshot
    }

    pub fn notice(&mut self, text: &str) {
        self.status = super::views::safe_text(text, DRAFT_BYTES_MAX);
    }

    fn assert_consistent(&self) {
        self.declaration.assert_consistent();
        assert!(self.navigation_generation > 0);
        assert!(self.drafts.len() <= 128);
        for draft in &self.drafts {
            assert!(draft.message.len() <= DRAFT_BYTES_MAX && draft.brief.len() <= DRAFT_BYTES_MAX);
            assert!(
                !draft
                    .message
                    .chars()
                    .chain(draft.brief.chars())
                    .any(char::is_control)
            );
            assert!(draft.message_generation > 0 && draft.brief_generation > 0);
            assert!(!draft.editing_brief || draft.expected_version.is_some());
        }
        assert!(self.draft.len() <= DRAFT_BYTES_MAX);
        assert!(!self.draft.chars().any(char::is_control));
        assert!(self.project_draft.len() <= DRAFT_BYTES_MAX);
        assert!(!self.project_draft.chars().any(char::is_control));
        assert!(
            self.selected
                .is_none_or(|index| index < self.projects.len())
        );
        assert!(FOCI.contains(&self.focus));
    }
}

#[cfg(test)]
mod conversation_invariants {
    use super::*;

    fn project_draft() -> ProjectDraft {
        ProjectDraft {
            database: "internal-invariant-input".into(),
            project: ProjectId::new("internal-invariant-project").unwrap(),
            message: String::new(),
            message_generation: 1,
            brief: String::new(),
            brief_generation: 1,
            expected_version: None,
            editing_brief: false,
        }
    }

    #[test]
    fn actual_project_draft_cache_invariants_trip_on_corruption() {
        for corruption in 0..7 {
            let mut app = App::new();
            app.drafts.push(project_draft());
            app.assert_consistent();
            match corruption {
                0 => app.drafts.extend((0..128).map(|_| project_draft())),
                1 => app.drafts[0].message = "x".repeat(DRAFT_BYTES_MAX + 1),
                2 => app.drafts[0].brief = "x".repeat(DRAFT_BYTES_MAX + 1),
                3 => app.drafts[0].message.push('\u{1b}'),
                4 => app.drafts[0].message_generation = 0,
                5 => app.drafts[0].brief_generation = 0,
                6 => app.drafts[0].editing_brief = true,
                _ => unreachable!(),
            }
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| app.assert_consistent()))
                    .is_err(),
                "actual cache corruption {corruption} did not trip its invariant"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic]
    fn corrupt_selection_trips_internal_invariant() {
        let mut app = App::new();
        app.selected = Some(1);
        app.assert_consistent();
    }
}
