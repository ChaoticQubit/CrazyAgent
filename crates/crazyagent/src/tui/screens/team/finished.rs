use crate::engine::owner::OwnerResult;
use crate::project_setup::{SetupQuery, SetupView};
use crate::protocol::ProjectId;
use crate::team::{TeamAction, TeamStatus};
use crate::tui::client::OwnerEvent;
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Message, hint};
use crate::tui::keys::Key;
use crate::tui::screens::Flow;
use crate::tui::screens::new_project::harness_name;
use crate::tui::seam::{Arrival, Request};
use crate::tui::text::{cut, styled, wrap};
use crate::tui::theme::Role;
use ratatui::text::Line;

/// One agent of the Finished box: its id, plain name, harness, status words, what it last
/// worked on, whether `w` can wake it and the engine's reason when it cannot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FinishedRow {
    pub agent: i64,
    pub label: String,
    pub harness: String,
    pub status: String,
    pub task: String,
    pub wakeable: bool,
    pub note: Option<String>,
}

/// Lists the agents the engine marks finished (DEC-102), newest first, as the desktop's
/// Finished fold-out does. Input: the team status. Output: the rows.
pub(super) fn finished_rows(status: &TeamStatus) -> Vec<FinishedRow> {
    let mut rows: Vec<FinishedRow> = status
        .agents
        .iter()
        .filter(|agent| agent.finished)
        .map(|agent| {
            let last = status
                .tasks
                .iter()
                .filter(|task| task.agent == Some(agent.id))
                .max_by_key(|task| task.id);
            let task = match (&agent.problem, last) {
                (Some(problem), _) => super::plain(problem),
                (None, Some(task)) => format!("Last task: {}", super::title(&task.prompt)),
                (None, None) => "No task yet".to_owned(),
            };
            FinishedRow {
                agent: agent.id,
                label: super::plain(&agent.label),
                harness: harness_name(&agent.harness, None),
                status: agent.status_label.clone(),
                task,
                wakeable: agent.wakeable,
                note: agent.wake_note.as_deref().map(super::plain),
            }
        })
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.agent));
    assert!(rows.len() <= status.agents.len());
    rows
}

/// Counts the agents the team tree lists, as the desktop's Running and All filters do: the
/// agents the engine has not marked finished, and the finished ones too while the setting is
/// Show; Running leaves out stopped and failed ones. Inputs: each agent's finished and live
/// flags and the setting. Output: the Running and All counts.
fn tree_counts(agents: &[(bool, bool)], show: bool) -> (usize, usize) {
    let listed = agents.iter().filter(|(finished, _)| show || !finished);
    let all = listed.clone().count();
    let running = listed.filter(|(_, live)| *live).count();
    (running, all)
}

/// The box title. Input: how many agents it holds. Output: "Finished (N)".
pub(super) fn finished_label(count: usize) -> String {
    format!("Finished ({count})")
}

/// One plain sentence for a refused wake or setting change. Input: the code. Output: the
/// sentence.
fn words(code: &str) -> &'static str {
    match code {
        "team_agent_running" => "It is already running.",
        "team_agent_no_worktree" => "Its worktree was cleaned up. Hand its work to a new agent.",
        "team_agent_not_found" => "That agent is no longer part of this team.",
        "team_agents_unavailable" => {
            crate::research::runner::confined_unavailable(std::env::consts::OS)
                .unwrap_or("Team agents cannot run on this system.")
        }
        "engine_support_not_ready" => "Waking agents is not available in this engine yet.",
        "trusted_connection_unavailable" | "controller_disconnected" => {
            "The engine connection is not ready yet."
        }
        other => crate::config::plain_problem(other).unwrap_or("The engine could not do this."),
    }
}

/// The engine's refusal in a change reply, whether the connection failed or the engine
/// rejected the change. Input: the reply. Output: the code, or None once it applied.
fn refusal(result: &Result<SetupView, String>) -> Option<&str> {
    match result {
        Ok(SetupView::Changed(OwnerResult::Rejected { code })) | Err(code) => Some(code),
        Ok(_) => None,
    }
}

/// The Team screen's Finished box (DEC-102): `f` reads the team and lists its finished
/// sub-agents, newest first; the arrows pick one, `w` wakes it as a team action by its id,
/// which the team runtime serves wherever team agents run, `h` switches the project's Finished agents setting between Hide and
/// Show, and `f` or Esc closes it. Input: keys and setup replies. Output: Setup requests and
/// one message per outcome.
pub(super) struct Finished {
    open: bool,
    rows: Option<Vec<FinishedRow>>,
    show: Option<bool>,
    version: u64,
    tally: Option<Vec<(bool, bool)>>,
    cursor: usize,
    waking: Option<String>,
    saving: bool,
}

impl Finished {
    /// A closed box with nothing read.
    pub(super) fn new() -> Self {
        Self {
            open: false,
            rows: None,
            show: None,
            version: 0,
            tally: None,
            cursor: 0,
            waking: None,
            saving: false,
        }
    }

    /// Says whether the box is open, so it takes every key.
    pub(super) fn open(&self) -> bool {
        self.open
    }

    fn read(project: &ProjectId, ctx: &mut Ctx) {
        ctx.request(Request::Setup(SetupQuery::Team {
            project: project.clone(),
        }));
    }

    /// Takes a key first while the box is open or for `f`. Inputs: the key, the project and
    /// the context. Output: the flow when the box took the key, None to leave it to the tree.
    pub(super) fn key(&mut self, key: &Key, project: &ProjectId, ctx: &mut Ctx) -> Option<Flow> {
        if !self.open {
            if !matches!(key, Key::Char('f')) {
                return None;
            }
            self.open = true;
            self.rows = None;
            self.cursor = 0;
            Self::read(project, ctx);
            return Some(Flow::Handled);
        }
        let count = self.rows.as_ref().map_or(0, Vec::len);
        match key {
            Key::Up => self.cursor = self.cursor.saturating_sub(1),
            Key::Down => self.cursor = (self.cursor + 1).min(count.saturating_sub(1)),
            Key::Char('w') => self.wake(project, ctx),
            Key::Char('h') => self.switch(project, ctx),
            Key::Char('f') | Key::Esc => self.open = false,
            _ => {}
        }
        assert!(self.cursor <= count.saturating_sub(1));
        Some(Flow::Handled)
    }

    fn wake(&mut self, project: &ProjectId, ctx: &mut Ctx) {
        if self.waking.is_some() {
            return;
        }
        let Some(row) = self.rows.as_ref().and_then(|rows| rows.get(self.cursor)) else {
            return;
        };
        if !row.wakeable {
            let why = row
                .note
                .clone()
                .unwrap_or_else(|| words("team_agent_not_found").to_owned());
            ctx.say(Message::warning(
                &format!("{} cannot be woken.", row.label),
                &why,
            ));
            return;
        }
        self.waking = Some(row.label.clone());
        let generation = ctx.next_generation();
        ctx.request(Request::Setup(SetupQuery::TeamChange {
            action: TeamAction::WakeAgent {
                project_id: project.clone(),
                agent: row.agent,
            },
            generation,
        }));
    }

    fn switch(&mut self, project: &ProjectId, ctx: &mut Ctx) {
        let Some(show) = self.show.filter(|_| !self.saving) else {
            return;
        };
        self.saving = true;
        let generation = ctx.next_generation();
        ctx.request(Request::Setup(SetupQuery::TeamChange {
            action: TeamAction::FinishedAgents {
                project_id: project.clone(),
                show: !show,
                version: self.version,
            },
            generation,
        }));
    }

    /// Applies the replies to the reads and changes this box sent for the project: a team
    /// read fills the list and the setting, a wake or setting change says how it went and
    /// reads the team and graph again. Inputs: the arrival, the project and the context.
    pub(super) fn arrive(&mut self, arrival: &Arrival, project: &ProjectId, ctx: &mut Ctx) {
        let Arrival::Owner(event) = arrival else {
            return;
        };
        let OwnerEvent::Setup(event) = event.as_ref() else {
            return;
        };
        match (&event.query, &event.result) {
            (SetupQuery::Team { project: asked }, Ok(SetupView::Team(status)))
                if asked == project =>
            {
                self.show = Some(status.show_finished);
                self.version = status.settings_version;
                self.tally = Some(
                    status
                        .agents
                        .iter()
                        .map(|agent| (agent.finished, agent.status.live()))
                        .collect(),
                );
                if self.open {
                    let rows = finished_rows(status);
                    self.cursor = self.cursor.min(rows.len().saturating_sub(1));
                    self.rows = Some(rows);
                }
            }
            (SetupQuery::Team { project: asked }, Err(code))
                if asked == project && self.open && self.rows.is_none() =>
            {
                self.open = false;
                ctx.say(Message::error(
                    "Could not read finished agents.",
                    words(code),
                ));
            }
            (
                SetupQuery::TeamChange {
                    action:
                        TeamAction::FinishedAgents {
                            project_id,
                            show,
                            version,
                        },
                    ..
                },
                result,
            ) if project_id == project => {
                self.saving = false;
                match refusal(result) {
                    None => {
                        self.show = Some(*show);
                        self.version = version + 1;
                        let lead = if *show {
                            "Finished agents now stay in the team tree."
                        } else {
                            "Finished agents now wait in Finished."
                        };
                        ctx.say(Message::success(lead, "Press f to see them."));
                        ctx.request(Request::TeamGraph {
                            project: project.clone(),
                        });
                    }
                    Some(code) => {
                        ctx.say(Message::warning(
                            "Could not change where finished agents show.",
                            words(code),
                        ));
                        if code == crate::storage::versioned::STALE_CODE {
                            Self::read(project, ctx);
                        }
                    }
                }
            }
            (
                SetupQuery::TeamChange {
                    action: TeamAction::WakeAgent { project_id, .. },
                    ..
                },
                result,
            ) if project_id == project => {
                let Some(label) = self.waking.take() else {
                    return;
                };
                match refusal(result) {
                    None => {
                        ctx.say(Message::success(
                            &format!("Waking {label}."),
                            "It comes back in the team tree.",
                        ));
                        ctx.request(Request::TeamGraph {
                            project: project.clone(),
                        });
                        Self::read(project, ctx);
                    }
                    Some(code) => ctx.say(Message::warning(
                        &format!("Could not wake {label}."),
                        words(code),
                    )),
                }
            }
            _ => {}
        }
    }

    /// The keys the open box offers.
    pub(super) fn hints(&self) -> Vec<Hint> {
        if !self.open {
            return Vec::new();
        }
        let setting = match self.show {
            Some(true) => "Hide finished",
            _ => "Show finished",
        };
        vec![hint("w", "Wake"), hint("h", setting), hint("f", "Close")]
    }

    /// The box lines while it is open: the setting and the team tree's Running and All counts,
    /// then each finished agent with its harness and status and, below it, the engine's
    /// reason when it cannot wake and what it last worked on; the picked one is marked.
    /// The list starts low enough that the picked row is always drawn, with the rows above it
    /// counted in one "N more above" line and the rows past the box's height in one "and N
    /// more" line.
    /// Inputs: the line width, the box's inner height and the context. Output: the title and
    /// at most `room` lines (one when `room` is zero), or None while closed.
    pub(super) fn lines(
        &self,
        limit: usize,
        room: usize,
        ctx: &Ctx,
    ) -> Option<(String, Vec<Line<'static>>)> {
        if !self.open {
            return None;
        }
        let theme = &ctx.theme;
        let Some(rows) = &self.rows else {
            return Some((
                finished_label(0),
                vec![ctx.loading("Reading finished agents...")],
            ));
        };
        let setting = match self.show {
            Some(true) => "Finished agents: Show. They also stay in the team tree.",
            _ => "Finished agents: Hide. They leave the team tree for this list.",
        };
        let mut lines: Vec<Line<'static>> = wrap(setting, limit)
            .into_iter()
            .map(|text| styled(text, Role::Muted, theme))
            .collect();
        if let Some(agents) = &self.tally {
            let (running, all) = tree_counts(agents, self.show == Some(true));
            lines.push(styled(
                cut(
                    &format!("Team tree: Running ({running})  All ({all})"),
                    limit,
                    ctx.glyphs.ellipsis,
                ),
                Role::Muted,
                theme,
            ));
        }
        lines.push(Line::default());
        if rows.is_empty() {
            lines.push(Line::raw(cut(
                "No finished agents.",
                limit,
                ctx.glyphs.ellipsis,
            )));
        }
        let free = room.max(1);
        assert!(rows.is_empty() || self.cursor < rows.len());
        let height = |row: &FinishedRow| 2 + usize::from(row.note.is_some() && !row.wakeable);
        let top = (0..=self.cursor)
            .take_while(|_| !rows.is_empty())
            .find(|&top| {
                let drawn: usize = rows[top..=self.cursor].iter().map(height).sum();
                let below = usize::from(self.cursor + 1 < rows.len());
                lines.len() + usize::from(top > 0) + drawn + below <= free
            })
            .unwrap_or(self.cursor);
        if top > 0 {
            lines.push(Line::raw(cut(
                &format!("{top} more above"),
                limit,
                ctx.glyphs.ellipsis,
            )));
        }
        for (index, row) in rows.iter().enumerate().skip(top) {
            let left = rows.len() - index;
            let note = row.note.as_ref().filter(|_| !row.wakeable);
            let height = height(row);
            if lines.len() + height + usize::from(left > 1) > free {
                lines.push(Line::raw(cut(
                    &format!("and {left} more"),
                    limit,
                    ctx.glyphs.ellipsis,
                )));
                break;
            }
            let mark = if index == self.cursor { "> " } else { "  " };
            let waking = self
                .waking
                .as_ref()
                .filter(|_| index == self.cursor)
                .map_or("", |_| "  waking...");
            let head = format!(
                "{mark}{}  {}  {}{waking}",
                row.label, row.harness, row.status
            );
            lines.push(if index == self.cursor {
                styled(cut(&head, limit, ctx.glyphs.ellipsis), Role::Accent, theme)
            } else {
                Line::raw(cut(&head, limit, ctx.glyphs.ellipsis))
            });
            if let Some(note) = note {
                lines.push(styled(
                    cut(&format!("  {note}"), limit, ctx.glyphs.ellipsis),
                    Role::Warning,
                    theme,
                ));
            }
            lines.push(styled(
                cut(&format!("  {}", row.task), limit, ctx.glyphs.ellipsis),
                Role::Muted,
                theme,
            ));
        }
        lines.truncate(free);
        assert!(lines.len() <= room.max(1));
        Some((finished_label(rows.len()), lines))
    }
}
