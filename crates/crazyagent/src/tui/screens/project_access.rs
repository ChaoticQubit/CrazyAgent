use super::new_project_view::{Look, Row, wrap_words};
use super::project_team::window;
use crate::sandbox::policy::{AgentAccess, AgentNetwork, AgentPolicy, PolicyView, ToolchainChoice};
use crossterm::event::{KeyCode, KeyEvent};

/// The plain warning the owner confirms before giving a project's agents Full access.
pub(in crate::tui) const FULL_ACCESS_WARNING: &str = "Agents run with your user's permissions: they can read your keys and change any file. Processes an agent starts outside its terminal can keep running after it stops.";

/// What a lower saved level does to agents already running: the engine stops each one that
/// started above it, but not what a Full access agent started outside its terminal.
pub(in crate::tui) const LOWERING_STOPS_AGENTS: &str = "Lowering the level stops agents running above it; they restart at the new level. It does not reach processes a Full access agent started outside its terminal.";

/// What saving a new level, hooks or network choice does to the project's open IDE, the same
/// in the desktop app.
pub(in crate::tui) const POLICY_RESTARTS_IDE: &str = "Changing the level, the IDE hooks switch below Full access, or the network choice at Own branch restarts this project's IDE at once. Its terminals end, and it reopens within a minute.";

/// What turning a toolchain off does to work already running: the engine stops each agent,
/// research job and master turn whose sandbox granted it.
pub(in crate::tui) const TOOLCHAIN_OFF_STOPS_AGENTS: &str = "Turning a toolchain off stops agents, research jobs and master turns running with it; their next start goes without it.";

/// The IDE hooks switch's name, the same in Desktop.
pub(in crate::tui) const IDE_GIT_HOOKS: &str = "IDE git runs the project's hooks";

/// What the IDE hooks switch does and never does (INT-1, IN1-1), the same in Desktop.
pub(in crate::tui) const IDE_GIT_HOOKS_LINE: &str = "Off: git in the IDE skips this project's hooks. Agent folders never run theirs, and commits from the IDE are never signed. Applies when the IDE next starts.";

/// Why the network choice has no effect under Full access: Full access runs agents with no
/// sandbox, so nothing bounds their network.
pub(in crate::tui) const NETWORK_IGNORED_UNDER_FULL: &str =
    "Full access runs agents without a sandbox, so this choice does not apply.";

const TEXT_WIDTH: usize = 56;

/// The Agent access box over the Settings tab (DEC-101, DEC-102): the project's access level,
/// one switch per toolchain family, the network choice and the IDE hooks switch. Row 0 is the
/// level, each next row one toolchain of the view, then the network row, and the last row the
/// IDE hooks switch. Busy while the engine saves; a refusal keeps the choices with the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct AccessEdit {
    pub view: PolicyView,
    pub access: AgentAccess,
    pub network: AgentNetwork,
    pub allowed: Vec<bool>,
    pub ide_git_hooks: bool,
    pub cursor: usize,
    pub confirm_full: bool,
    pub busy: bool,
    pub error: Option<String>,
}

/// What one key in the Agent access box asks the screen to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum AccessKey {
    Handled,
    Close,
    Save(AgentPolicy),
}

impl AccessEdit {
    /// Opens the box on the saved policy. Input: the engine's view. Output: the box with
    /// the cursor on the level.
    pub fn of(view: &PolicyView) -> Self {
        Self {
            access: view.access,
            network: view.network,
            allowed: view.toolchains.iter().map(|row| row.allowed).collect(),
            ide_git_hooks: view.ide_git_hooks,
            view: view.clone(),
            cursor: 0,
            confirm_full: false,
            busy: false,
            error: None,
        }
    }

    /// Handles one key: Up and Down move, Left, Right and Space change the row, Enter
    /// saves after the Full access warning is confirmed with y, Esc closes. Input: the key.
    /// Output: what the screen does next.
    pub fn key(&mut self, key: KeyEvent) -> AccessKey {
        assert_eq!(self.allowed.len(), self.view.toolchains.len());
        assert!(self.cursor <= self.hooks_row());
        if self.busy {
            return match key.code {
                KeyCode::Esc => AccessKey::Close,
                _ => AccessKey::Handled,
            };
        }
        if self.confirm_full {
            return match key.code {
                KeyCode::Char('y') => {
                    self.confirm_full = false;
                    AccessKey::Save(self.policy())
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirm_full = false;
                    AccessKey::Handled
                }
                _ => AccessKey::Handled,
            };
        }
        match key.code {
            KeyCode::Esc => return AccessKey::Close,
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down => self.cursor = (self.cursor + 1).min(self.hooks_row()),
            KeyCode::Left => self.change(false),
            KeyCode::Right | KeyCode::Char(' ') => self.change(true),
            KeyCode::Enter if self.access == AgentAccess::Full => {
                self.confirm_full = true;
            }
            KeyCode::Enter => return AccessKey::Save(self.policy()),
            _ => {}
        }
        AccessKey::Handled
    }

    /// The policy the box would save: the chosen level, a choice for each toolchain whose
    /// switch differs from the project's own use, so the others keep following the project,
    /// and the IDE hooks switch.
    pub fn policy(&self) -> AgentPolicy {
        let toolchains: Vec<ToolchainChoice> = self
            .view
            .toolchains
            .iter()
            .zip(&self.allowed)
            .filter(|(row, allowed)| row.used != **allowed)
            .map(|(row, allowed)| ToolchainChoice {
                toolchain: row.toolchain,
                allowed: *allowed,
            })
            .collect();
        let policy = AgentPolicy {
            access: self.access,
            network: self.network,
            toolchains,
            ide_git_hooks: self.ide_git_hooks,
        };
        assert!(policy.validate().is_ok());
        policy
    }

    /// The box's lines fitted to the rows the window leaves it: what the setting does, the
    /// level row, what lowering the level does to running agents, the toolchain rows, the
    /// network row with its help, the IDE hooks row with its line, the warning or the refusal,
    /// and the save line, all of them when they fit. Else the warning or refusal and the save
    /// line stay whole at the bottom, and the settings above them show a window that keeps the
    /// cursor's row and as many of its help lines as fit, with how many lines sit above and
    /// below it. Input: the look and the rows for the body. Output: at most that many rows, or
    /// the action lines, the cursor's row and the counts when even those do not fit.
    pub fn rows(&self, look: &Look, room: usize) -> Vec<Row> {
        let (form, at, end) = self.form(look);
        let actions = self.actions(look);
        let actions_len = actions.len();
        if form.len() + actions_len <= room {
            let mut rows = form;
            rows.extend(actions);
            return rows;
        }
        let keep = room.saturating_sub(actions_len).max(1);
        let mut size = keep;
        let first = loop {
            let first = window(end, form.len(), size).min(at);
            let counts = usize::from(first > 0) + usize::from(first + size < form.len());
            if size + counts <= keep || size == 1 {
                break first;
            }
            size -= 1;
        };
        assert!(first <= at && at < first + size, "the cursor's row shows");
        let below = form.len() - first - size;
        let mut rows = Vec::with_capacity(room.max(size + actions_len + 2));
        if first > 0 {
            rows.push(Row::of(format!("{first} more lines above"), look.muted));
        }
        rows.extend(form.into_iter().skip(first).take(size));
        if below > 0 {
            rows.push(Row::of(format!("{below} more lines below"), look.muted));
        }
        rows.extend(actions);
        assert!(rows.len() <= room || room < actions_len + 3);
        rows
    }

    /// The box's settings lines, through the blank line after the IDE hooks switch, and the
    /// line the cursor's row is on. Input: the look. Output: the rows, that line and the last
    /// line of the help under it.
    fn form(&self, look: &Look) -> (Vec<Row>, usize, usize) {
        let mut body: Vec<Row> = wrap_words(
            "Who decides what this project's agents may touch. Applies to the master, sub-agents and research jobs; research jobs write reports, so they run Sandboxed unless you choose Full access.",
            TEXT_WIDTH,
        )
        .into_iter()
        .map(|line| Row::of(line, look.muted))
        .collect();
        body.push(Row::default());
        let mark = |row: usize| if self.cursor == row { "> " } else { "  " };
        let level = self
            .view
            .levels
            .iter()
            .find(|level| level.access == self.access);
        let mut at = body.len();
        body.push(
            Row::of(mark(0), look.accent)
                .push("Access      ", look.muted)
                .push(
                    format!("< {} >", self.access.label()),
                    if self.cursor == 0 {
                        look.selection
                    } else {
                        look.fg
                    },
                ),
        );
        body.extend(
            wrap_words(level_help(self.access), TEXT_WIDTH)
                .into_iter()
                .map(|line| Row::of(format!("              {line}"), look.muted)),
        );
        if let Some(reason) = level.and_then(|level| level.unavailable.as_deref()) {
            body.extend(
                wrap_words(reason, TEXT_WIDTH)
                    .into_iter()
                    .map(|line| Row::of(line, look.warning)),
            );
        }
        body.extend(
            wrap_words(LOWERING_STOPS_AGENTS, TEXT_WIDTH)
                .into_iter()
                .map(|line| Row::of(line, look.muted)),
        );
        body.extend(
            wrap_words(POLICY_RESTARTS_IDE, TEXT_WIDTH)
                .into_iter()
                .map(|line| Row::of(line, look.muted)),
        );
        let mut end = body.len() - 1;
        body.push(Row::default());
        body.push(Row::of("Toolchains agents may use", look.accent));
        if self.view.toolchains.is_empty() {
            body.push(Row::of(
                "  None found in your home folder or this project.",
                look.muted,
            ));
        }
        for (index, (row, allowed)) in self.view.toolchains.iter().zip(&self.allowed).enumerate() {
            let state = if *allowed { "[x]" } else { "[ ]" };
            let note = match (row.used, row.installed) {
                (true, true) => "used by this project",
                (true, false) => "used by this project, not found in your home folder",
                (false, true) => "installed, not used by this project",
                (false, false) => "",
            };
            if self.cursor == index + 1 {
                at = body.len();
                end = at;
            }
            body.push(
                Row::of(mark(index + 1), look.accent)
                    .push(
                        format!("{state} {}", row.label),
                        if self.cursor == index + 1 {
                            look.selection
                        } else {
                            look.fg
                        },
                    )
                    .push(format!("   {note}"), look.muted),
            );
        }
        body.extend(
            wrap_words(TOOLCHAIN_OFF_STOPS_AGENTS, TEXT_WIDTH)
                .into_iter()
                .map(|line| Row::of(line, look.muted)),
        );
        body.push(Row::default());
        body.push(Row::of("Network for agents with tools on", look.accent));
        let network_row = self.allowed.len() + 1;
        if self.cursor == network_row {
            at = body.len();
        }
        body.push(
            Row::of(mark(network_row), look.accent)
                .push("Network     ", look.muted)
                .push(
                    format!("< {} >", self.network.label()),
                    if self.cursor == network_row {
                        look.selection
                    } else {
                        look.fg
                    },
                ),
        );
        body.extend(
            wrap_words(network_help(self.network), TEXT_WIDTH)
                .into_iter()
                .map(|line| Row::of(format!("              {line}"), look.muted)),
        );
        if self.access == AgentAccess::Full {
            body.extend(
                wrap_words(NETWORK_IGNORED_UNDER_FULL, TEXT_WIDTH)
                    .into_iter()
                    .map(|line| Row::of(line, look.warning)),
            );
        }
        if self.cursor == network_row {
            end = body.len() - 1;
        }
        body.push(Row::default());
        body.push(Row::of("IDE git", look.accent));
        let hooks = self.hooks_row();
        if self.cursor == hooks {
            at = body.len();
        }
        body.push(Row::of(mark(hooks), look.accent).push(
            format!(
                "{} {IDE_GIT_HOOKS}",
                if self.ide_git_hooks { "[x]" } else { "[ ]" }
            ),
            if self.cursor == hooks {
                look.selection
            } else {
                look.fg
            },
        ));
        body.extend(
            wrap_words(IDE_GIT_HOOKS_LINE, TEXT_WIDTH)
                .into_iter()
                .map(|line| Row::of(format!("  {line}"), look.muted)),
        );
        if self.cursor == hooks {
            end = body.len() - 1;
        }
        body.push(Row::default());
        assert!(at < body.len(), "the cursor's row is a settings line");
        assert!(
            at <= end && end < body.len(),
            "the cursor's help follows its row"
        );
        (body, at, end)
    }

    /// The box's closing lines: the Full access warning with its y and n, or the refusal and
    /// the save line. Input: the look. Output: the rows.
    fn actions(&self, look: &Look) -> Vec<Row> {
        if self.confirm_full {
            let mut rows: Vec<Row> =
                wrap_words(&format!("Full access: {FULL_ACCESS_WARNING}"), TEXT_WIDTH)
                    .into_iter()
                    .map(|line| Row::of(line, look.warning))
                    .collect();
            rows.push(
                Row::of("[> y Allow Full access ]", look.selection).push("   [ n Keep ]", look.fg),
            );
            return rows;
        }
        let mut rows: Vec<Row> = self
            .error
            .iter()
            .flat_map(|error| wrap_words(error, TEXT_WIDTH))
            .map(|line| Row::of(line, look.error))
            .collect();
        rows.push(if self.busy {
            Row::of("Saving...", look.running)
        } else {
            Row::of("[> Save ]", look.selection).push("   [ Cancel ]", look.fg)
        });
        rows
    }

    fn hooks_row(&self) -> usize {
        self.allowed.len() + 2
    }

    fn change(&mut self, forward: bool) {
        self.error = None;
        if self.cursor == 0 {
            self.access = next_level(self.access, forward);
            return;
        }
        if self.cursor == self.allowed.len() + 1 {
            self.network = next_network(self.network, forward);
            return;
        }
        if self.cursor == self.hooks_row() {
            self.ide_git_hooks = !self.ide_git_hooks;
            return;
        }
        let row = self.cursor - 1;
        assert!(row < self.allowed.len());
        self.allowed[row] = !self.allowed[row];
    }
}

/// The next access level this computer can run, cycling in either direction. Input: the
/// current level and the direction. Output: that level, or the current one when no other
/// level can run here.
pub(in crate::tui) fn next_level(current: AgentAccess, forward: bool) -> AgentAccess {
    let at = AgentAccess::ALL
        .iter()
        .position(|access| *access == current)
        .unwrap_or(0);
    let count = AgentAccess::ALL.len();
    (1..count)
        .map(|step| {
            if forward {
                (at + step) % count
            } else {
                (at + count - step) % count
            }
        })
        .map(|index| AgentAccess::ALL[index])
        .find(|candidate| candidate.unavailable().is_none())
        .unwrap_or(current)
}

/// The next network choice, cycling in either direction; every choice runs on every
/// computer. Input: the current choice and the direction. Output: the next choice.
pub(in crate::tui) fn next_network(current: AgentNetwork, forward: bool) -> AgentNetwork {
    let at = AgentNetwork::ALL
        .iter()
        .position(|network| *network == current)
        .unwrap_or(0);
    let count = AgentNetwork::ALL.len();
    let step = if forward { 1 } else { count - 1 };
    AgentNetwork::ALL[(at + step) % count]
}

/// One line on what a network choice lets agents and the plugin check reach. Input: the
/// choice. Output: the line.
pub(in crate::tui) fn network_help(network: AgentNetwork) -> &'static str {
    match network {
        AgentNetwork::Open => {
            "Agents with tools on reach the whole network, and so does the plugin check."
        }
        AgentNetwork::Https => {
            "Agents and the plugin check reach port 443 only; agents also still reach the model."
        }
        AgentNetwork::None => {
            "None limits agents to their model: a hosted model's own hosts, through the engine's proxy, or only the local model's port. The plugin check reaches nothing."
        }
    }
}

/// One line on what an access level lets agents do. Input: the level. Output: the line.
pub(in crate::tui) fn level_help(access: AgentAccess) -> &'static str {
    match access {
        AgentAccess::Sandboxed => {
            "Edits only its own worktree; no git commits; your keys and other folders stay out of reach."
        }
        AgentAccess::OwnBranch => {
            "Full git on its own branch in its own git folder; its commits reach your branch only when you Accept."
        }
        AgentAccess::Full => FULL_ACCESS_WARNING,
    }
}

/// The Settings line for a project's agent access: the level and the toolchains agents may
/// use, or loading. Input: the saved view. Output: the text.
pub(in crate::tui) fn summary(view: Option<&PolicyView>) -> String {
    let Some(view) = view else {
        return "Loading...".to_owned();
    };
    let allowed: Vec<&str> = view
        .toolchains
        .iter()
        .filter(|row| row.allowed)
        .map(|row| row.label.as_str())
        .collect();
    let tools = if allowed.is_empty() {
        "no toolchains".to_owned()
    } else {
        allowed.join(", ")
    };
    format!("{} · {tools}", view.access.label())
}
