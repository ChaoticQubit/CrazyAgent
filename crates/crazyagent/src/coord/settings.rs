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

/// What an edit to a file another agent claimed does (Q15): warn both sides, block the
/// edit where the harness has a pre-edit hook, or nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Overlaps {
    #[default]
    Warn,
    Block,
    Off,
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

/// A project's coordination settings. Every option is the owner's choice; the defaults
/// are Lead, Warn, nudges Off and `COUNTED_TOOLS_DEFAULT`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoordSettings {
    pub questions_to: QuestionsTo,
    pub overlaps: Overlaps,
    pub nudges: Nudges,
    pub counted_tools: Vec<String>,
}

impl Default for CoordSettings {
    fn default() -> Self {
        Self {
            questions_to: QuestionsTo::Lead,
            overlaps: Overlaps::Warn,
            nudges: Nudges::Off,
            counted_tools: COUNTED_TOOLS_DEFAULT.map(str::to_owned).to_vec(),
        }
    }
}

impl CoordSettings {
    /// Checks settings that came from a client: at most `COUNTED_TOOLS_MAX` counted
    /// tools, each 1 to `COUNTED_TOOL_BYTES_MAX` bytes of letters, digits, `_`, `-` and
    /// `*`, none twice. Takes the settings. Returns Ok or `coord_settings_invalid`.
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
        let fine = self.counted_tools.len() <= COUNTED_TOOLS_MAX
            && self.counted_tools.iter().all(name)
            && unique;
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

/// Says whether a harness kind has native teammates the engine can use: Claude Code
/// agent teams only (Q10). Takes the kind.
pub fn native_supported(harness: HarnessKind) -> bool {
    harness == HarnessKind::ClaudeCode
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
