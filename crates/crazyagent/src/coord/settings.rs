use super::store::{failure, project_known, state};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// The most entries of the counted-tools list (Q18).
pub const COUNTED_TOOLS_MAX: usize = 32;
/// The longest counted-tools entry, in bytes.
pub const COUNTED_TOOL_BYTES_MAX: usize = 64;
/// The counted-tools list a project starts with: the five work tools, context-mode,
/// graphify and skills.
pub const COUNTED_TOOLS_DEFAULT: [&str; 8] = [
    "start_work",
    "inspect_work",
    "update_work",
    "transfer_work",
    "finish_work",
    "ctx_*",
    "graphify",
    "Skill",
];

const _: () = assert!(COUNTED_TOOLS_DEFAULT.len() <= COUNTED_TOOLS_MAX);

/// The most skill names one team role's choice holds: every skill of both scopes.
pub const ROLE_SKILLS_MAX: usize = 2 * crate::plugins::SKILLS_PER_SCOPE_MAX;

/// The harness kinds the coordination settings are kept for, one per existing harness
/// id that runs team agents or the master.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HarnessKind {
    ClaudeCode,
    Codex,
    Rcli,
}

named!(HarnessKind { ClaudeCode => "claude-code", Codex => "codex", Rcli => "rcli" });

impl HarnessKind {
    /// Every kind, in the order settings list them.
    pub const ALL: [Self; 3] = [Self::ClaudeCode, Self::Codex, Self::Rcli];

    /// Maps a harness id as connections and the catalog name it ("claude-code" or
    /// "claude", "codex", "rcli") to its kind. Takes the id. Returns the kind, or None
    /// for a harness without coordination settings.
    pub fn from_harness(id: &str) -> Option<Self> {
        match id {
            "claude-code" | "claude" => Some(Self::ClaudeCode),
            "codex" => Some(Self::Codex),
            "rcli" => Some(Self::Rcli),
            _ => None,
        }
    }
}

/// How a lead of one harness kind gets sub-agents (Q10): the engine starts each as its
/// own process, worktree and branch, or the harness's own teammates run them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentMode {
    #[default]
    AppSpawned,
    Native,
}

named!(SubagentMode { AppSpawned => "app_spawned", Native => "native" });

/// How messages reach an agent of one harness kind (Q13): native channels with typing as
/// the fallback, native channels only, or typing only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    #[default]
    Auto,
    NativeOnly,
    TypingOnly,
}

named!(Delivery { Auto => "auto", NativeOnly => "native_only", TypingOnly => "typing_only" });

/// Where a sub-agent's For: Lead questions go (Q14): its lead, or the owner directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionsTo {
    #[default]
    Lead,
    Me,
}

/// One protection the team gives an agent the same way on every harness that can (DEC-141
/// PA-12): receipts on the messages it is sent, the owner's hooks, and the edit guard that
/// holds an edit of a claimed file under Overlapping edits Block or Wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protection {
    Receipts,
    OwnerHooks,
    EditGuard,
}

impl Protection {
    /// Every protection, in the order the parity check walks them.
    pub const ALL: [Self; 3] = [Self::Receipts, Self::OwnerHooks, Self::EditGuard];
}

/// Why Claude Code on an API key or URL account runs no hooks: it starts `--bare`, which
/// skips settings and plugin hooks (DEC-104 bare-hooks, Claude Code 2.1.322 `--help`).
pub const BARE_NO_HOOKS: &str = "Claude Code on an API key or URL account starts without hooks, so it runs none of your hooks and Block and Wait cannot hold its edits.";
/// Why an rcli master runs no hooks: rcli has none.
pub const RCLI_NO_HOOKS: &str =
    "rcli has no hooks, so it runs none of yours and Block and Wait cannot hold its edits.";
/// Which owner hooks a Codex agent runs: those on an event Codex has (PA-12).
pub const CODEX_HOOK_EVENTS_ONLY: &str = "Codex agents run your hooks on the events Codex has.";
/// What a Codex agent's Messages timeline shows when its app-server lists a hook the launch
/// pinned as not trusted (PA1213R2-F2), so Codex runs neither the edit guard nor that hook.
pub const CODEX_HOOKS_UNTRUSTED: &str = "Codex does not trust CrazyAgent's hooks for this agent, so Block and Wait cannot hold its edits and your hooks do not run for it.";
/// What a Codex agent's Messages timeline shows when the engine cannot ask its app-server
/// about the pinned hooks in time: another Codex release, another system, or no answer.
pub const CODEX_HOOKS_UNCHECKED: &str = "CrazyAgent could not confirm that Codex trusts its hooks for this agent, so Block and Wait may not hold its edits and your hooks may not run for it.";
/// The short form of BARE_NO_HOOKS and RCLI_NO_HOOKS a terminal Hooks heading has room
/// for: which agents run none of the owner's hooks.
pub const HOOKS_NONE_SHORT: &str = "Claude Code on a key or URL and rcli run none";

impl HarnessKind {
    /// Says why this harness's agents miss one protection on one account kind, or None
    /// when they get it (DEC-141 PA-12): Claude Code on a sign-in and Codex on any account
    /// get all three; Claude Code on an API key or URL account starts bare, without hooks
    /// or receipts; rcli has no hooks. Takes the kind, whether the account is an API key or
    /// URL one, and the protection. Returns the sentence the settings show, or None.
    pub fn lacks(self, endpoint: bool, protection: Protection) -> Option<&'static str> {
        match (self, endpoint, protection) {
            (Self::ClaudeCode, true, Protection::Receipts) => Some(BARE_NO_RECEIPTS),
            (Self::ClaudeCode, true, Protection::OwnerHooks | Protection::EditGuard) => {
                Some(BARE_NO_HOOKS)
            }
            (Self::Rcli, _, Protection::OwnerHooks | Protection::EditGuard) => Some(RCLI_NO_HOOKS),
            (Self::ClaudeCode, false, _) | (Self::Codex, _, _) | (Self::Rcli, _, _) => None,
        }
    }
}

/// Why receipts are missing for Claude Code on an API key or URL account, in the words the
/// clients show in place of receipts (`adoption::HOOKLESS_WORDS`).
pub const BARE_NO_RECEIPTS: &str = "Receipts are unavailable for this account kind.";

/// What an edit to a file another agent claimed does (Q15): warn both sides, block the
/// edit where the harness runs the edit guard (`Protection::EditGuard`), or nothing; Wait
/// (OS-8) also refuses a start on a file another agent's work holds and prompts the agent
/// once it is free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Overlaps {
    #[default]
    Warn,
    Block,
    Off,
    Wait,
}

/// How every agent terminal of the project draws, whatever harness runs in it (N-D9,
/// DEC-141): Classic on the normal screen, so the app's terminal view scrolls with the
/// wheel at once and a plain drag selects text, or Fullscreen, the harness's own view, which
/// takes the mouse. Classic is the default. Each harness kind's own switches for a display
/// are `harnesses::interactive::display_switches`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalDisplay {
    #[default]
    Classic,
    Fullscreen,
}

impl TerminalDisplay {
    /// Every display, in the order settings list them.
    pub const ALL: [Self; 2] = [Self::Classic, Self::Fullscreen];
}

/// A team role whose skills the owner chooses (DEC-111 R3): the master, a lead (a
/// sub-agent that may start sub-agents of its own) or a worker (any other sub-agent or
/// teammate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillRole {
    Master,
    Lead,
    Worker,
}

impl SkillRole {
    /// Every role, in the order settings list them.
    pub const ALL: [Self; 3] = [Self::Master, Self::Lead, Self::Worker];
}

/// Which approved skills each team role of a project gets: none is All approved skills
/// (the default), a list is Chosen skills, and an empty list gives that role no skills.
/// A chosen name that is not an approved skill of the project reaches no agent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleSkills {
    pub master: Option<Vec<String>>,
    pub lead: Option<Vec<String>>,
    pub worker: Option<Vec<String>>,
}

impl RoleSkills {
    /// Gives one role's choice. Takes the role. Returns its chosen names, or none for All
    /// approved skills.
    pub fn of(&self, role: SkillRole) -> Option<&[String]> {
        match role {
            SkillRole::Master => self.master.as_deref(),
            SkillRole::Lead => self.lead.as_deref(),
            SkillRole::Worker => self.worker.as_deref(),
        }
    }

    /// Sets one role's choice. Takes the role and its chosen names, or none for All
    /// approved skills. Returns nothing.
    pub fn set(&mut self, role: SkillRole, chosen: Option<Vec<String>>) {
        let slot = match role {
            SkillRole::Master => &mut self.master,
            SkillRole::Lead => &mut self.lead,
            SkillRole::Worker => &mut self.worker,
        };
        *slot = chosen;
    }
}

/// When the workflow nudges remind an agent (DEC-111 Q4, R4): never, with the engine's
/// note for each flag an agent raised after ten minutes of work, or once on its first file
/// edit before any code-graph query, by its hook where hooks run and by an engine note for
/// an agent without them. A stored `true` from before reads as TenMinutes and `false` as Off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Nudges {
    #[default]
    Off,
    TenMinutes,
    FirstEdit,
}

named!(Nudges { Off => "off", TenMinutes => "ten_minutes", FirstEdit => "first_edit" });

impl Nudges {
    /// Every option, in the order settings list them.
    pub const ALL: [Self; 3] = [Self::Off, Self::TenMinutes, Self::FirstEdit];

    /// The words settings show for this option. Takes the option. Returns its fixed text.
    pub fn words(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::TenMinutes => "Every 10 minutes",
            Self::FirstEdit => "First edit without a graph query",
        }
    }
}

impl<'de> Deserialize<'de> for Nudges {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Flag(bool),
            Name(String),
        }
        match Stored::deserialize(deserializer)? {
            Stored::Flag(true) => Ok(Self::TenMinutes),
            Stored::Flag(false) => Ok(Self::Off),
            Stored::Name(name) => {
                Self::parse(&name).ok_or_else(|| serde::de::Error::custom("unknown nudges option"))
            }
        }
    }
}

/// Whether the first turn of each task of a team agent carries the Task-start reminder
/// (T3-15): On, the default, names the context-mode tools to load and the approved skills
/// to check for the task; Off gives none. It is advice, never a guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStart {
    #[default]
    On,
    Off,
}

named!(TaskStart { On => "on", Off => "off" });

impl TaskStart {
    /// Every option, in the order settings list them.
    pub const ALL: [Self; 2] = [Self::On, Self::Off];

    /// The words settings show for this option. Takes the option. Returns its fixed text.
    pub fn words(self) -> &'static str {
        match self {
            Self::On => "On",
            Self::Off => "Off",
        }
    }
}

/// A project's coordination settings. Every option is the owner's choice; the defaults
/// are Lead, Warn, nudges Off, `COUNTED_TOOLS_DEFAULT`, the Classic terminal display and
/// the Task-start reminder On. A row stored before DEC-141 names the display
/// `claude_display`; it reads as `terminal_display`, so the owner's choice is kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoordSettings {
    pub questions_to: QuestionsTo,
    pub overlaps: Overlaps,
    pub nudges: Nudges,
    pub counted_tools: Vec<String>,
    #[serde(default)]
    pub role_skills: RoleSkills,
    #[serde(default, alias = "claude_display")]
    pub terminal_display: TerminalDisplay,
    #[serde(default)]
    pub task_start: TaskStart,
}

impl Default for CoordSettings {
    fn default() -> Self {
        Self {
            questions_to: QuestionsTo::Lead,
            overlaps: Overlaps::Warn,
            nudges: Nudges::Off,
            counted_tools: COUNTED_TOOLS_DEFAULT.map(str::to_owned).to_vec(),
            role_skills: RoleSkills::default(),
            terminal_display: TerminalDisplay::Classic,
            task_start: TaskStart::On,
        }
    }
}

impl CoordSettings {
    /// Checks settings that came from a client: at most `COUNTED_TOOLS_MAX` counted
    /// tools, each 1 to `COUNTED_TOOL_BYTES_MAX` bytes of letters, digits, `_`, `-` and
    /// `*`, none twice, and at most `ROLE_SKILLS_MAX` valid skill names per role, none
    /// twice. Takes the settings. Returns Ok or `coord_settings_invalid`.
    pub fn validate(&self) -> Result<(), EngineError> {
        let name = |tool: &String| {
            !tool.is_empty()
                && tool.len() <= COUNTED_TOOL_BYTES_MAX
                && tool
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'*'))
        };
        let unique = self
            .counted_tools
            .iter()
            .enumerate()
            .all(|(at, tool)| !self.counted_tools[..at].contains(tool));
        let skills = SkillRole::ALL
            .into_iter()
            .filter_map(|role| self.role_skills.of(role))
            .all(|names| {
                names.len() <= ROLE_SKILLS_MAX
                    && names
                        .iter()
                        .all(|skill| crate::plugins::skill_folder::valid_name(skill))
                    && names
                        .iter()
                        .enumerate()
                        .all(|(at, skill)| !names[..at].contains(skill))
            });
        let fine = self.counted_tools.len() <= COUNTED_TOOLS_MAX
            && self.counted_tools.iter().all(name)
            && unique
            && skills;
        fine.then_some(())
            .ok_or_else(|| failure("coord_settings_invalid"))
    }
}

/// Reads a project's coordination settings, the defaults when none are stored. Takes the
/// database and the project. Returns them, or `coord_settings_invalid` for a stored row
/// this build cannot read.
pub fn read(db: &Connection, project: &ProjectId) -> Result<CoordSettings, EngineError> {
    let stored: Option<String> = db
        .query_row(
            "SELECT json FROM coord_settings WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(state)?;
    let Some(json) = stored else {
        return Ok(CoordSettings::default());
    };
    let settings: CoordSettings =
        serde_json::from_str(&json).map_err(|_| failure("coord_settings_invalid"))?;
    settings.validate()?;
    Ok(settings)
}

/// Stores a project's coordination settings after `validate`. Takes the database, the
/// project and the settings. Returns Ok, `coord_settings_invalid` or
/// `coord_project_unknown`.
pub fn write(
    db: &Connection,
    project: &ProjectId,
    settings: &CoordSettings,
) -> Result<(), EngineError> {
    settings.validate()?;
    project_known(db, project)?;
    let json = serde_json::to_string(settings).map_err(state)?;
    db.execute(
        "INSERT INTO coord_settings(project,json) VALUES (?1,?2)
         ON CONFLICT(project) DO UPDATE SET json=excluded.json",
        params![project.as_str(), json],
    )
    .map_err(state)?;
    Ok(())
}

/// Reads how leads of one harness kind get sub-agents in a project, AppSpawned when not
/// set. Takes the database, the project and the kind.
pub fn mode(
    db: &Connection,
    project: &ProjectId,
    harness: HarnessKind,
) -> Result<SubagentMode, EngineError> {
    let stored = column(db, project, harness, "mode")?;
    match stored {
        None => Ok(SubagentMode::default()),
        Some(text) => SubagentMode::parse(&text).ok_or_else(|| failure("coord_settings_invalid")),
    }
}

/// Stores how leads of one harness kind get sub-agents. Native only for a kind that has
/// native teammates (`native_supported`). Takes the database, the project, the kind and
/// the mode. Returns Ok, `coord_native_unsupported` or `coord_project_unknown`.
pub fn set_mode(
    db: &Connection,
    project: &ProjectId,
    harness: HarnessKind,
    mode: SubagentMode,
) -> Result<(), EngineError> {
    if mode == SubagentMode::Native && !native_supported(harness) {
        return Err(failure("coord_native_unsupported"));
    }
    project_known(db, project)?;
    db.execute(
        "INSERT INTO coord_harness_settings(project,harness,mode) VALUES (?1,?2,?3)
         ON CONFLICT(project,harness) DO UPDATE SET mode=excluded.mode",
        params![project.as_str(), harness.as_str(), mode.as_str()],
    )
    .map_err(state)?;
    Ok(())
}

/// Reads how messages reach agents of one harness kind in a project, Auto when not set.
/// Takes the database, the project and the kind.
pub fn delivery(
    db: &Connection,
    project: &ProjectId,
    harness: HarnessKind,
) -> Result<Delivery, EngineError> {
    let stored = column(db, project, harness, "delivery")?;
    match stored {
        None => Ok(Delivery::default()),
        Some(text) => Delivery::parse(&text).ok_or_else(|| failure("coord_settings_invalid")),
    }
}

/// Stores how messages reach agents of one harness kind. Takes the database, the
/// project, the kind and the delivery. Returns Ok or `coord_project_unknown`.
pub fn set_delivery(
    db: &Connection,
    project: &ProjectId,
    harness: HarnessKind,
    delivery: Delivery,
) -> Result<(), EngineError> {
    project_known(db, project)?;
    db.execute(
        "INSERT INTO coord_harness_settings(project,harness,delivery) VALUES (?1,?2,?3)
         ON CONFLICT(project,harness) DO UPDATE SET delivery=excluded.delivery",
        params![project.as_str(), harness.as_str(), delivery.as_str()],
    )
    .map_err(state)?;
    Ok(())
}

/// How a harness kind runs the Native sub-agent mode, the lead's own harness running its
/// sub-agents (Q10, DEC-141 PA-7): Claude Code as an agent team in tmux panes, Codex as
/// multi-agent threads of the lead's own process, or not at all, with the reason the
/// setting shows beside the choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeSubAgents {
    Teams,
    Threads,
    Unavailable(&'static str),
}

/// Why an RCLI lead has no native sub-agents: RCLI runs without tools (PA-3, V3).
pub const RCLI_NATIVE_UNAVAILABLE: &str = "RCLI runs without tools, so it has no native sub-agents yet; its sub-agents are CrazyAgent workers.";

/// What the app cannot show of Codex's own sub-agents (PA-7 r1): Codex 0.164 runs them as
/// threads of the lead's process with no pane, takes their team calls through the lead's
/// MCP servers, and needs a thread count even under No limit.
pub const CODEX_NATIVE_SCOPE: &str = "Codex sub-agents show only in the lead's terminal, where Codex lists each one: the app's graph, counts and Stop act on the lead, their team tool calls count as the lead's, and Codex holds them to Max teammates at once, 15 under No limit because Codex needs a number.";

/// What a harness kind's native sub-agents leave out of the app, for the setting to say
/// beside the choice (DEC-141): Codex threads' limits, nothing for an agent team, whose
/// teammates the app shows, counts and stops. Takes the kind. Returns the text, if any.
pub fn native_scope(harness: HarnessKind) -> Option<&'static str> {
    match native_sub_agents(harness) {
        NativeSubAgents::Threads => Some(CODEX_NATIVE_SCOPE),
        NativeSubAgents::Teams | NativeSubAgents::Unavailable(_) => None,
    }
}

/// Names how a harness kind runs native sub-agents, every kind answered (DEC-141). Takes
/// the kind. Returns the answer.
pub fn native_sub_agents(harness: HarnessKind) -> NativeSubAgents {
    match harness {
        HarnessKind::ClaudeCode => NativeSubAgents::Teams,
        HarnessKind::Codex => NativeSubAgents::Threads,
        HarnessKind::Rcli => NativeSubAgents::Unavailable(RCLI_NATIVE_UNAVAILABLE),
    }
}

/// Says whether a harness kind has native sub-agents the engine can use: Claude Code agent
/// teams and Codex threads (Q10, PA-7). Takes the kind.
pub fn native_supported(harness: HarnessKind) -> bool {
    !matches!(native_sub_agents(harness), NativeSubAgents::Unavailable(_))
}

fn column(
    db: &Connection,
    project: &ProjectId,
    harness: HarnessKind,
    name: &str,
) -> Result<Option<String>, EngineError> {
    assert!(matches!(name, "mode" | "delivery"));
    db.query_row(
        &format!("SELECT {name} FROM coord_harness_settings WHERE project=?1 AND harness=?2"),
        params![project.as_str(), harness.as_str()],
        |row| row.get(0),
    )
    .optional()
    .map_err(state)
}
