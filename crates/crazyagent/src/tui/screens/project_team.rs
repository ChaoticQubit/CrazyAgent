use super::new_project::{HarnessChoice, HarnessState, harness_name, same_harness};
use super::new_project_input::TextInput;
use super::new_project_page::harness_choices_from;
use super::new_project_view::{Look, Row, columns, list_row, spread, wrap_words};
use super::project::{REFRESH_MS, plain_problem};
use super::project_time::{clock, duration};
use super::team_messages::{ListKey, MASTER_ELSEWHERE, Messages, Shown};
use crate::coord::settings::{
    COUNTED_TOOL_BYTES_MAX, COUNTED_TOOLS_DEFAULT, COUNTED_TOOLS_MAX, Delivery, HarnessKind,
    HeavyLane, LISTED_NATIVE_UNAVAILABLE, NativeSubAgents, Nudges, Overlaps, QuestionsTo,
    RESOURCE_NAME_BYTES_MAX, ROLE_SKILLS_MAX, SHARED_RESOURCES_MAX, SharedResource, SkillRole,
    SubagentMode, TaskStart, TerminalDisplay, native_scope, native_sub_agents,
};
use crate::engine::owner::OwnerResult;
use crate::harnesses::HarnessEntry;
use crate::harnesses::leftovers::HoldState;
use crate::project_setup::{
    ProjectSetup, SetupAction, SetupQuery, SetupView, StoredSetup, TeamPlan,
};
use crate::protocol::ProjectId;
use crate::team::agent_messages::{KEPT_CHOICES, MessageSettings, PAGE_CHOICES, ResultsShown};
use crate::team::events::{EventKind, HOURLY_CAP_MAX, RuleTarget, WakeRule};
use crate::team::{
    AgentRole, AgentStatus, BackgroundAtEnd, BranchMode, CatchUpMain, ChildLanding,
    CoordinationView, DelegationPath, LandingMode, LeftoverMode, MASTER_BRANCH_BYTES_MAX,
    MasterLands, ReuseIdle, SwitchApply, SwitchMode, TASK_PROMPT_BYTES_MAX, TaskSource, TaskStatus,
    TeamAction, TeamAgent, TeamHarness, TeamStatus, TeamTask,
};
use crate::tui::keys::Key;
use crate::tui::seam::{AccountRow, account_label};
use crate::tui::theme::Role;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::Style;

mod coord;
mod draw;
mod settings;
mod state;

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

    /// Draws the notice wrapped to a width, so a long one is read whole instead of cut.
    /// Takes the look and the width in cells. Returns its rows in the error or plain style.
    pub fn rows(&self, look: &Look, width: usize) -> Vec<Row> {
        let style = if self.error { look.error } else { look.fg };
        wrap_words(&self.text, width)
            .into_iter()
            .map(|line| Row::of(line, style))
            .collect()
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
/// Ignored lets the host's own keys run; Change is one owner team action, sent as
/// `SetupQuery::TeamChange`; Save is one project setup change, sent as `SetupQuery::Change`;
/// both with a fresh generation.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::tui) enum TeamReply {
    Ignored,
    Handled,
    Change(TeamAction),
    Save(SetupAction),
}

/// The open master harness picker. `cursor` is the picked row among the harnesses that can
/// run the master; none until the owner moves it, which keeps it on the saved harness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) struct MasterPick {
    pub cursor: Option<usize>,
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

/// Where the team read after a saved change stands: due, or sent after the save. Every change
/// is built from the team read on screen, so the next change waits for a read sent after the
/// save; a read sent before it still shows the older settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reread {
    Due,
    Sent,
}

const WAKE_ROWS: usize = EventKind::ALL.len() + 1;
const MESSAGE_ROWS: usize = 6;
const MESSAGE_LABEL: usize = 40;

/// The open coordination editor (DEC-104): the row with the cursor and, while the owner
/// types the counted tools or one role's skill names, that input. Rows follow `COORD_ROWS`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct CoordEdit {
    pub row: usize,
    pub input: Option<TextInput>,
}

/// One row of the coordination editor: a harness kind's sub-agent mode or message delivery,
/// the one terminal display of every harness (N-D9, DEC-141), where sub-agent questions go,
/// overlapping edits, whether agents may catch up with main (OS-8), workflow nudges, the
/// Task-start reminder (T3-15), the counted tools, the skills one team role gets (DEC-111
/// R3), the heavy lane size or the shared resources (F28).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum CoordRow {
    Mode(HarnessKind),
    Delivery(HarnessKind),
    TerminalDisplay,
    Questions,
    Overlaps,
    CatchUpMain,
    Nudges,
    TaskStart,
    Counted,
    Skills(SkillRole),
    ReuseIdle,
    LandingMode,
    HeavyLane,
    Resources,
}

const COORD_ROWS: [CoordRow; 20] = [
    CoordRow::Mode(HarnessKind::ClaudeCode),
    CoordRow::Delivery(HarnessKind::ClaudeCode),
    CoordRow::Mode(HarnessKind::Codex),
    CoordRow::Delivery(HarnessKind::Codex),
    CoordRow::Mode(HarnessKind::Rcli),
    CoordRow::Delivery(HarnessKind::Rcli),
    CoordRow::TerminalDisplay,
    CoordRow::Questions,
    CoordRow::Overlaps,
    CoordRow::CatchUpMain,
    CoordRow::Nudges,
    CoordRow::TaskStart,
    CoordRow::Counted,
    CoordRow::Skills(SkillRole::Master),
    CoordRow::Skills(SkillRole::Lead),
    CoordRow::Skills(SkillRole::Worker),
    CoordRow::ReuseIdle,
    CoordRow::LandingMode,
    CoordRow::HeavyLane,
    CoordRow::Resources,
];
const RESOURCES_TEXT_BYTES_MAX: usize = SHARED_RESOURCES_MAX * (RESOURCE_NAME_BYTES_MAX + 6);
const DELIVERIES: [Delivery; 3] = [Delivery::Auto, Delivery::NativeOnly, Delivery::TypingOnly];
const OVERLAPS: [Overlaps; 4] = [
    Overlaps::Warn,
    Overlaps::Block,
    Overlaps::Off,
    Overlaps::Wait,
];
const OVERLAPS_HOLD_NOTE: &str = "  except Claude Code on a key or URL";
const COUNTED_TEXT_BYTES_MAX: usize = COUNTED_TOOLS_MAX * (COUNTED_TOOL_BYTES_MAX + 2);
const SKILL_NAMES_TEXT_BYTES_MAX: usize =
    ROLE_SKILLS_MAX * (crate::plugins::skill_folder::NAME_BYTES_MAX + 2);

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

/// The branch setting the open branch editor writes: the Master's branch (DEC-100) or the
/// Land on branch (DEC-111 Q1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum BranchSetting {
    Master,
    LandOn,
}

/// The open branch editor: the setting it writes and the branch name being typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct BranchEdit {
    pub setting: BranchSetting,
    pub input: TextInput,
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
    pub ask: Option<Ask>,
    pub draft: Option<Draft>,
    pub branch_edit: Option<BranchEdit>,
    pub wake: Option<WakeEdit>,
    pub coord: Option<CoordEdit>,
    pub messages: Option<Messages>,
    pub message_row: Option<usize>,
    pub master_pick: Option<MasterPick>,
    pub alerts: bool,
    pub notice: Option<Notice>,
    pub now_ms: i64,
    pub offset: i64,
    read_ms: Option<i64>,
    waiting: bool,
    changing: Option<String>,
    reread: Option<Reread>,
    stored: Option<Box<StoredSetup>>,
    setup_waiting: bool,
    setup_failed_ms: Option<i64>,
    saving: Option<SetupAction>,
    catalog: Option<Vec<HarnessChoice>>,
    catalog_due: bool,
    settings_scroll: usize,
    settings_scroll_max: std::cell::Cell<usize>,
    settings_window: std::cell::Cell<usize>,
}

/// What V says for a master with no live terminal, whose conversation already is the console,
/// and what the master view says when such a master takes it back to the console.
pub(in crate::tui) const NO_LIVE_TERMINAL: &str =
    "This master has no live terminal: its conversation is the console.";

/// Says whether "Live terminal" governs the project's master (OWNER-Q4-2-L3). Input: the team
/// status. Output: true for a master harness the engine reads as a terminal app, and with no
/// master yet, whose choice is stored for the master that comes.
pub(in crate::tui) fn master_runs_live(status: &TeamStatus) -> bool {
    status
        .harnesses
        .iter()
        .find(|harness| harness.role == AgentRole::Master)
        .is_none_or(|harness| {
            crate::harnesses::interactive::support(&harness.harness)
                == crate::harnesses::interactive::Support::Terminal
        })
}

/// The "End a turn after done" choice after `secs`: each of `IDLE_CHOICES_SECS` in turn,
/// then Off, then the first again (F15). Input: the saved choice, none for Off. Output: the
/// next choice.
fn done_quiet_next(secs: Option<u32>) -> Option<u32> {
    let choices = crate::team::IDLE_CHOICES_SECS;
    match secs.and_then(|secs| choices.iter().position(|choice| *choice == secs)) {
        Some(at) => choices.get(at + 1).copied(),
        None if secs.is_none() => Some(choices[0]),
        None => Some(crate::team::DONE_QUIET_DEFAULT_SECS),
    }
}

/// How the Team settings list shows the "End a turn after done" choice. Input: the saved
/// choice, none for Off. Output: the words.
fn done_quiet_words(secs: Option<u32>) -> String {
    secs.map_or_else(
        || "Off".to_owned(),
        |secs| crate::team::span_words(secs.into()),
    )
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

/// Words a Sub-agent rounds per message choice. Takes the choice, None for Until the goal is
/// done. Returns the words.
fn rounds_choice(max: Option<u8>) -> String {
    max.map_or_else(
        || "Until the goal is done".to_owned(),
        |max| max.to_string(),
    )
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

/// Writes shared resources as their row shows and edits them: `name:slots` entries joined
/// by ", ". Takes the resources. Returns the text.
fn resources_text(resources: &[SharedResource]) -> String {
    resources
        .iter()
        .map(|resource| format!("{}:{}", resource.name, resource.slots))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Reads typed shared resources: `name` or `name:slots` entries split on commas or spaces,
/// one slot when none is given, at most `SHARED_RESOURCES_MAX + 1` entries read so a long
/// line is refused by the engine's rule. Takes the text. Returns the resources, or None for
/// slots that are not a number.
fn resources_typed(typed: &str) -> Option<Vec<SharedResource>> {
    let mut found = Vec::new();
    for entry in typed
        .split([',', ' ', '\t'])
        .filter(|entry| !entry.is_empty())
        .take(SHARED_RESOURCES_MAX + 1)
    {
        let (name, slots) = match entry.split_once(':') {
            Some((name, slots)) => (name, slots.parse::<u8>().ok()?),
            None => (entry, 1),
        };
        found.push(SharedResource {
            name: name.to_owned(),
            slots,
        });
    }
    assert!(found.len() <= SHARED_RESOURCES_MAX + 1);
    Some(found)
}

/// The choice after or before `current` in `all`, wrapping at both ends.
/// Takes the choices, the current one and the direction. Returns the next choice.
fn cycle<T: Copy + PartialEq>(all: &[T], current: T, forward: bool) -> T {
    assert!(!all.is_empty());
    let at = all.iter().position(|item| *item == current).unwrap_or(0);
    let next = if forward {
        (at + 1) % all.len()
    } else {
        (at + all.len() - 1) % all.len()
    };
    all[next]
}

/// The words a per-harness Coordination row goes by, the same as the desktop's: "RCLI
/// sub-agents" for its mode, "Messages to RCLI agents" for its message delivery. Inputs:
/// whether the row is the delivery row and its harness kind. Output: the label.
fn harness_row_label(delivery: bool, kind: HarnessKind) -> String {
    let name = harness_name(kind.as_str(), None);
    match delivery {
        true => format!("Messages to {name} agents"),
        false => format!("{name} sub-agents"),
    }
}

/// The words a coordination row shows for a harness kind's sub-agent mode (PA-7): Native
/// names what the kind runs, an agent team's teammates or its own harness's sub-agents.
/// Takes the kind and the mode. Returns the words.
fn mode_words(kind: HarnessKind, mode: SubagentMode) -> &'static str {
    match (mode, native_sub_agents(kind)) {
        (SubagentMode::AppSpawned, _) => "app-spawned",
        (SubagentMode::Native, NativeSubAgents::Teams) => "native teammates",
        (SubagentMode::Native, NativeSubAgents::Threads | NativeSubAgents::Unavailable(_)) => {
            "native sub-agents"
        }
    }
}

/// Why a harness kind's sub-agents stay app-spawned, as the coordination editor says it
/// when Native is chosen on a kind without native sub-agents (PA-7). Takes the kind.
/// Returns the kind's own reason, or the general one for a kind this build runs natively.
fn native_unavailable(kind: HarnessKind) -> &'static str {
    match native_sub_agents(kind) {
        NativeSubAgents::Unavailable(reason) => reason,
        NativeSubAgents::Teams | NativeSubAgents::Threads => LISTED_NATIVE_UNAVAILABLE,
    }
}

fn delivery_words(delivery: Delivery) -> &'static str {
    match delivery {
        Delivery::Auto => "Auto: native, typing as fallback",
        Delivery::NativeOnly => "Native only",
        Delivery::TypingOnly => "Typing only",
    }
}

/// What the Delivery choices do on one harness kind (PA-11, DEC-141): Native only never
/// types, and a harness with no channel while its agent works says how its messages arrive
/// instead. The app shows the same sentences under each Messages to row. Takes the kind.
/// Returns the sentences.
fn delivery_scope(kind: HarnessKind) -> &'static str {
    match kind {
        HarnessKind::ClaudeCode => {
            "Auto hands a message to a Claude Code agent through its relay and types it in if the relay has not taken it after 60 seconds. Native only uses the relay alone and never types. Typing only types every message in."
        }
        HarnessKind::Codex => {
            "Auto hands a message to a Codex agent with tools on through its app-server bridge and types it in when the bridge can't take it. Native only never types: with tools off, a Codex agent gets its messages as a task of their own once it is idle. Typing only types every message in."
        }
        HarnessKind::Rcli => {
            "RCLI runs no team agents, so no message goes to one here. A master on any harness hears its messages through the engine under every choice."
        }
        HarnessKind::Listed(_) => {
            "This harness has no channel of its own: Auto and Typing only type every message in, and Native only never types."
        }
    }
}

fn display_words(display: TerminalDisplay) -> &'static str {
    match display {
        TerminalDisplay::Classic => "Classic",
        TerminalDisplay::Fullscreen => "Fullscreen",
    }
}

/// Says which harness kinds cannot draw the chosen Fullscreen display and so stay Classic
/// (DEC-141), as the engine's coordination view names them. Classic holds for every kind, and
/// a kind whose row lists no displays, sent by an engine from before DEC-141, is not named.
/// Takes the view and the display. Returns a sentence with a leading space, or nothing.
fn display_kept(view: &CoordinationView, display: TerminalDisplay) -> String {
    if display == TerminalDisplay::Classic {
        return String::new();
    }
    let kept: Vec<String> = view
        .harnesses
        .iter()
        .filter(|row| !row.displays.is_empty() && !row.displays.contains(&display))
        .map(|row| harness_name(row.harness.as_str(), None))
        .collect();
    match kept.len() {
        0 => String::new(),
        1 => format!(" {} stays Classic.", kept[0]),
        _ => format!(" {} stay Classic.", kept.join(" and ")),
    }
}

fn overlaps_words(overlaps: Overlaps) -> &'static str {
    match overlaps {
        Overlaps::Warn => "Warn",
        Overlaps::Block => "Block",
        Overlaps::Off => "Off",
        Overlaps::Wait => "Wait",
    }
}

fn role_label(role: SkillRole) -> &'static str {
    match role {
        SkillRole::Master => "Master",
        SkillRole::Lead => "Lead",
        SkillRole::Worker => "Worker",
    }
}

/// One Messages setting as the editor shows it. Takes the settings and the row, 0 to 5 in
/// the editor's order; any later row is the page size. Returns the label and the value in
/// words.
fn message_setting(settings: &MessageSettings, row: usize) -> (&'static str, String) {
    let shown = |on: bool| if on { "Shown" } else { "Hidden" }.to_owned();
    match row {
        0 => (
            "Keep the master's messages to agents",
            if settings.keep_master_messages {
                "On"
            } else {
                "Off"
            }
            .to_owned(),
        ),
        1 => (
            "Messages kept per project",
            settings.kept_per_project.to_string(),
        ),
        2 => ("Progress events", shown(settings.progress_shown)),
        3 => (
            "Task results",
            match settings.results {
                ResultsShown::FirstLine => "First line, Enter shows all",
                ResultsShown::Full => "Full text",
            }
            .to_owned(),
        ),
        4 => (
            "Your own tasks and prompts",
            shown(settings.owner_items_shown),
        ),
        _ => ("Entries per page", settings.page_size.to_string()),
    }
}

/// The settings with one row moved to its next or previous choice; a stored value outside
/// the choices goes to the first. Takes the settings, the row and the direction. Returns the
/// changed settings.
fn next_message_setting(
    mut settings: MessageSettings,
    row: usize,
    forward: bool,
) -> MessageSettings {
    match row {
        0 => settings.keep_master_messages = !settings.keep_master_messages,
        1 => {
            settings.kept_per_project =
                cycle_or_first(&KEPT_CHOICES, settings.kept_per_project, forward)
        }
        2 => settings.progress_shown = !settings.progress_shown,
        3 => {
            settings.results = match settings.results {
                ResultsShown::FirstLine => ResultsShown::Full,
                ResultsShown::Full => ResultsShown::FirstLine,
            }
        }
        4 => settings.owner_items_shown = !settings.owner_items_shown,
        _ => settings.page_size = cycle_or_first(&PAGE_CHOICES, settings.page_size, forward),
    }
    settings
}

/// The choice after or before `now` in a ring of choices; one outside them goes to the
/// first. Takes the choices, the value and the direction. Returns the choice.
fn cycle_or_first<T: Copy + PartialEq>(choices: &[T], now: T, forward: bool) -> T {
    let count = choices.len();
    match choices.iter().position(|choice| *choice == now) {
        Some(at) if forward => choices[(at + 1) % count],
        Some(at) => choices[(at + count - 1) % count],
        None => choices[0],
    }
}

/// A Messages list line as a Team view row. Takes the line and the look. Returns the row in
/// the style of the line's color role.
fn shown_row(line: Shown, look: &Look) -> Row {
    let style = match line.role {
        None => look.fg,
        Some(Role::Heading) => look.heading,
        Some(Role::Muted) => look.muted,
        Some(Role::Accent) => look.accent,
        Some(Role::Success) => look.success,
        Some(Role::Warning) => look.warning,
        Some(Role::Error) => look.error,
        Some(Role::Running) => look.running,
    };
    Row::of(line.text, style)
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

/// Which agents Background commands at task end acts on, said with every choice in the
/// TUI and the desktop alike: every choice on Claude Code and Codex panes, whose records
/// report their background commands (PA-10), a turn-end stop for agents run per turn, and
/// none for rcli, which has no tools (rv-code BHT-M1).
const BACKGROUND_SCOPE: &str = "Wait, a grace time and Stop act the same on Claude Code and Codex panes. Agents run per turn, whatever their harness, have their background commands stopped when the turn ends, and an RCLI agent has no tools, so it starts none.";

/// The most holds the Team rows list; the rest stay in the team view.
const LEFTOVER_ROWS_MAX: usize = 5;

/// The summary words for Left running after a stop (DEC-147.4). Input: the setting. Output:
/// the words.
fn leftover_words(mode: LeftoverMode) -> &'static str {
    match mode {
        LeftoverMode::End => "End them",
        LeftoverMode::Keep => "Keep running",
        LeftoverMode::Ask => "Ask me",
    }
}

/// Where one hold stands, as its row says it, with the keys that still change it. Input: the
/// state. Output: the words.
fn hold_words(state: HoldState) -> &'static str {
    match state {
        HoldState::Held => "waiting for you: X ends, K keeps",
        HoldState::Ending => "ending: K keeps",
        HoldState::Kept => "kept running: X ends",
    }
}

/// The summary words for Background commands at task end (DEC-111 Q13). Input: the
/// choice. Output: the words.
fn background_words(choice: BackgroundAtEnd) -> String {
    match choice {
        BackgroundAtEnd::Wait => "Wait".to_owned(),
        BackgroundAtEnd::Grace { secs } => {
            format!("Stop after {}", crate::team::span_words(secs.into()))
        }
        BackgroundAtEnd::Stop => "Stop at turn end".to_owned(),
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
        "stale_revision" => {
            "This changed in another window, so yours was not saved. The saved one shows now."
                .into()
        }
        "team_time_limits_invalid" => {
            "Pick a listed time limit; an agent session may not be shorter than a task.".into()
        }
        "team_agent_not_found" => "That agent is no longer part of this team.".into(),
        "team_notification_missing" => "There is no such notification to mark as seen.".into(),
        "team_stop_master" => "The master stops with Stop all.".into(),
        "team_teammate_not_found" => "That teammate has already ended.".into(),
        "team_cap_invalid" => "Pick a listed limit, or No limit.".into(),
        "team_rounds_invalid" => {
            "Pick a listed number of sub-agent rounds, or Until the goal is done.".into()
        }
        "team_run_time_invalid" => "Pick a listed master run time, or No limit.".into(),
        "team_budget_invalid" => "Pick a listed token budget, or Off.".into(),
        "coord_native_unsupported" => LISTED_NATIVE_UNAVAILABLE.into(),
        "coord_settings_invalid" => {
            "Count at most 32 tools, each named once with letters, digits, _, - or *.".into()
        }
        "coord_project_unknown" => "This project was not found.".into(),
        "team_master_branch_invalid" => {
            "Type a branch name such as main or release/2.0, or leave it empty for the repository default.".into()
        }
        "team_land_on_invalid" => {
            "Type a branch name such as stage or release/2.0, or leave it empty for the master's branch.".into()
        }
        "team_background_grace_invalid" => "Pick a grace time from 10 s to 1 h.".into(),
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

/// Draws the editor of one branch setting: the Master's branch or Land on. Takes the
/// editor, the content width and the look. Returns the rows and the cursor as a row and a
/// column inside those rows.
fn branch_rows(edit: &BranchEdit, content: usize, look: &Look) -> (Vec<Row>, (usize, usize)) {
    let (heading, help, empty) = match edit.setting {
        BranchSetting::Master => (
            "Master's branch",
            "The master works there and merges finished harnesses into it. Leave it empty for the repository default.",
            "Repository default",
        ),
        BranchSetting::LandOn => (
            "Land on",
            "The master's merges and your Accept land there, and each change set counts only what it lacks. It can be a branch checked out nowhere: then your project folder's files stay as they are. Leave it empty for the master's branch.",
            "The master's branch",
        ),
    };
    let input = &edit.input;
    let mut rows = vec![Row::of(heading, look.heading)];
    rows.extend(
        wrap_words(help, content)
            .into_iter()
            .map(|line| Row::of(line, look.muted)),
    );
    let inner = content.saturating_sub(4).max(2);
    let (lines, (line, column)) = input.wrap(inner - 1);
    let text = if input.value().is_empty() {
        Row::of(empty, look.muted)
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

/// Packs the team settings onto summary rows after the counts, each setting whole and in
/// order: a setting that does not fit starts the next row. Inputs: the settings, the counts,
/// the content width and the style of the gap between settings. Output: each row's counts
/// part and settings part, the counts on the first row only.
fn pack(items: &[Row], counts: Row, content: usize, gap: Style) -> Vec<(Row, Row)> {
    let used = |(lead, right): &(Row, Row)| match (lead.width(), right.width()) {
        (0, 0) => 0,
        (0, taken) | (taken, 0) => taken + 2,
        (lead, taken) => lead + 2 + taken + 2,
    };
    let mut rows = vec![(counts, Row::default())];
    for item in items {
        let fits = |row: &(Row, Row)| used(row) == 0 || used(row) + item.width() <= content;
        let at = rows.last().is_some_and(fits).then(|| rows.len() - 1);
        let at = at.unwrap_or_else(|| {
            rows.push((Row::default(), Row::default()));
            rows.len() - 1
        });
        let right = std::mem::take(&mut rows[at].1);
        rows[at].1 = if right.width() == 0 {
            item.clone()
        } else {
            right.push("  ", gap).cat(item.clone())
        };
    }
    rows
}

/// How many settings rows after the first scroll out of view to win back `over` rows: one
/// more than `over`, for the marker row, keeping at least one scrolled row in view; none when
/// that wins no row back. Inputs: the packed rows, the counts row included, and the rows the
/// window is short by. Output: the rows scrolled out of view.
fn scrolled_rows(packed: usize, over: usize) -> usize {
    let hidden = (over + 1).min(packed.saturating_sub(2));
    if over == 0 || hidden < 2 { 0 } else { hidden }
}

/// The marker row under scrolled settings, right-aligned like them: the keys that bring more
/// settings and which way they are. Inputs: whether settings are above and below the view,
/// the content width and the look. Output: the row.
fn scroll_marker(above: bool, below: bool, content: usize, look: &Look) -> Row {
    assert!(
        above || below,
        "the marker shows only while settings are out of view"
    );
    let (keys, words) = match (above, below) {
        (true, true) => ("PgUp PgDn", "more settings above and below"),
        (true, false) => ("PgUp", "more settings above"),
        (false, _) => ("PgDn", "more settings below"),
    };
    spread(
        content,
        Row::default(),
        Row::of(keys, look.accent).push(format!("  {words}"), look.muted),
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

/// Names an account as the owner sees it in Accounts.
/// Takes the account id, if there is one, and the owner's account rows.
/// Returns the account's words ([`account_label`]: the name, with the id after a name two
/// accounts share), the id itself while no name is known, or "-" for none.
pub(in crate::tui) fn account_name(id: Option<&str>, accounts: &[AccountRow]) -> String {
    match id {
        None => "-".into(),
        Some(id) => account_label(id, accounts).unwrap_or_else(|| id.to_owned()),
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

/// The words for the Apply a master harness change setting, after the On harness switch
/// choice on the team summary row, where the plan line already names the saved master.
/// Takes the mode. Returns its choice name in lower case.
fn apply_words(mode: SwitchApply) -> &'static str {
    match mode {
        SwitchApply::WhenIdle => "when the master is idle",
        SwitchApply::NextStart => "at the next start",
    }
}

/// The short words for the On harness switch setting on the team summary row.
/// Takes the mode. Returns its choice name.
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
