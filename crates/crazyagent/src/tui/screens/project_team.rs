use super::new_project::harness_name;
use super::new_project_input::TextInput;
use super::new_project_view::{Look, Row, columns, list_row, spread, wrap_words};
use super::project::{REFRESH_MS, plain_problem};
use super::project_time::{clock, duration};
use crate::engine::owner::OwnerResult;
use crate::project_setup::{SetupQuery, SetupView};
use crate::protocol::ProjectId;
use crate::team::events::{EventKind, HOURLY_CAP_MAX, RuleTarget, WakeRule};
use crate::team::{
    AgentRole, AgentStatus, BranchMode, ChildLanding, DelegationPath, MASTER_BRANCH_BYTES_MAX,
    SwitchMode, TASK_PROMPT_BYTES_MAX, TaskSource, TaskStatus, TeamAction, TeamAgent, TeamHarness,
    TeamStatus, TeamTask,
};
use crate::tui::seam::AccountRow;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::Style;

/// The help keys a live view gives its host, as key and label pairs.
pub(in crate::tui) type Help = Vec<(&'static str, &'static str)>;

pub(in crate::tui) const TABS: (&str, &str) = ("Shift-\u{2190}\u{2192}", "Tabs");
const WIDE: usize = 96;
const PANEL_TEXT_LINES: usize = 2;
const DRAFT_LINES: usize = 3;

/// One short line under a live view after a change, a hint or a failed read.
/// Holds the plain text and whether it reports a problem.
/// Problems draw in the error style, everything else in the plain style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct Notice {
    pub text: String,
    pub error: bool,
}

impl Notice {
    /// Makes a plain notice: something worked, or a hint.
    /// Takes the text.
    /// Returns the notice.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: false,
        }
    }

    /// Makes a problem notice.
    /// Takes the text.
    /// Returns the notice.
    pub fn problem(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: true,
        }
    }

    /// Draws the notice as one row.
    /// Takes the look.
    /// Returns the row in the error or plain style.
    pub fn row(&self, look: &Look) -> Row {
        Row::of(
            self.text.clone(),
            if self.error { look.error } else { look.fg },
        )
    }
}

/// Which list of the Team view has focus.
/// Tab moves between the agents and the tasks.
/// The panel under the lists describes the focused row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum TeamFocus {
    Agents,
    Tasks,
}

/// What the Team view asks its host to do after a key.
/// Ignored lets the host's own keys run; Change is one owner team action.
/// The host sends a change as `SetupQuery::TeamChange` with a fresh generation.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::tui) enum TeamReply {
    Ignored,
    Handled,
    Change(TeamAction),
}

/// A task the owner is writing for a sub-agent.
/// Holds the text and the chosen sub-agent harness; none lets the engine pick
/// the least busy one. Stays open until the engine takes the task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct Draft {
    pub input: TextInput,
    pub harness: Option<String>,
}

/// Which wake rule the owner edits: the app's, this project's or one sub-agent's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum WakeLevel {
    App,
    Project,
    Agent(i64),
}

/// The open wake rule editor: the level, the sub-agent it can switch to and the row with
/// the cursor. Rows 0 to 4 are the event kinds in `EventKind::ALL` order, row 5 the limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) struct WakeEdit {
    pub level: WakeLevel,
    pub agent: Option<i64>,
    pub row: usize,
}

const WAKE_ROWS: usize = EventKind::ALL.len() + 1;

/// A stop the owner pressed that waits on one more key: Stop all asks y or n, Stop harness
/// asks y or n for that agent, and Stop teammate asks which running teammate of that agent
/// by its number, 1 to 9.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum Ask {
    StopAll,
    StopAgent(i64),
    Teammate(i64),
}

const TEAMMATE_KEYS_MAX: usize = 9;

/// The live team of one project: its agents, the tasks the master or the owner
/// handed to sub-agents, cancel and the keep-warm switch.
/// Takes engine reads through `apply` and keys through `key`; never invents data.
pub(in crate::tui) struct TeamView {
    pub project: ProjectId,
    pub status: Option<TeamStatus>,
    pub focus: TeamFocus,
    pub agent_cursor: usize,
    pub task_cursor: usize,
    pub confirm: Option<i64>,
    pub ask: Option<Ask>,
    pub draft: Option<Draft>,
    pub branch_edit: Option<TextInput>,
    pub wake: Option<WakeEdit>,
    pub alerts: bool,
    pub notice: Option<Notice>,
    pub now_ms: i64,
    pub offset: i64,
    read_ms: Option<i64>,
    waiting: bool,
    changing: Option<String>,
}

impl TeamView {
    /// Opens the team of one project with nothing read yet.
    /// Takes the project.
    /// Returns the view; reads start with `next_query`.
    pub fn new(project: ProjectId) -> Self {
        Self {
            project,
            status: None,
            focus: TeamFocus::Agents,
            agent_cursor: 0,
            task_cursor: 0,
            confirm: None,
            ask: None,
            draft: None,
            branch_edit: None,
            wake: None,
            alerts: false,
            notice: None,
            now_ms: 0,
            offset: 0,
            read_ms: None,
            waiting: false,
            changing: None,
        }
    }

    /// Gives the next team read, at most one at a time and one per refresh period.
    /// Takes the current time in milliseconds.
    /// Returns `SetupQuery::Team`, or none while a read is out or nothing is due.
    pub fn next_query(&mut self, now_ms: i64) -> Option<SetupQuery> {
        self.now_ms = now_ms;
        if self.waiting || self.read_ms.is_some_and(|last| now_ms - last < REFRESH_MS) {
            return None;
        }
        self.waiting = true;
        Some(SetupQuery::Team {
            project: self.project.clone(),
        })
    }

    /// Forgets the read in flight after the engine connection dropped.
    /// Takes the view; the last team stays on screen.
    /// Returns nothing.
    pub fn forget_reads(&mut self) {
        self.waiting = false;
    }

    /// Applies one team read or team change result for this project.
    /// Takes the query that was sent and the engine's view or plain problem code.
    /// Returns nothing; other queries and other projects are left alone.
    pub fn apply(&mut self, query: &SetupQuery, result: Result<SetupView, String>) {
        match query {
            SetupQuery::Team { project } if project == &self.project => {
                self.waiting = false;
                self.read_ms = Some(self.now_ms);
                match result {
                    Ok(SetupView::Team(status)) => {
                        assert_eq!(status.project_id, self.project);
                        self.status = Some(*status);
                    }
                    Ok(_) => {}
                    Err(code) => self.notice = Some(Notice::problem(team_plain(&code))),
                }
            }
            SetupQuery::TeamChange { action, .. }
                if action
                    .project()
                    .map_or(self.changing.is_some(), |project| project == &self.project) =>
            {
                let done = self.changing.take();
                match result {
                    Ok(SetupView::Changed(OwnerResult::Applied)) => {
                        if matches!(action, TeamAction::Delegate { .. }) {
                            self.draft = None;
                        }
                        if matches!(action, TeamAction::MasterBranch { .. }) {
                            self.branch_edit = None;
                        }
                        self.notice = done.map(Notice::plain);
                        self.read_ms = None;
                    }
                    Ok(SetupView::Changed(OwnerResult::Rejected { code })) | Err(code) => {
                        self.notice = Some(Notice::problem(team_plain(&code)));
                    }
                    Ok(_) => {}
                }
            }
            _ => {}
        }
        self.clamp();
    }

    /// Says whether a team change is waiting for the engine.
    /// Takes the view.
    /// Returns true until the change result arrives.
    pub fn busy(&self) -> bool {
        self.changing.is_some()
    }

    /// Says whether the owner is writing a new task or the Master's branch, so the host
    /// sends pastes here, keeps its own letter keys off and shows the cursor.
    /// Takes the view. Returns true while the task composer or the branch editor is open.
    pub fn typing(&self) -> bool {
        self.draft.is_some() || self.branch_edit.is_some()
    }

    /// Adds pasted text to the task or the Master's branch being written.
    /// Takes untrusted pasted text; control characters are dropped.
    /// Returns true when the composer or the branch editor took it.
    pub fn paste(&mut self, text: &str) -> bool {
        if let Some(input) = &mut self.branch_edit {
            input.insert_str(text);
            return true;
        }
        match &mut self.draft {
            Some(draft) => {
                draft.input.insert_str(text);
                true
            }
            None => false,
        }
    }

    /// Lists the agents in display order: live agents first with the master on
    /// top, then stopped agents, each oldest first.
    /// Takes the view. Returns the agents, empty before the first read.
    pub fn agents(&self) -> Vec<&TeamAgent> {
        self.agent_tree()
            .into_iter()
            .map(|(agent, _)| agent)
            .collect()
    }

    /// Lists the agents as `agents` does, each with its depth under its lead (N2), so a
    /// lead's workers show right below it. Takes the view. Returns the agents and depths.
    fn agent_tree(&self) -> Vec<(&TeamAgent, usize)> {
        let mut agents: Vec<&TeamAgent> = self
            .status
            .iter()
            .flat_map(|status| status.agents.iter())
            .collect();
        agents.sort_by_key(|agent| {
            (
                !agent.status.live(),
                agent.role != AgentRole::Master,
                agent.id,
            )
        });
        crate::team::lead_tree(&agents)
    }

    /// Lists the tasks in display order: unfinished first, oldest first, then
    /// finished tasks, newest first.
    /// Takes the view. Returns the tasks, empty before the first read.
    pub fn tasks(&self) -> Vec<&TeamTask> {
        let mut tasks: Vec<&TeamTask> = self
            .status
            .iter()
            .flat_map(|status| status.tasks.iter())
            .collect();
        tasks.sort_by_key(|task| {
            let finished = task.status.finished();
            (finished, if finished { -task.id } else { task.id })
        });
        tasks
    }

    fn task(&self, id: i64) -> Option<&TeamTask> {
        self.status
            .as_ref()?
            .tasks
            .iter()
            .find(|task| task.id == id)
    }

    fn last_task(&self, agent: &TeamAgent) -> Option<&TeamTask> {
        self.status
            .as_ref()?
            .tasks
            .iter()
            .filter(|task| task.agent == Some(agent.id))
            .max_by_key(|task| task.id)
    }

    fn clamp(&mut self) {
        let agents = self.agents().len();
        let tasks = self.tasks().len();
        self.agent_cursor = self.agent_cursor.min(agents.saturating_sub(1));
        self.task_cursor = self.task_cursor.min(tasks.saturating_sub(1));
        if self
            .confirm
            .is_some_and(|id| self.task(id).is_none_or(|task| task.status.finished()))
        {
            self.confirm = None;
        }
    }

    /// Handles one key: move, switch list, cancel a task after asking, the
    /// keep-warm switch, or open the notifications and mark them seen.
    /// Takes the key event. Returns what the host should do next.
    pub fn key(&mut self, key: KeyEvent) -> TeamReply {
        if self.draft.is_some() {
            return self.draft_key(key);
        }
        if self.branch_edit.is_some() {
            return self.branch_key(key);
        }
        if self.wake.is_some() {
            return self.wake_key(key);
        }
        if self.alerts {
            return match key.code {
                KeyCode::Char('s') => self.mark_seen(),
                KeyCode::Char('a') | KeyCode::Esc => {
                    self.alerts = false;
                    TeamReply::Handled
                }
                _ => TeamReply::Ignored,
            };
        }
        if let Some(ask) = self.ask {
            return self.ask_key(ask, key);
        }
        if let Some(task) = self.confirm {
            return match key.code {
                KeyCode::Char('y') => {
                    self.confirm = None;
                    self.cancel(task)
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirm = None;
                    TeamReply::Handled
                }
                _ => TeamReply::Handled,
            };
        }
        let reply = match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    TeamFocus::Agents => TeamFocus::Tasks,
                    TeamFocus::Tasks => TeamFocus::Agents,
                };
                TeamReply::Handled
            }
            KeyCode::Up => {
                self.step(false);
                TeamReply::Handled
            }
            KeyCode::Down => {
                self.step(true);
                TeamReply::Handled
            }
            KeyCode::Char('x') => {
                self.ask_cancel();
                TeamReply::Handled
            }
            KeyCode::Char('w') => self.keep_warm(),
            KeyCode::Char('t') => self.time_limit(Limit::Task),
            KeyCode::Char('l') => self.time_limit(Limit::Session),
            KeyCode::Char('i') => self.time_limit(Limit::Idle),
            KeyCode::Char('h') => self.harness_switch(),
            KeyCode::Char('d') => self.sub_agent_depth(),
            KeyCode::Char('k') => self.child_landing(),
            KeyCode::Char('b') => self.agent_branches(),
            KeyCode::Char('S') => self.stop_all(),
            KeyCode::Char('z') => self.ask_stop_agent(),
            KeyCode::Char('p') => self.ask_teammate(),
            KeyCode::Char('v') => self.cap(Cap::Harnesses),
            KeyCode::Char('u') => self.cap(Cap::Teammates),
            KeyCode::Char('T') => self.token_budget(),
            KeyCode::Char('m') => {
                self.open_branch_edit();
                TeamReply::Handled
            }
            KeyCode::Char('g') => self.ask_before_merge(),
            KeyCode::Char('n') => {
                self.open_draft();
                TeamReply::Handled
            }
            KeyCode::Char('e') => {
                self.open_wake();
                TeamReply::Handled
            }
            KeyCode::Char('s') => self.mark_seen(),
            KeyCode::Char('a') => {
                self.alerts = true;
                TeamReply::Handled
            }
            _ => TeamReply::Ignored,
        };
        self.clamp();
        reply
    }

    /// Answers the stop that waits on a key: y sends Stop all or Stop harness, a number
    /// stops that running teammate, and n or Esc keeps everything running. Takes the stop
    /// and the key. Returns the change to send, or Handled.
    fn ask_key(&mut self, ask: Ask, key: KeyEvent) -> TeamReply {
        let reply = match (ask, key.code) {
            (_, KeyCode::Char('n') | KeyCode::Esc) => TeamReply::Handled,
            (Ask::StopAll, KeyCode::Char('y')) => self.change(
                "Stopping every agent of the project. Press S to start them again.",
                TeamAction::Pause {
                    project_id: self.project.clone(),
                },
            ),
            (Ask::StopAgent(agent), KeyCode::Char('y')) => self.change(
                "Stopping that harness and its teammates. Wake it from Finished to start it again.",
                TeamAction::StopAgent {
                    project_id: self.project.clone(),
                    agent,
                },
            ),
            (Ask::Teammate(agent), KeyCode::Char(digit @ '1'..='9')) => {
                let at = usize::from(digit as u8 - b'1');
                match self.running_teammates(agent).get(at) {
                    Some(pane) => {
                        let pane = pane.clone();
                        self.change(
                            "Stopping that teammate. Its harness is told.",
                            TeamAction::StopTeammate {
                                project_id: self.project.clone(),
                                agent,
                                pane,
                            },
                        )
                    }
                    None => return TeamReply::Handled,
                }
            }
            _ => return TeamReply::Handled,
        };
        self.ask = None;
        reply
    }

    /// Sends one team change with the line shown once it applied, unless the last change is
    /// still saving. Takes the line and the change. Returns the change, or Handled.
    fn change(&mut self, done: &str, action: TeamAction) -> TeamReply {
        if self.changing.is_some() {
            self.notice = Some(Notice::problem(
                "Still saving the last change. Try again in a moment.",
            ));
            return TeamReply::Handled;
        }
        self.changing = Some(done.to_owned());
        TeamReply::Change(action)
    }

    /// The pane ids of one harness's running teammates, oldest first, at most
    /// `TEAMMATE_KEYS_MAX`, numbered 1 up in the panel. Takes the agent. Returns the ids.
    fn running_teammates(&self, agent: i64) -> Vec<String> {
        self.status
            .iter()
            .flat_map(|status| status.agents.iter())
            .filter(|item| item.id == agent)
            .flat_map(|item| item.teammates.iter())
            .filter(|teammate| !teammate.ended && !teammate.stopped)
            .map(|teammate| teammate.pane.clone())
            .take(TEAMMATE_KEYS_MAX)
            .collect()
    }

    /// Stop all: asks before stopping every agent of the project, and starts them again
    /// at once when the team is stopped. Returns the change, or Handled with a question or
    /// a notice.
    fn stop_all(&mut self) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        if status.paused {
            return self.change(
                "Starting the team again.",
                TeamAction::Resume {
                    project_id: self.project.clone(),
                },
            );
        }
        self.ask = Some(Ask::StopAll);
        TeamReply::Handled
    }

    /// Stop harness: asks before stopping the selected sub-agent with its teammates. The
    /// master stops only with Stop all. Returns Handled with the question or a notice.
    fn ask_stop_agent(&mut self) -> TeamReply {
        let Some(agent) = self.agents().get(self.agent_cursor).copied() else {
            self.notice = Some(Notice::plain("Pick a harness in the agents list first."));
            return TeamReply::Handled;
        };
        let note = match (agent.role, agent.status.live(), agent.owner_stopped) {
            (AgentRole::Master, ..) => Some("The master stops with Stop all (S)."),
            (_, false, _) | (_, _, true) => {
                Some("That harness is already stopped. Wake it from Finished to start it again.")
            }
            _ => None,
        };
        match note {
            Some(note) => self.notice = Some(Notice::plain(note)),
            None => self.ask = Some(Ask::StopAgent(agent.id)),
        }
        TeamReply::Handled
    }

    /// Stop teammate: asks which running teammate of the selected harness to stop.
    /// Returns Handled with the question or a notice.
    fn ask_teammate(&mut self) -> TeamReply {
        let Some(agent) = self.agents().get(self.agent_cursor).map(|agent| agent.id) else {
            self.notice = Some(Notice::plain("Pick a harness in the agents list first."));
            return TeamReply::Handled;
        };
        if self.running_teammates(agent).is_empty() {
            self.notice = Some(Notice::plain("That harness has no running teammates."));
        } else {
            self.ask = Some(Ask::Teammate(agent));
        }
        TeamReply::Handled
    }

    /// Moves Max harnesses per project or Max teammates per harness to its next choice:
    /// No limit, then each of `CAP_CHOICES`, then No limit again. Takes which cap. Returns
    /// the change, or Handled while the team loads or saves.
    fn cap(&mut self, which: Cap) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let current = match which {
            Cap::Harnesses => status.max_harnesses,
            Cap::Teammates => status.max_teammates,
        };
        let max = next_choice(&crate::team::CAP_CHOICES, current);
        let project_id = self.project.clone();
        let (done, action) = match which {
            Cap::Harnesses => (
                format!("Max harnesses is now {}.", cap_words(max)),
                TeamAction::MaxHarnesses { project_id, max },
            ),
            Cap::Teammates => (
                format!("Max teammates per harness is now {}.", cap_words(max)),
                TeamAction::MaxTeammates { project_id, max },
            ),
        };
        self.change(&done, action)
    }

    /// Moves the Token budget to its next choice: Off, then each of `BUDGET_CHOICES`, then
    /// Off again. Returns the change, or Handled while the team loads or saves.
    fn token_budget(&mut self) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let tokens = next_choice(&crate::team::BUDGET_CHOICES, status.token_budget);
        let done = match tokens {
            Some(tokens) => format!(
                "The team now stops after {} from now.",
                crate::team::token_words(tokens)
            ),
            None => "The token budget is off.".to_owned(),
        };
        self.change(
            &done,
            TeamAction::TokenBudget {
                project_id: self.project.clone(),
                tokens,
            },
        )
    }

    fn step(&mut self, down: bool) {
        let cursor = match self.focus {
            TeamFocus::Agents => &mut self.agent_cursor,
            TeamFocus::Tasks => &mut self.task_cursor,
        };
        *cursor = if down {
            cursor.saturating_add(1)
        } else {
            cursor.saturating_sub(1)
        };
    }

    fn ask_cancel(&mut self) {
        let target = match self.focus {
            TeamFocus::Tasks => self.tasks().get(self.task_cursor).map(|task| task.id),
            TeamFocus::Agents => self
                .agents()
                .get(self.agent_cursor)
                .and_then(|agent| agent.task),
        };
        let open = target
            .and_then(|id| self.task(id))
            .filter(|task| !task.status.finished())
            .map(|task| task.id);
        match open {
            Some(id) => self.confirm = Some(id),
            None => self.notice = Some(Notice::plain("There is no running or waiting task here.")),
        }
    }

    fn cancel(&mut self, id: i64) -> TeamReply {
        if self.changing.is_some() {
            self.notice = Some(Notice::problem(
                "Still saving the last change. Try again in a moment.",
            ));
            return TeamReply::Handled;
        }
        let Some(task) = self.task(id) else {
            return TeamReply::Handled;
        };
        let done = if task.status == TaskStatus::Running {
            format!("Stopping \"{}\".", title(&task.prompt))
        } else {
            format!("Cancelled \"{}\".", title(&task.prompt))
        };
        self.changing = Some(done);
        TeamReply::Change(TeamAction::Cancel {
            project_id: self.project.clone(),
            task: Some(id),
        })
    }

    fn sub_harnesses(&self) -> Vec<String> {
        self.status
            .iter()
            .flat_map(|status| status.harnesses.iter())
            .filter(|harness| harness.role == AgentRole::Sub && harness.runnable)
            .map(|harness| harness.harness.clone())
            .collect()
    }

    fn open_draft(&mut self) {
        if self.status.is_none() {
            self.notice = Some(Notice::plain("The team is still loading."));
            return;
        }
        if self.sub_harnesses().is_empty() {
            self.notice = Some(Notice::plain(
                "This team has no sub-agent that can run yet.",
            ));
            return;
        }
        self.notice = None;
        self.draft = Some(Draft {
            input: TextInput::multi(TASK_PROMPT_BYTES_MAX),
            harness: None,
        });
    }

    fn draft_key(&mut self, key: KeyEvent) -> TeamReply {
        match key.code {
            KeyCode::Esc => {
                self.draft = None;
                TeamReply::Handled
            }
            KeyCode::Enter => self.send_draft(),
            KeyCode::Tab | KeyCode::BackTab => {
                self.pick_harness(key.code == KeyCode::Tab);
                TeamReply::Handled
            }
            _ => {
                if let Some(draft) = &mut self.draft {
                    draft.input.handle(key);
                }
                TeamReply::Handled
            }
        }
    }

    fn pick_harness(&mut self, forward: bool) {
        let choices: Vec<Option<String>> = std::iter::once(None)
            .chain(self.sub_harnesses().into_iter().map(Some))
            .collect();
        let Some(draft) = &mut self.draft else {
            return;
        };
        let at = choices
            .iter()
            .position(|choice| choice == &draft.harness)
            .unwrap_or(0);
        let next = if forward {
            (at + 1) % choices.len()
        } else {
            (at + choices.len() - 1) % choices.len()
        };
        draft.harness = choices[next].clone();
    }

    fn send_draft(&mut self) -> TeamReply {
        let Some(draft) = &self.draft else {
            return TeamReply::Ignored;
        };
        let prompt = draft.input.value().trim().to_owned();
        let harness = draft.harness.clone();
        if prompt.is_empty() {
            self.notice = Some(Notice::plain("Write what the sub-agent should do first."));
            return TeamReply::Handled;
        }
        if self.changing.is_some() {
            self.notice = Some(Notice::problem(
                "Still saving the last change. Try again in a moment.",
            ));
            return TeamReply::Handled;
        }
        self.notice = None;
        self.changing = Some(format!(
            "Handed \"{}\" to {}.",
            title(&prompt),
            harness
                .as_deref()
                .map_or_else(|| "a sub-agent".to_owned(), |name| harness_name(name, None))
        ));
        TeamReply::Change(TeamAction::Delegate {
            project_id: self.project.clone(),
            harness,
            prompt,
        })
    }

    fn open_wake(&mut self) {
        if self.status.is_none() {
            self.notice = Some(Notice::plain("The team is still loading."));
            return;
        }
        let agent = match self.focus {
            TeamFocus::Agents => self
                .agents()
                .get(self.agent_cursor)
                .filter(|agent| agent.role == AgentRole::Sub)
                .map(|agent| agent.id),
            TeamFocus::Tasks => None,
        };
        self.notice = None;
        self.wake = Some(WakeEdit {
            level: agent.map_or(WakeLevel::Project, WakeLevel::Agent),
            agent,
            row: 0,
        });
    }

    /// Gives the rule a level sets itself, if any, and the rule shown for it: its own, else
    /// the one it falls back to. Takes the level. Returns none before the first read.
    pub fn wake_rule(&self, level: WakeLevel) -> Option<(Option<WakeRule>, WakeRule)> {
        let wake = &self.status.as_ref()?.events.wake;
        let (own, fallback) = match level {
            WakeLevel::App => (wake.app, WakeRule::default()),
            WakeLevel::Project => (wake.project, wake.app.unwrap_or_default()),
            WakeLevel::Agent(id) => (
                wake.agents
                    .iter()
                    .find(|rule| rule.agent == id)
                    .map(|rule| rule.rule),
                wake.effective,
            ),
        };
        Some((own, own.unwrap_or(fallback)))
    }

    fn wake_key(&mut self, key: KeyEvent) -> TeamReply {
        let Some(edit) = self.wake else {
            return TeamReply::Ignored;
        };
        let Some((own, shown)) = self.wake_rule(edit.level) else {
            self.wake = None;
            return TeamReply::Handled;
        };
        let mut next = shown;
        match key.code {
            KeyCode::Esc => {
                self.wake = None;
                return TeamReply::Handled;
            }
            KeyCode::Up | KeyCode::Down => {
                let row = if key.code == KeyCode::Down {
                    (edit.row + 1).min(WAKE_ROWS - 1)
                } else {
                    edit.row.saturating_sub(1)
                };
                self.wake = Some(WakeEdit { row, ..edit });
                return TeamReply::Handled;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let levels: Vec<WakeLevel> = [WakeLevel::App, WakeLevel::Project]
                    .into_iter()
                    .chain(edit.agent.map(WakeLevel::Agent))
                    .collect();
                let at = levels
                    .iter()
                    .position(|level| *level == edit.level)
                    .unwrap_or(0);
                let next = if key.code == KeyCode::Tab {
                    (at + 1) % levels.len()
                } else {
                    (at + levels.len() - 1) % levels.len()
                };
                self.wake = Some(WakeEdit {
                    level: levels[next],
                    ..edit
                });
                return TeamReply::Handled;
            }
            KeyCode::Char(' ') | KeyCode::Enter if edit.row < EventKind::ALL.len() => {
                let kind = EventKind::ALL[edit.row];
                let on = !shown.wakes(kind);
                match kind {
                    EventKind::Question => next.question = on,
                    EventKind::Finished => next.finished = on,
                    EventKind::Failed => next.failed = on,
                    EventKind::ChangeSetReady => next.change_set_ready = on,
                    EventKind::Progress => next.progress = on,
                }
            }
            KeyCode::Left | KeyCode::Right | KeyCode::PageDown | KeyCode::PageUp
                if edit.row == EventKind::ALL.len() =>
            {
                next.hourly_cap = match key.code {
                    KeyCode::Left => shown.hourly_cap.saturating_sub(1),
                    KeyCode::Right => shown.hourly_cap.saturating_add(1),
                    KeyCode::PageDown => shown.hourly_cap.saturating_sub(10),
                    _ => shown.hourly_cap.saturating_add(10),
                }
                .min(HOURLY_CAP_MAX);
                if next.hourly_cap == shown.hourly_cap {
                    return TeamReply::Handled;
                }
            }
            KeyCode::Char('r') => {
                if own.is_none() {
                    self.notice = Some(Notice::plain("This level already uses the rule above it."));
                    return TeamReply::Handled;
                }
                return self.wake_change(edit.level, None);
            }
            _ => return TeamReply::Handled,
        }
        self.wake_change(edit.level, Some(next))
    }

    fn wake_change(&mut self, level: WakeLevel, rule: Option<WakeRule>) -> TeamReply {
        if self.changing.is_some() {
            self.notice = Some(Notice::problem(
                "Still saving the last change. Try again in a moment.",
            ));
            return TeamReply::Handled;
        }
        let target = match level {
            WakeLevel::App => RuleTarget::App,
            WakeLevel::Project => RuleTarget::Project {
                project_id: self.project.clone(),
            },
            WakeLevel::Agent(agent) => RuleTarget::Agent {
                project_id: self.project.clone(),
                agent,
            },
        };
        self.changing = Some(match rule {
            Some(_) => "Wake rule saved.".to_owned(),
            None => "Wake rule reset. The rule above it applies.".to_owned(),
        });
        TeamReply::Change(TeamAction::Wake { target, rule })
    }

    fn mark_seen(&mut self) -> TeamReply {
        let Some(upto) = self.status.as_ref().and_then(|status| {
            status
                .events
                .notifications
                .iter()
                .filter(|item| item.seen_ms.is_none())
                .map(|item| item.id)
                .max()
        }) else {
            self.notice = Some(Notice::plain("There are no new notifications."));
            return TeamReply::Handled;
        };
        if self.changing.is_some() {
            self.notice = Some(Notice::problem(
                "Still saving the last change. Try again in a moment.",
            ));
            return TeamReply::Handled;
        }
        self.changing = Some("Notifications marked as seen.".to_owned());
        TeamReply::Change(TeamAction::Seen {
            project_id: self.project.clone(),
            upto,
        })
    }

    fn alert_rows(&self, content: usize, room: usize, look: &Look) -> Vec<Row> {
        let mut notifications: Vec<_> = self
            .status
            .iter()
            .flat_map(|status| status.events.notifications.iter())
            .collect();
        notifications.sort_by_key(|item| std::cmp::Reverse(item.id));
        let unseen = notifications
            .iter()
            .filter(|item| item.seen_ms.is_none())
            .count();
        let mut rows = vec![
            Row::default(),
            Row::of("Notifications", look.heading).push(format!("  {unseen} new"), look.muted),
        ];
        if notifications.is_empty() {
            rows.push(Row::of(
                "  No notifications yet. Sub-agents that need you show here.",
                look.muted,
            ));
            return rows;
        }
        let fits = room.saturating_sub(rows.len());
        let shown = if notifications.len() > fits {
            fits.saturating_sub(1)
        } else {
            notifications.len()
        };
        for item in &notifications[..shown] {
            let (word, style) = match item.seen_ms {
                None => ("New", look.warning),
                Some(_) => ("Seen", look.muted),
            };
            rows.push(spread(
                content,
                Row::of(format!("  {word}"), style)
                    .pad(8)
                    .push(title(&item.text), look.fg),
                Row::of(
                    format!("  {} ago", duration(self.now_ms - item.created_ms)),
                    look.muted,
                ),
            ));
        }
        let hidden = notifications.len() - shown;
        if hidden > 0 {
            rows.push(Row::of(format!("  {hidden} older not shown"), look.muted));
        }
        rows
    }

    fn wake_rows(&self, edit: WakeEdit, content: usize, look: &Look) -> Vec<Row> {
        let Some((own, shown)) = self.wake_rule(edit.level) else {
            return Vec::new();
        };
        let whose = match edit.level {
            WakeLevel::App => "every project".to_owned(),
            WakeLevel::Project => "this project".to_owned(),
            WakeLevel::Agent(id) => self
                .agents()
                .into_iter()
                .find(|agent| agent.id == id)
                .map_or_else(|| "this sub-agent".to_owned(), |agent| agent.label.clone()),
        };
        let source = match (own.is_some(), edit.level) {
            (true, _) => "Set here",
            (false, WakeLevel::App) => "Uses the default",
            (false, WakeLevel::Project) => "Uses the app rule",
            (false, WakeLevel::Agent(_)) => "Uses the project rule",
        };
        let mut rows = vec![spread(
            content,
            Row::of(format!("Wake the master for {whose}"), look.heading),
            Row::of(source, look.muted),
        )];
        for (row, kind) in EventKind::ALL.into_iter().enumerate() {
            let mark = if shown.wakes(kind) { "[x] " } else { "[ ] " };
            rows.push(list_row(
                Row::of(mark, look.fg).push(wake_kind_label(kind), look.fg),
                content,
                edit.row == row,
                true,
                look,
            ));
        }
        rows.push(list_row(
            Row::of("Wakes an hour before pausing  ", look.fg)
                .push(shown.hourly_cap.to_string(), look.heading)
                .push("  (0 never wakes)", look.muted),
            content,
            edit.row == EventKind::ALL.len(),
            true,
            look,
        ));
        rows
    }

    fn keep_warm(&mut self) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        if self.changing.is_some() {
            self.notice = Some(Notice::problem(
                "Still saving the last change. Try again in a moment.",
            ));
            return TeamReply::Handled;
        }
        let on = !status.keep_warm;
        let done = match (on, status.min_agents) {
            (false, _) => "Agents now stop when they run out of work.".to_owned(),
            (true, 0) => {
                "Keep warm is on, but this team keeps no agents ready. Raise the minimum in Settings."
                    .to_owned()
            }
            (true, count) => format!(
                "Keeping {} ready between tasks.",
                agents_word(usize::from(count))
            ),
        };
        self.changing = Some(done);
        TeamReply::Change(TeamAction::KeepWarm {
            project_id: self.project.clone(),
            on,
        })
    }

    /// Moves one time limit to its next choice. The task and session limits go Off, then
    /// 30 min up to 8 h, then Off again; when both are set, the other follows so a session is
    /// never shorter than a task, and Off on either bounds nothing. The idle stop goes Off,
    /// 2 min, 10 min, 1 h. Takes which limit. Returns the
    /// change, or Handled while the team loads or saves.
    fn time_limit(&mut self, which: Limit) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        if self.changing.is_some() {
            self.notice = Some(Notice::problem(
                "Still saving the last change. Try again in a moment.",
            ));
            return TeamReply::Handled;
        }
        let below = |a: Option<u32>, b: Option<u32>| matches!((a, b), (Some(a), Some(b)) if a < b);
        let limits = &crate::team::LIMIT_CHOICES_SECS;
        let (turn, kept, idle) = (
            status.turn_limit_secs,
            status.session_limit_secs,
            status.idle_stop_secs,
        );
        let (turn, kept, idle) = match which {
            Limit::Task => {
                let next = next_limit(limits, turn);
                (next, if below(kept, next) { next } else { kept }, idle)
            }
            Limit::Session => {
                let next = next_limit(limits, kept);
                (if below(next, turn) { next } else { turn }, next, idle)
            }
            Limit::Idle => (
                turn,
                kept,
                next_limit(&crate::team::IDLE_CHOICES_SECS, idle),
            ),
        };
        self.changing = Some(match which {
            Limit::Idle => format!(
                "{} An idle master stops {} after its work is done.",
                idle.map_or_else(
                    || "Idle sub-agents now keep running until the team stops.".to_owned(),
                    |secs| {
                        format!(
                            "An idle sub-agent now stops after {}.",
                            crate::team::span_words(secs.into())
                        )
                    },
                ),
                crate::team::span_words(crate::team::MASTER_IDLE_STOP_SECS.into())
            ),
            Limit::Task | Limit::Session => format!(
                "Each task may now run {}, each agent session {}.",
                limit_words(turn),
                limit_words(kept)
            ),
        });
        TeamReply::Change(TeamAction::TimeLimits {
            project_id: self.project.clone(),
            turn_secs: turn,
            session_secs: kept,
            idle_secs: idle,
        })
    }

    /// Flips the On harness switch setting between Carry over and Start fresh.
    /// Output: the change to send, or Handled with a notice while loading or saving.
    fn harness_switch(&mut self) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        if self.changing.is_some() {
            self.notice = Some(Notice::problem(
                "Still saving the last change. Try again in a moment.",
            ));
            return TeamReply::Handled;
        }
        let mode = match status.on_harness_switch {
            SwitchMode::Carry => SwitchMode::Fresh,
            SwitchMode::Fresh => SwitchMode::Carry,
        };
        self.changing = Some(
            match mode {
                SwitchMode::Carry => {
                    "When the master harness changes, the new harness gets the earlier conversation."
                }
                SwitchMode::Fresh => {
                    "When the master harness changes, the new harness starts fresh."
                }
            }
            .to_owned(),
        );
        TeamReply::Change(TeamAction::OnHarnessSwitch {
            project_id: self.project.clone(),
            mode,
        })
    }

    /// Moves the Sub-agent depth setting to its next choice, 1 after 3.
    /// Output: the change to send, or Handled with a notice while loading or saving.
    fn sub_agent_depth(&mut self) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        if self.changing.is_some() {
            self.notice = Some(Notice::problem(
                "Still saving the last change. Try again in a moment.",
            ));
            return TeamReply::Handled;
        }
        let choices = crate::team::DEPTH_CHOICES;
        let depth = choices
            .iter()
            .position(|choice| *choice == status.sub_agent_depth)
            .map_or(choices[0], |at| choices[(at + 1) % choices.len()]);
        self.changing = Some(depth_note(depth).to_owned());
        TeamReply::Change(TeamAction::SubAgentDepth {
            project_id: self.project.clone(),
            depth,
        })
    }

    /// Flips where a lead's worker lands its change set between the lead's Accept and the
    /// owner's own. Output: the change to send, or Handled with a notice while loading or
    /// saving.
    fn child_landing(&mut self) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        if self.changing.is_some() {
            self.notice = Some(Notice::problem(
                "Still saving the last change. Try again in a moment.",
            ));
            return TeamReply::Handled;
        }
        let mode = match status.child_landing {
            ChildLanding::Lead => ChildLanding::Owner,
            ChildLanding::Owner => ChildLanding::Lead,
        };
        self.changing = Some(
            match mode {
                ChildLanding::Lead => {
                    "A lead's worker now lands on the lead's branch when the lead accepts it."
                }
                ChildLanding::Owner => "A lead's worker now lands only when you accept it.",
            }
            .to_owned(),
        );
        TeamReply::Change(TeamAction::ChildLanding {
            project_id: self.project.clone(),
            mode,
        })
    }

    /// Flips the Agent branches setting between Named branch and Detached.
    /// Output: the change to send, or Handled with a notice while loading or saving.
    fn agent_branches(&mut self) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        if self.changing.is_some() {
            self.notice = Some(Notice::problem(
                "Still saving the last change. Try again in a moment.",
            ));
            return TeamReply::Handled;
        }
        let mode = match status.agent_branches {
            BranchMode::Named => BranchMode::Detached,
            BranchMode::Detached => BranchMode::Named,
        };
        self.changing = Some(
            match mode {
                BranchMode::Named => "Each new agent now works on its own branch.",
                BranchMode::Detached => "New agents now work on a detached HEAD.",
            }
            .to_owned(),
        );
        TeamReply::Change(TeamAction::AgentBranches {
            project_id: self.project.clone(),
            mode,
        })
    }

    /// Opens the Master's branch editor filled with the current choice, empty for the
    /// repository default. Output: none; a notice while the team is loading.
    fn open_branch_edit(&mut self) {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return;
        };
        let mut input = TextInput::single(MASTER_BRANCH_BYTES_MAX);
        input.set(status.master_branch.as_deref().unwrap_or(""));
        self.branch_edit = Some(input);
    }

    /// Handles one key while the Master's branch editor is open: Enter saves, Esc closes,
    /// other keys edit. Takes the key. Output: the change to send, or Handled.
    fn branch_key(&mut self, key: KeyEvent) -> TeamReply {
        match key.code {
            KeyCode::Esc => {
                self.branch_edit = None;
                TeamReply::Handled
            }
            KeyCode::Enter => self.save_master_branch(),
            _ => {
                if let Some(input) = &mut self.branch_edit {
                    input.handle(key);
                }
                TeamReply::Handled
            }
        }
    }

    /// Sends the Master's branch typed in the editor; an empty name goes back to the
    /// repository default. Output: the change to send, or Handled with a notice while saving.
    fn save_master_branch(&mut self) -> TeamReply {
        let Some(input) = &self.branch_edit else {
            return TeamReply::Handled;
        };
        let typed = input.value().trim();
        let branch = (!typed.is_empty()).then(|| typed.to_owned());
        if self.changing.is_some() {
            self.notice = Some(Notice::problem(
                "Still saving the last change. Try again in a moment.",
            ));
            return TeamReply::Handled;
        }
        self.changing = Some(match &branch {
            Some(name) => format!("The master now works on {name}."),
            None => "The master now works on the repository's default branch.".to_owned(),
        });
        TeamReply::Change(TeamAction::MasterBranch {
            project_id: self.project.clone(),
            branch,
        })
    }

    /// Flips "Ask me before merging into the master's branch" between Off and On.
    /// Output: the change to send, or Handled with a notice while loading or saving.
    fn ask_before_merge(&mut self) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        if self.changing.is_some() {
            self.notice = Some(Notice::problem(
                "Still saving the last change. Try again in a moment.",
            ));
            return TeamReply::Handled;
        }
        let on = !status.ask_before_merge;
        self.changing = Some(
            if on {
                "The master now asks you before merging a harness."
            } else {
                "The master now merges a finished harness without asking."
            }
            .to_owned(),
        );
        TeamReply::Change(TeamAction::AskBeforeMerge {
            project_id: self.project.clone(),
            on,
        })
    }

    /// Draws the Team view for the body of the project's Agents tab.
    /// Takes the content width, the rows available, the accounts used to name
    /// agents' accounts and the look. Returns the body rows and the help keys.
    pub fn rows(
        &self,
        content: usize,
        room: usize,
        accounts: &[AccountRow],
        look: &Look,
    ) -> (Vec<Row>, Help) {
        let (lines, help, _) = self.draw(content, room, accounts, look);
        (lines, help)
    }

    /// Draws the Team view like `rows` and also places the typing cursor.
    /// Takes the content width, the rows available, the accounts and the look.
    /// Returns the body rows, the help keys and, while the owner writes a task,
    /// the cursor as a body row and a column inside that row.
    pub fn draw(
        &self,
        content: usize,
        room: usize,
        accounts: &[AccountRow],
        look: &Look,
    ) -> (Vec<Row>, Help, Option<(usize, usize)>) {
        let wide = content >= WIDE;
        let mut lines = self.summary(content, look);
        let Some(status) = &self.status else {
            lines.push(Row::of("Loading...", look.muted));
            lines.extend(self.notice.iter().map(|notice| notice.row(look)));
            return (lines, self.help(), None);
        };
        lines.push(Row::of(plan_line(status), look.muted));
        if let Some(label) = status
            .harnesses
            .iter()
            .find(|harness| {
                harness.role == AgentRole::Master
                    && harness.delegation == Some(DelegationPath::Unavailable)
            })
            .and_then(|harness| harness.delegation_label.clone())
        {
            lines.push(Row::of(label, look.muted));
        }
        if self.alerts {
            let notice: Vec<Row> = self.notice.iter().map(|notice| notice.row(look)).collect();
            let room = room.saturating_sub(lines.len() + notice.len());
            lines.extend(self.alert_rows(content, room, look));
            lines.extend(notice);
            return (lines, self.help(), None);
        }
        if wide {
            lines.push(Row::default());
        }
        let tree = self.agent_tree();
        let agents: Vec<&TeamAgent> = tree.iter().map(|(agent, _)| *agent).collect();
        let tasks = self.tasks();
        let (panel, caret) = match (&self.draft, &self.branch_edit, self.wake) {
            (Some(draft), _, _) => {
                let (rows, at) = draft_rows(draft, content, look);
                (rows, Some(at))
            }
            (None, Some(input), _) => {
                let (rows, at) = branch_rows(input, content, look);
                (rows, Some(at))
            }
            (None, None, Some(edit)) => (self.wake_rows(edit, content, look), None),
            (None, None, None) => (self.panel(content, accounts, look), None),
        };
        let footer = self.footer(look);
        let fixed = lines.len() + 5 + panel.len() + footer.len();
        let free = room.saturating_sub(fixed).max(2);
        let wanted_tasks = tasks.len().max(1).min(free / 2);
        let agent_rows = agents
            .len()
            .max(1)
            .min(free.saturating_sub(wanted_tasks))
            .max(1);
        let task_rows = free.saturating_sub(agent_rows).max(1);

        let agent_widths = if wide {
            vec![16, 13, 22, content.saturating_sub(2 + 16 + 13 + 22 + 6)]
        } else {
            vec![16, 13, content.saturating_sub(2 + 16 + 13 + 4)]
        };
        let agent_heads: &[&str] = if wide {
            &["Agent", "Harness", "Account", "Status"]
        } else {
            &["Agent", "Harness", "Status"]
        };
        lines.push(heads(&agent_widths, agent_heads, look));
        if agents.is_empty() {
            lines.push(Row::of(
                "  No agents are running. They start when there is work.",
                look.muted,
            ));
        }
        let first = window(self.agent_cursor, agents.len(), agent_rows);
        for (index, agent) in agents.iter().enumerate().skip(first).take(agent_rows) {
            let current = agent.task.and_then(|id| self.task(id));
            let last = current.is_none().then(|| self.last_task(agent)).flatten();
            let depth = tree[index].1;
            let shown = if depth == 0 {
                agent.label.clone()
            } else {
                format!("{}\u{21b3} {}", "  ".repeat(depth - 1), agent.label)
            };
            let mut cells = vec![
                Row::of(shown, look.fg),
                Row::of(harness_name(&agent.harness, None), look.fg),
            ];
            if wide {
                cells.push(Row::of(
                    account_name(agent.account.as_deref(), accounts),
                    look.fg,
                ));
            }
            cells.push(agent_status(agent, current, last, look));
            lines.push(list_row(
                columns(&agent_widths, cells),
                content,
                index == self.agent_cursor,
                self.focus == TeamFocus::Agents,
                look,
            ));
        }
        lines.push(Row::default());

        lines.push(Row::of("Tasks", look.heading).push(format!("  {}", tasks.len()), look.muted));
        let task_widths = if wide {
            vec![36, 13, 8, content.saturating_sub(2 + 36 + 13 + 8 + 6)]
        } else {
            vec![30, 13, content.saturating_sub(2 + 30 + 13 + 4)]
        };
        let task_heads: &[&str] = if wide {
            &["Task", "Harness", "From", "Status"]
        } else {
            &["Task", "Harness", "Status"]
        };
        if tasks.is_empty() {
            lines.push(Row::of(
                "  No tasks yet. The master hands work to sub-agents here.",
                look.muted,
            ));
        } else {
            lines.push(heads(&task_widths, task_heads, look));
        }
        let first = window(self.task_cursor, tasks.len(), task_rows);
        for (index, task) in tasks.iter().enumerate().skip(first).take(task_rows) {
            let mut cells = vec![
                Row::of(title(&task.prompt), look.fg),
                Row::of(harness_name(&task.harness, None), look.fg),
            ];
            if wide {
                cells.push(Row::of(source_word(task.source), look.fg));
            }
            cells.push(task_status(task, self.now_ms, self.offset, look));
            lines.push(list_row(
                columns(&task_widths, cells),
                content,
                index == self.task_cursor,
                self.focus == TeamFocus::Tasks,
                look,
            ));
        }
        lines.push(Row::default());
        let cursor = caret.map(|(row, column)| (lines.len() + row, column));
        lines.extend(panel);
        lines.extend(footer);
        (lines, self.help(), cursor)
    }

    /// The Team rows: the agent counts, a stopped team and alerts, then every team setting
    /// in order (Keep warm, Task, Session, Idle stop, On harness switch, Sub-agent depth,
    /// Worker lands, Agent branches, Max harnesses, Max teammates, Token budget), each whole. A
    /// setting that does not fit on the row starts the next one, right-aligned under the
    /// first. Counts wider
    /// than the content are cut with an ellipsis like any row. Inputs: the content width and
    /// the look. Output: one row, or more when the settings wrap.
    fn summary(&self, content: usize, look: &Look) -> Vec<Row> {
        let Some(status) = &self.status else {
            return vec![Row::of("Team", look.accent).pad(content)];
        };
        let count = |wanted: &[AgentStatus]| {
            status
                .agents
                .iter()
                .filter(|agent| wanted.contains(&agent.status))
                .count()
        };
        let mut left = Row::of("Team", look.accent);
        for (number, word, style) in [
            (count(&[AgentStatus::Working]), "working", look.running),
            (
                count(&[AgentStatus::Starting, AgentStatus::Idle]),
                "waiting",
                look.muted,
            ),
            (
                count(&[AgentStatus::Failed]),
                "stopped with a problem",
                look.error,
            ),
        ] {
            if number > 0 {
                left = left
                    .push("  ", Style::new())
                    .push(format!("{number} {word}"), style);
            }
        }
        let asking = status.events.waiting.len();
        if asking > 0 {
            left = left
                .push("  ", Style::new())
                .push(format!("{asking} waiting for an answer"), look.warning);
        }
        if status.paused {
            left = left.push("  ", Style::new()).push(
                if status.budget_reached {
                    "stopped: token budget used"
                } else {
                    "stopped by you"
                },
                look.error,
            );
        }
        if status.events.paused {
            left = left
                .push("  ", Style::new())
                .push("auto-wake paused", look.warning);
        }
        if status.partial {
            left = left
                .push("  ", Style::new())
                .push("older history hidden", look.muted);
        }
        let setting = |label: &str, value: String| {
            Row::of(format!("{label}  "), look.muted).push(value, look.fg)
        };
        let settings = [
            setting(
                "Keep warm",
                if status.keep_warm { "On" } else { "Off" }.to_owned(),
            ),
            setting("Task", limit_label(status.turn_limit_secs)),
            setting("Session", limit_label(status.session_limit_secs)),
            setting("Idle stop", limit_label(status.idle_stop_secs)),
            setting(
                "On harness switch",
                switch_words(status.on_harness_switch).to_owned(),
            ),
            setting("Sub-agent depth", status.sub_agent_depth.to_string()),
            setting(
                "Worker lands",
                landing_words(status.child_landing).to_owned(),
            ),
            setting(
                "Agent branches",
                branch_words(status.agent_branches).to_owned(),
            ),
            setting("Max harnesses", cap_words(status.max_harnesses)),
            setting("Max teammates", cap_words(status.max_teammates)),
            setting(
                "Token budget",
                budget_words(status.token_budget, status.budget_used),
            ),
            setting(
                "Master's branch",
                status
                    .master_branch
                    .clone()
                    .unwrap_or_else(|| "Repository default".to_owned()),
            ),
            setting(
                "Ask before merge",
                if status.ask_before_merge { "On" } else { "Off" }.to_owned(),
            ),
        ];
        let mut rows = Vec::new();
        let mut right = Row::default();
        for item in settings {
            let used = match (left.width(), right.width()) {
                (0, 0) => 0,
                (0, taken) | (taken, 0) => taken + 2,
                (lead, taken) => lead + 2 + taken + 2,
            };
            if used > 0 && used + item.width() > content {
                rows.push(spread(content, left, right));
                left = Row::default();
                right = Row::default();
            }
            right = if right.width() == 0 {
                item
            } else {
                right.push("  ", look.muted).cat(item)
            };
        }
        rows.push(spread(content, left, right));
        rows
    }

    fn panel(&self, content: usize, accounts: &[AccountRow], look: &Look) -> Vec<Row> {
        let entry =
            |label: &str, value: String| Row::of(label, look.muted).pad(14).push(value, look.fg);
        let mut rows = Vec::new();
        match self.focus {
            TeamFocus::Agents => {
                let Some(agent) = self.agents().get(self.agent_cursor).copied() else {
                    return rows;
                };
                let when = match agent.stopped_ms {
                    Some(at) => format!("Stopped {}", clock(at, self.offset)),
                    None => format!(
                        "Started {}, up {}",
                        clock(agent.created_ms, self.offset),
                        duration(self.now_ms - agent.created_ms)
                    ),
                };
                rows.push(spread(
                    content,
                    Row::of(
                        crate::team::names::line(&agent.label, &agent.harness, &agent.status_label),
                        look.heading,
                    ),
                    Row::of(when, look.muted),
                ));
                rows.push(entry(
                    "Account",
                    account_name(agent.account.as_deref(), accounts),
                ));
                rows.push(entry("Tools", tools_word(agent.tools).into()));
                if agent.owner_stopped {
                    rows.push(Row::of(
                        "Stopped by you. Wake it from Finished to start it again.",
                        look.warning,
                    ));
                }
                let running = self.running_teammates(agent.id);
                for teammate in &agent.teammates {
                    let state = match (teammate.ended, teammate.stopped) {
                        (_, true) => "stopped".to_owned(),
                        (true, false) => "finished".to_owned(),
                        (false, false) => running
                            .iter()
                            .position(|pane| pane == &teammate.pane)
                            .map_or_else(
                                || "running".to_owned(),
                                |at| format!("running, p then {}", at + 1),
                            ),
                    };
                    rows.push(entry("Teammate", format!("{}  {state}", teammate.pane)));
                }
                if let Some(waiting) = self
                    .status
                    .iter()
                    .flat_map(|status| status.events.waiting.iter())
                    .find(|item| item.agent == agent.id)
                {
                    rows.extend(
                        wrap_words(&format!("Asks: {}", waiting.text), content)
                            .into_iter()
                            .take(PANEL_TEXT_LINES)
                            .map(|line| Row::of(line, look.warning)),
                    );
                }
                match agent.task.and_then(|id| self.task(id)) {
                    Some(task) => rows.push(entry("Task", title(&task.prompt))),
                    None => {
                        if let Some(last) = self.last_task(agent) {
                            rows.push(entry("Last task", title(&last.prompt)));
                        }
                        if let Some(path) = &agent.worktree {
                            rows.push(entry("Working copy", short_path(path)));
                        }
                    }
                }
                if let Some(problem) = &agent.problem {
                    rows.extend(
                        wrap_words(problem, content)
                            .into_iter()
                            .take(PANEL_TEXT_LINES)
                            .map(|line| Row::of(line, look.error)),
                    );
                }
            }
            TeamFocus::Tasks => {
                let Some(task) = self.tasks().get(self.task_cursor).copied() else {
                    return rows;
                };
                let when = match (task.started_ms, task.finished_ms) {
                    (_, Some(at)) => format!("Finished {}", clock(at, self.offset)),
                    (Some(at), None) => format!(
                        "Started {}, running {}",
                        clock(at, self.offset),
                        duration(self.now_ms - at)
                    ),
                    (None, None) => format!("Added {}", clock(task.created_ms, self.offset)),
                };
                let heading = match task.source {
                    TaskSource::Master => "Task from the master",
                    TaskSource::Owner => "Task from you",
                };
                rows.push(spread(
                    content,
                    Row::of(heading, look.heading),
                    Row::of(when, look.muted),
                ));
                rows.extend(
                    wrap_words(task.prompt.trim(), content)
                        .into_iter()
                        .take(PANEL_TEXT_LINES)
                        .map(|line| Row::of(line, look.fg)),
                );
                if let Some(agent) = task
                    .agent
                    .and_then(|id| self.agents().into_iter().find(|agent| agent.id == id))
                {
                    rows.push(entry("Agent", agent.label.clone()));
                }
                if task.attempts > 1 {
                    rows.push(entry("Tries", task.attempts.to_string()));
                }
                match (&task.result, &task.problem) {
                    (Some(result), _) if task.status == TaskStatus::Done => {
                        rows.push(entry("Result", title(result)));
                    }
                    (_, Some(problem)) => rows.extend(
                        wrap_words(problem, content)
                            .into_iter()
                            .take(PANEL_TEXT_LINES)
                            .map(|line| Row::of(line, look.error)),
                    ),
                    _ => {}
                }
            }
        }
        rows
    }

    fn footer(&self, look: &Look) -> Vec<Row> {
        if let Some(ask) = self.ask {
            let label = |id: i64| {
                self.agents()
                    .into_iter()
                    .find(|agent| agent.id == id)
                    .map_or_else(|| "that harness".to_owned(), |agent| agent.label.clone())
            };
            let question = match ask {
                Ask::StopAll => {
                    "Stop the master, every harness and every teammate of this project?".to_owned()
                }
                Ask::StopAgent(id) => format!("Stop {} and its teammates?", label(id)),
                Ask::Teammate(id) => {
                    let count = self.running_teammates(id).len();
                    return vec![
                        Row::of(
                            format!("Stop which teammate of {}?", label(id)),
                            look.warning,
                        )
                        .push(format!("   1-{count}"), look.heading)
                        .push(" Teammate   ", look.fg)
                        .push("Esc", look.heading)
                        .push(" Keep them", look.fg),
                    ];
                }
            };
            return vec![
                Row::of(question, look.warning)
                    .push("   y", look.heading)
                    .push(" Stop   ", look.fg)
                    .push("n", look.heading)
                    .push(" Keep running", look.fg),
            ];
        }
        if let Some(task) = self.confirm.and_then(|id| self.task(id)) {
            return vec![
                Row::of(format!("Cancel \"{}\"?", title(&task.prompt)), look.warning)
                    .push("   y", look.heading)
                    .push(" Cancel task   ", look.fg)
                    .push("n", look.heading)
                    .push(" Keep", look.fg),
            ];
        }
        if let Some(notice) = &self.notice {
            return vec![notice.row(look)];
        }
        self.status
            .iter()
            .flat_map(|status| status.events.notifications.iter())
            .find(|item| item.seen_ms.is_none())
            .map(|item| Row::of(title(&item.text), look.warning))
            .into_iter()
            .collect()
    }

    fn help(&self) -> Help {
        if self.draft.is_some() {
            return vec![
                ("Enter", "Hand over"),
                ("Ctrl-J", "New line"),
                ("Tab", "Sub-agent"),
                ("Esc", "Close"),
            ];
        }
        if let Some(ask) = self.ask {
            return match ask {
                Ask::Teammate(_) => vec![("1-9", "Stop teammate"), ("Esc", "Keep")],
                Ask::StopAll | Ask::StopAgent(_) => vec![("y", "Stop"), ("n", "Keep running")],
            };
        }
        if self.branch_edit.is_some() {
            return vec![("Enter", "Save"), ("Esc", "Close")];
        }
        if self.confirm.is_some() {
            return vec![("y", "Cancel task"), ("n", "Keep")];
        }
        if self.wake.is_some() {
            return vec![
                ("\u{2191}\u{2193}", "Move"),
                ("Space", "Switch"),
                ("\u{2190}\u{2192}", "Limit"),
                ("Tab", "Level"),
                ("r", "Reset"),
                ("Esc", "Close"),
            ];
        }
        let unseen = self.status.as_ref().is_some_and(|status| {
            status
                .events
                .notifications
                .iter()
                .any(|item| item.seen_ms.is_none())
        });
        if self.alerts {
            let mut help = Vec::new();
            if unseen {
                help.push(("s", "Seen"));
            }
            help.push(("Esc", "Close"));
            return help;
        }
        let mut help = vec![
            ("\u{2191}\u{2193}", "Move"),
            ("Tab", "Agents or tasks"),
            ("n", "New task"),
            ("x", "Cancel task"),
            ("w", "Keep warm"),
            ("t", "Task limit"),
            ("l", "Session limit"),
            ("i", "Idle stop"),
            ("h", "On harness switch"),
            ("d", "Sub-agent depth"),
            ("k", "Worker lands"),
            ("b", "Agent branches"),
            ("v", "Max harnesses"),
            ("u", "Max teammates"),
            ("T", "Token budget"),
            ("z", "Stop harness"),
            ("p", "Stop teammate"),
            (
                "S",
                if self.status.as_ref().is_some_and(|status| status.paused) {
                    "Start all"
                } else {
                    "Stop all"
                },
            ),
            ("m", "Master's branch"),
            ("g", "Ask before merge"),
            ("e", "Wake rules"),
        ];
        if self
            .status
            .as_ref()
            .is_some_and(|status| !status.events.notifications.is_empty())
        {
            help.push(("a", "Notifications"));
        }
        if unseen {
            help.push(("s", "Seen"));
        }
        help.extend([TABS, ("Esc", "Home")]);
        help
    }
}

/// Which cap on running agents a key moves.
#[derive(Clone, Copy)]
enum Cap {
    Harnesses,
    Teammates,
}

/// The choice after `current`: none, then each of `choices`, then none again; a stored value
/// outside the choices goes to none. Inputs: the choices, ascending, and the value now.
/// Output: the next.
fn next_choice<T: Copy + PartialEq>(choices: &[T], current: Option<T>) -> Option<T> {
    match current {
        None => choices.first().copied(),
        Some(value) => choices
            .iter()
            .position(|choice| *choice == value)
            .and_then(|at| choices.get(at + 1))
            .copied(),
    }
}

/// Words a cap on running agents. Takes the cap, None for No limit. Returns the words.
fn cap_words(cap: Option<u8>) -> String {
    cap.map_or_else(|| "No limit".to_owned(), |cap| cap.to_string())
}

/// Words the Token budget and what the agents used of it. Takes the budget, None for Off,
/// and the tokens used. Returns the words, such as `12.5M tokens of 50M tokens`.
fn budget_words(budget: Option<u64>, used: Option<u64>) -> String {
    match budget {
        None => "Off".to_owned(),
        Some(budget) => format!(
            "{} of {}",
            crate::team::token_words(used.unwrap_or(0)),
            crate::team::token_words(budget)
        ),
    }
}

/// Which team time limit a key moves.
#[derive(Clone, Copy)]
enum Limit {
    Task,
    Session,
    Idle,
}

/// The time limit choice after `current`: Off, then each of `choices`, then Off again; a
/// stored value outside the choices goes to Off. Inputs: the choices in seconds, ascending,
/// and the limit now. Output: the next.
fn next_limit(choices: &[u32], current: Option<u32>) -> Option<u32> {
    match current {
        None => Some(choices[0]),
        Some(secs) => choices
            .iter()
            .position(|choice| *choice == secs)
            .and_then(|at| choices.get(at + 1).copied()),
    }
}

/// A time limit as Team settings shows it, "Off" for none. Input: the limit in seconds.
fn limit_label(secs: Option<u32>) -> String {
    secs.map_or_else(
        || "Off".to_owned(),
        |secs| crate::team::span_words(secs.into()),
    )
}

/// A time limit in a sentence: "as long as it needs" for none, else "up to 1 h".
fn limit_words(secs: Option<u32>) -> String {
    secs.map_or_else(
        || "as long as it needs".to_owned(),
        |secs| format!("up to {}", crate::team::span_words(secs.into())),
    )
}

/// Turns a team refusal code into the sentence shown to the owner.
/// Takes the engine's rejection code.
/// Returns a plain sentence; codes the team does not name use the project wording.
pub(in crate::tui) fn team_plain(code: &str) -> String {
    match code {
        "team_task_missing" => "That task is gone already.".into(),
        "team_engine_not_ready" => "The engine is still starting. Try again in a moment.".into(),
        "team_state_not_canonical" | "team_state_not_private" => {
            "The engine cannot read the team safely right now.".into()
        }
        "team_account_left_pool" => "That account left the pool. Another one takes over.".into(),
        "team_harness_cannot_run" => "This harness cannot run yet.".into(),
        "team_has_no_sub_agents" => "This team has no sub-agents to hand work to.".into(),
        "team_harness_not_in_plan" => "That harness is no longer part of this team.".into(),
        "team_harness_invalid" => "That harness name is not valid.".into(),
        "team_task_invalid" => "The task is empty or too long.".into(),
        "team_queue_full" => {
            "Too much work is waiting already. Try again when some finishes.".into()
        }
        "team_task_limit" => "This project has reached its task limit.".into(),
        "team_wake_cap_invalid" => "Set the hourly limit between 0 and 500 wakes.".into(),
        "team_time_limits_invalid" => {
            "Pick a listed time limit; an agent session may not be shorter than a task.".into()
        }
        "team_agent_not_found" => "That agent is no longer part of this team.".into(),
        "team_notification_missing" => "There is no such notification to mark as seen.".into(),
        "team_stop_master" => "The master stops with Stop all.".into(),
        "team_teammate_not_found" => "That teammate has already ended.".into(),
        "team_cap_invalid" => "Pick a listed limit, or No limit.".into(),
        "team_budget_invalid" => "Pick a listed token budget, or Off.".into(),
        "team_master_branch_invalid" => {
            "Type a branch name such as main or release/2.0, or leave it empty for the repository default.".into()
        }
        other => plain_problem(other),
    }
}

/// Gives the first index of a list window that keeps the cursor visible.
/// Takes the cursor, the list length and the window size.
/// Returns the index of the first row to draw.
pub(in crate::tui) fn window(cursor: usize, length: usize, size: usize) -> usize {
    let first = (cursor + 1).saturating_sub(size);
    let first = first.min(length.saturating_sub(size));
    assert!(length == 0 || first < length);
    first
}

/// Gives a task's title: the first line of what it was asked to do.
/// Takes the task prompt or result text.
/// Returns the trimmed first non-empty line, or "Untitled task".
pub(in crate::tui) fn title(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map_or_else(|| "Untitled task".to_owned(), str::to_owned)
}

fn draft_rows(draft: &Draft, content: usize, look: &Look) -> (Vec<Row>, (usize, usize)) {
    let who = draft.harness.as_deref().map_or_else(
        || "Any sub-agent".to_owned(),
        |name| harness_name(name, None),
    );
    let mut rows = vec![spread(
        content,
        Row::of("New task from you", look.heading),
        Row::of("For  ", look.muted).push(format!("< {who} >"), look.accent),
    )];
    let inner = content.saturating_sub(4).max(2);
    let (lines, (line, column)) = draft.input.wrap(inner - 1);
    let first = (line + 1).saturating_sub(DRAFT_LINES);
    let cursor = (rows.len() + line - first, 2 + column);
    let empty = draft.input.value().is_empty();
    for text in lines.iter().skip(first).take(DRAFT_LINES) {
        let text = if empty {
            Row::of("What should the sub-agent do?", look.muted)
        } else {
            Row::of(text.clone(), look.fg)
        };
        rows.push(
            Row::of("[", look.accent)
                .push(" ", Style::new())
                .cat(text.pad(inner))
                .push(" ", Style::new())
                .push("]", look.accent),
        );
    }
    assert!(cursor.0 < rows.len());
    (rows, cursor)
}

/// Draws the Master's branch editor. Takes the input, the content width and the look.
/// Returns the rows and the cursor as a row and a column inside those rows.
fn branch_rows(input: &TextInput, content: usize, look: &Look) -> (Vec<Row>, (usize, usize)) {
    let mut rows = vec![
        Row::of("Master's branch", look.heading),
        Row::of(
            "The master works there and merges finished harnesses into it. Leave it empty for the repository default.",
            look.muted,
        ),
    ];
    let inner = content.saturating_sub(4).max(2);
    let (lines, (line, column)) = input.wrap(inner - 1);
    let text = if input.value().is_empty() {
        Row::of("Repository default", look.muted)
    } else {
        Row::of(lines.get(line).cloned().unwrap_or_default(), look.fg)
    };
    let cursor = (rows.len(), 2 + column);
    rows.push(
        Row::of("[", look.accent)
            .push(" ", Style::new())
            .cat(text.pad(inner))
            .push(" ", Style::new())
            .push("]", look.accent),
    );
    assert!(cursor.0 < rows.len());
    (rows, cursor)
}

fn heads(widths: &[usize], names: &[&str], look: &Look) -> Row {
    Row::of("  ", Style::new()).cat(
        columns(
            widths,
            names
                .iter()
                .map(|name| Row::of(*name, look.muted))
                .collect(),
        )
        .restyle(look.muted),
    )
}

fn plan_line(status: &TeamStatus) -> String {
    let name = |harness: &TeamHarness| {
        let plain = harness_name(&harness.harness, None);
        if harness.runnable {
            plain
        } else {
            format!("{plain} (can't run yet)")
        }
    };
    let mut line = match status
        .harnesses
        .iter()
        .find(|harness| harness.role == AgentRole::Master)
    {
        Some(master) => format!("{} leads.", name(master)),
        None => "No master yet.".to_owned(),
    };
    let subs: Vec<String> = status
        .harnesses
        .iter()
        .filter(|harness| harness.role == AgentRole::Sub)
        .map(|harness| format!("{} x{}", name(harness), harness.agents_max))
        .collect();
    if !subs.is_empty() {
        line.push_str(&format!(" {}.", subs.join(", ")));
    }
    line.push_str(&format!(
        " Up to {} at once.",
        agents_word(usize::from(status.max_agents))
    ));
    line
}

fn wake_kind_label(kind: EventKind) -> &'static str {
    match kind {
        EventKind::Question => "A sub-agent asks a question",
        EventKind::Finished => "A sub-agent finishes its task",
        EventKind::Failed => "A sub-agent fails",
        EventKind::ChangeSetReady => "A change set is ready",
        EventKind::Progress => "A sub-agent reports progress",
    }
}

fn agents_word(count: usize) -> String {
    if count == 1 {
        "1 agent".into()
    } else {
        format!("{count} agents")
    }
}

/// Names an agent's account as the owner sees it in Accounts, never by its id.
/// Takes the account id, if the agent has one, and the owner's account rows.
/// Returns the account name, "A pool account" for an id not listed, or "-" for none.
pub(in crate::tui) fn account_name(id: Option<&str>, accounts: &[AccountRow]) -> String {
    match id {
        None => "-".into(),
        Some(id) => accounts
            .iter()
            .find(|account| account.id.as_str() == id)
            .map_or_else(
                || "A pool account".to_owned(),
                |account| account.name.clone(),
            ),
    }
}

fn tools_word(on: bool) -> &'static str {
    if on {
        "On, inside the project folder"
    } else {
        "Off"
    }
}

/// The short words for the On harness switch setting on the team summary row.
/// Takes the mode. Returns its choice name.
/// What the owner reads after picking a Sub-agent depth. Input: the depth, 1 to 3.
/// Output: the sentence.
fn depth_note(depth: u8) -> &'static str {
    match depth {
        1 => "Only the master starts sub-agents now.",
        2 => "The master's sub-agents can now lead workers of their own.",
        _ => "Workers can now lead workers of their own, three levels deep.",
    }
}

fn landing_words(mode: ChildLanding) -> &'static str {
    match mode {
        ChildLanding::Lead => "When the lead accepts",
        ChildLanding::Owner => "Only when I accept",
    }
}

fn switch_words(mode: SwitchMode) -> &'static str {
    match mode {
        SwitchMode::Carry => "Carry over",
        SwitchMode::Fresh => "Start fresh",
    }
}

/// The short words for the Agent branches setting on the team summary row.
/// Takes the mode. Returns its choice name.
fn branch_words(mode: BranchMode) -> &'static str {
    match mode {
        BranchMode::Named => "Named branch",
        BranchMode::Detached => "Detached",
    }
}

fn source_word(source: TaskSource) -> &'static str {
    match source {
        TaskSource::Master => "Master",
        TaskSource::Owner => "You",
    }
}

fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.trim_end_matches('/').rsplit('/').take(2).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

fn agent_status(
    agent: &TeamAgent,
    task: Option<&TeamTask>,
    last: Option<&TeamTask>,
    look: &Look,
) -> Row {
    let (glyph, style) = match agent.status {
        AgentStatus::Working => ("\u{25b8}", look.running),
        AgentStatus::Starting | AgentStatus::Idle => ("\u{25cc}", look.muted),
        AgentStatus::Stopped => ("\u{2298}", look.muted),
        AgentStatus::Failed => ("\u{2717}", look.error),
    };
    let label = if agent.owner_stopped {
        "Stopped by you"
    } else {
        agent.status.label()
    };
    let mut row = Row::of(format!("{} {label}", look.glyph(glyph)), style);
    let running = agent
        .teammates
        .iter()
        .filter(|teammate| !teammate.ended && !teammate.stopped)
        .count();
    if running > 0 {
        row = row.push(
            format!(
                "  +{running} {}",
                if running == 1 {
                    "teammate"
                } else {
                    "teammates"
                }
            ),
            look.muted,
        );
    }
    match (task.filter(|_| agent.status == AgentStatus::Working), last) {
        (Some(task), _) => row.push(format!("  {}", title(&task.prompt)), look.fg),
        (None, Some(last)) => row.push(format!("  Last task: {}", title(&last.prompt)), look.muted),
        (None, None) => row,
    }
}

fn task_status(task: &TeamTask, now_ms: i64, offset: i64, look: &Look) -> Row {
    match task.status {
        TaskStatus::Running => Row::of(format!("{} Running", look.glyph("\u{25b8}")), look.running)
            .push(
                task.started_ms
                    .map(|at| format!("  {}", duration(now_ms - at)))
                    .unwrap_or_default(),
                look.muted,
            ),
        TaskStatus::Queued => Row::of(
            format!("{} Waiting to start", look.glyph("\u{25cc}")),
            look.muted,
        ),
        TaskStatus::NeedsPool => Row::of("! Needs a pool", look.warning),
        TaskStatus::Done => Row::of(
            format!(
                "{} Done{}",
                look.glyph("\u{2713}"),
                task.finished_ms
                    .map(|at| format!(" {}", clock(at, offset)))
                    .unwrap_or_default()
            ),
            look.success,
        ),
        TaskStatus::Failed => Row::of(format!("{} Failed", look.glyph("\u{2717}")), look.error)
            .push(
                match task.attempts {
                    0 | 1 => String::new(),
                    tries => format!("  after {tries} tries"),
                },
                look.muted,
            ),
        TaskStatus::Cancelled => {
            Row::of(format!("{} Cancelled", look.glyph("\u{2298}")), look.muted)
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
