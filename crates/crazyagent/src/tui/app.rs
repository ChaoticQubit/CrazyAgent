use super::client::{Evidence, OwnerEvent, OwnerProgress};
use crate::config::EffectiveConfig;
use crate::coordination::PlanState;
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
    Submit,
    Cancel,
    Scroll(i16),
}

pub struct App {
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
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
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
        }
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    pub fn draft(&self) -> &str {
        if self.creating_project {
            &self.project_draft
        } else {
            &self.draft
        }
    }

    fn active_draft(&mut self) -> &mut String {
        if self.creating_project {
            &mut self.project_draft
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
                if !character.is_control()
                    && self.draft().len() + character.len_utf8() <= DRAFT_BYTES_MAX
                {
                    self.active_draft().push(character);
                    if self.creating_project {
                        self.advance_generation()?;
                    }
                }
            }
            UiEvent::Backspace => {
                if self.active_draft().pop().is_some() && self.creating_project {
                    self.advance_generation()?;
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
            UiEvent::Cancel => {
                self.advance_generation()?;
                self.active_draft().clear();
                self.creating_project = false;
                self.command = None;
                self.submitted_generation = None;
                self.notice("Draft cleared");
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
                } else {
                    self.notice("Leader conversation unavailable: authenticated owner API is required; draft retained");
                }
            }
            UiEvent::Scroll(delta) => self.scroll = self.scroll.saturating_add_signed(delta),
        }
        if previous_project
            != self
                .selected_project()
                .map(|project| project.project_id.clone())
        {
            self.invalidate_evidence()?;
        }
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
                let selected = self.selected_project().map(|p| p.project_id.clone());
                let task = self.selected_task().cloned();
                self.projects = projects;
                self.tasks = tasks;
                self.selected = selected
                    .and_then(|id| self.projects.iter().position(|p| p.project_id == id))
                    .or_else(|| (!self.projects.is_empty()).then_some(0));
                self.task = task
                    .as_ref()
                    .and_then(|old| {
                        self.project_tasks()
                            .position(|new| old.task_id == new.task_id)
                    })
                    .unwrap_or(0);
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
        Ok(())
    }

    pub(super) fn reconnect_owner(&mut self) -> Result<(), String> {
        self.invalidate_evidence()?;
        self.owner_ready = false;
        self.owner_problem = None;
        self.owner_notice = "Reconnecting to the original configured engine endpoint".into();
        Ok(())
    }

    pub(super) fn owner_continuation(&self) -> bool {
        self.owner_step == Some(false)
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
            OwnerEvent::Access { ready, problem } => {
                self.invalidate_evidence()?;
                self.owner_ready = ready;
                self.owner_problem = (!ready).then_some(problem);
                if !ready {
                    self.owner_notice =
                        "Evidence and approvals are disabled; Ctrl-R reconnects".into();
                }
            }
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
        self.creating_project
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
