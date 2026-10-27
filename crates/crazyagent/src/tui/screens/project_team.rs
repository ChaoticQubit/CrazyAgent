use super::new_project::harness_name;
use super::new_project_input::TextInput;
use super::new_project_view::{Look, Row, columns, list_row, spread, wrap_words};
use super::project::{REFRESH_MS, plain_problem};
use super::project_time::{clock, duration};
use crate::engine::owner::OwnerResult;
use crate::project_setup::{SetupQuery, SetupView};
use crate::protocol::ProjectId;
use crate::team::{
    AgentRole, AgentStatus, DelegationPath, TASK_PROMPT_BYTES_MAX, TaskSource, TaskStatus,
    TeamAction, TeamAgent, TeamHarness, TeamStatus, TeamTask,
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
    pub draft: Option<Draft>,
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
            draft: None,
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
            SetupQuery::TeamChange { action, .. } if action.project() == &self.project => {
                let done = self.changing.take();
                match result {
                    Ok(SetupView::Changed(OwnerResult::Applied)) => {
                        if matches!(action, TeamAction::Delegate { .. }) {
                            self.draft = None;
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

    /// Says whether the owner is writing a new task, so the host sends pastes
    /// here, keeps its own letter keys off and shows the cursor.
    /// Takes the view. Returns true while the task composer is open.
    pub fn typing(&self) -> bool {
        self.draft.is_some()
    }

    /// Adds pasted text to the task being written.
    /// Takes untrusted pasted text; control characters are dropped.
    /// Returns true when the composer took it.
    pub fn paste(&mut self, text: &str) -> bool {
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
        agents
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

    /// Handles one key: move, switch list, cancel a task after asking, or the
    /// keep-warm switch.
    /// Takes the key event. Returns what the host should do next.
    pub fn key(&mut self, key: KeyEvent) -> TeamReply {
        if self.draft.is_some() {
            return self.draft_key(key);
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
            KeyCode::Char('n') => {
                self.open_draft();
                TeamReply::Handled
            }
            _ => TeamReply::Ignored,
        };
        self.clamp();
        reply
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
        let mut lines = vec![self.summary(content, look)];
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
        if wide {
            lines.push(Row::default());
        }
        let agents = self.agents();
        let tasks = self.tasks();
        let (panel, caret) = match &self.draft {
            Some(draft) => {
                let (rows, at) = draft_rows(draft, content, look);
                (rows, Some(at))
            }
            None => (self.panel(content, accounts, look), None),
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
            let mut cells = vec![
                Row::of(self.agent_name(agent), look.fg),
                Row::of(harness_name(&agent.harness, None), look.fg),
            ];
            if wide {
                cells.push(Row::of(
                    account_name(agent.account.as_deref(), accounts),
                    look.fg,
                ));
            }
            cells.push(agent_status(agent, current, look));
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

    fn summary(&self, content: usize, look: &Look) -> Row {
        let Some(status) = &self.status else {
            return Row::of("Team", look.accent).pad(content);
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
        let right = Row::of("Keep warm  ", look.muted)
            .push(if status.keep_warm { "On" } else { "Off" }, look.fg);
        spread(content, left, right)
    }

    fn agent_name(&self, agent: &TeamAgent) -> String {
        if agent.role == AgentRole::Master {
            return "Master".into();
        }
        if !agent.name.trim().is_empty() {
            return agent.name.clone();
        }
        let before = self
            .status
            .iter()
            .flat_map(|status| status.agents.iter())
            .filter(|other| other.role == AgentRole::Sub && other.id < agent.id)
            .count();
        format!("Sub-agent {}", before + 1)
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
                        crate::team::names::line(
                            &self.agent_name(agent),
                            &agent.harness,
                            &agent.status_label,
                        ),
                        look.heading,
                    ),
                    Row::of(when, look.muted),
                ));
                rows.push(entry(
                    "Account",
                    account_name(agent.account.as_deref(), accounts),
                ));
                rows.push(entry("Tools", tools_word(agent.tools).into()));
                match (agent.task.and_then(|id| self.task(id)), &agent.worktree) {
                    (Some(task), _) => rows.push(entry("Task", title(&task.prompt))),
                    (None, Some(path)) => rows.push(entry("Working copy", short_path(path))),
                    (None, None) => {}
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
                    rows.push(entry("Agent", self.agent_name(agent)));
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
        if let Some(task) = self.confirm.and_then(|id| self.task(id)) {
            return vec![
                Row::of(format!("Cancel \"{}\"?", title(&task.prompt)), look.warning)
                    .push("   y", look.heading)
                    .push(" Cancel task   ", look.fg)
                    .push("n", look.heading)
                    .push(" Keep", look.fg),
            ];
        }
        self.notice.iter().map(|notice| notice.row(look)).collect()
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
        if self.confirm.is_some() {
            return vec![("y", "Cancel task"), ("n", "Keep")];
        }
        vec![
            ("\u{2191}\u{2193}", "Move"),
            ("Tab", "Agents or tasks"),
            ("n", "New task"),
            ("x", "Cancel task"),
            ("w", "Keep warm"),
            TABS,
            ("Esc", "Home"),
        ]
    }
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

fn agents_word(count: usize) -> String {
    if count == 1 {
        "1 agent".into()
    } else {
        format!("{count} agents")
    }
}

fn account_name(id: Option<&str>, accounts: &[AccountRow]) -> String {
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

fn agent_status(agent: &TeamAgent, task: Option<&TeamTask>, look: &Look) -> Row {
    let (glyph, style) = match agent.status {
        AgentStatus::Working => ("\u{25b8}", look.running),
        AgentStatus::Starting | AgentStatus::Idle => ("\u{25cc}", look.muted),
        AgentStatus::Stopped => ("\u{2298}", look.muted),
        AgentStatus::Failed => ("\u{2717}", look.error),
    };
    let row = Row::of(
        format!("{} {}", look.glyph(glyph), agent.status.label()),
        style,
    );
    match task.filter(|_| agent.status == AgentStatus::Working) {
        Some(task) => row.push(format!("  {}", title(&task.prompt)), look.fg),
        None => row,
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
