use crate::engine::EngineError;
use crate::protocol::{Outcome, ProjectId};
use crate::sandbox::policy::AgentAccess;
use crate::storage::versioned;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

pub mod repo_config;

#[cfg(all(test, unix))]
mod tests;

const TABLE: &str = "CREATE TABLE IF NOT EXISTS project_trust (
    project TEXT PRIMARY KEY,
    level TEXT NOT NULL,
    origin TEXT NOT NULL,
    changed_ms INTEGER NOT NULL
)";

/// The file an engine clone leaves in the new project's `.git` folder. Repository content
/// cannot write `.git`, so its presence only ever means the engine cloned the folder.
pub const CLONED_MARKER: &str = "crazyagent-cloned";

/// The line a launch refuses with when the saved agent access is wider than an untrusted
/// project allows; it names the trust level so the owner knows what to change.
pub const UNTRUSTED_ACCESS: &str = "This project's repository is untrusted, so its agents run Sandboxed only. Raise its trust in the project's Settings, Repository trust, before choosing another level.";

/// The refusal code of an agent access choice or launch an untrusted project does not allow.
pub const ACCESS_CODE: &str = "agent_access_untrusted";

/// The refusal code of an untrusted project's agent or check on a platform not qualified
/// for untrusted repositories.
pub const PLATFORM_CODE: &str = "trust_platform_unqualified";

/// The refusal code of a harness without a CrazyAgent adapter in an untrusted project:
/// the engine cannot switch off the repository configuration it reads (threat model D4).
pub const PLAIN_HARNESS_CODE: &str = "launch_harness_untrusted";

/// The line the owner sees for [`PLAIN_HARNESS_CODE`].
pub const PLAIN_HARNESS: &str = "This project's repository is untrusted, so only Claude Code and Codex run its agents: this harness has no CrazyAgent adapter that keeps the repository's own settings, hooks and MCP servers switched off.";

/// The refusal code of a raise from untrusted to reviewed sent without the owner's confirm.
pub const RAISE_CODE: &str = "trust_raise_unconfirmed";

const RULES: [(&str, &str, &str); 9] = [
    (
        "agent_access",
        "Agents run Sandboxed only; Own branch and Full access are refused when chosen and at launch.",
        "Git rights or your own user permissions would let repository content steer an agent outside its worktree.",
    ),
    (
        "network",
        "Agents reach only their model endpoints, and every check runs with network off whatever its profile says.",
        "Repository code or text could send your files or secrets to a host it chooses.",
    ),
    (
        "repository_config",
        "The repository's .claude/, .mcp.json, .codex/, committed .crazyagent/ files, CLAUDE.md and AGENTS.md are never settings, hooks, MCP servers, permissions or instructions: the harness cannot read them, Codex sees the folder as untrusted, and agents get them only as quoted untrusted text. Agents cannot read or edit those files.",
        "A repository cannot grant itself authority.",
    ),
    (
        "harnesses",
        "Only Claude Code and Codex start; harnesses without a CrazyAgent adapter are refused.",
        "The engine cannot switch off the repository configuration those harnesses read.",
    ),
    (
        "check_profile",
        "No check profile is proposed from repository files or by an agent or master; you write the checks.",
        "The checks that judge the work must not come from the work.",
    ),
    (
        "engine_git",
        "Engine git refuses core.fsmonitor, core.sshCommand, core.hooksPath, filter drivers, diff textconv and include.path from the repository's config.",
        "Each of those runs a command or reads config the repository chooses.",
    ),
    (
        "ide",
        "The IDE opens the project in VS Code Restricted Mode, and its git never runs the project's hooks.",
        "Workspace settings, tasks, extensions and hooks from the repository could run code.",
    ),
    (
        "memory",
        "A memory an agent saves stays scoped to this project; only you can widen it.",
        "Text steered by the repository must not reach your other projects.",
    ),
    (
        "limits",
        "Checks and agents run with their time, output and size limits, and only on macOS in this version.",
        "Repository scripts could otherwise run forever or fill the disk, and other platforms are not qualified yet.",
    ),
];

const GAPS: [(&str, &str, &str); 8] = [
    (
        "G1",
        "No CPU, memory or process-count caps on macOS (Seatbelt has none)",
        "Hostile code can slow or exhaust the owner's machine",
    ),
    (
        "G2",
        "Network filtering is by port only until S4b merges",
        "A \"model endpoints\" agent reaches any host on 443",
    ),
    (
        "G3",
        "Prompt injection inside the agent's own sandbox",
        "Steered by repository text, an agent can do anything its sandbox allows: write hostile code into its change set, send misleading team messages, spend model budget, read its own worktree and send it to the model provider",
    ),
    (
        "G4",
        "Check verdicts in an untrusted repository are self-reported",
        "Repository code controls both its exit code and its report, so a hostile suite can report `passed`",
    ),
    (
        "G5",
        "Code that runs inside a confined process keeps that process's grants",
        "Read access to the worktree or snapshot and tool roots; write access to scratch",
    ),
    (
        "G6",
        "After Accept, the owner's own tools run repository content with full owner authority. These are tools outside CrazyAgent: global git hooks and LFS, an editor, `direnv`, `npm install`, builds.",
        "Outside every CrazyAgent boundary",
    ),
    (
        "G7",
        "Harness vendor behavior drifts between versions (flags, native file discovery)",
        "A new version could load a file that today's flags exclude",
    ),
    (
        "G8",
        "Linux and Windows not qualified for this level",
        "No untrusted agents or checks there (D6)",
    ),
];

/// The line agents and checks of an untrusted project are refused with off macOS, where
/// the hidden owner home and the network modes are not qualified (threat model D6, G8).
pub const UNQUALIFIED_PLATFORM: &str = "Untrusted projects run agents and checks only on macOS in this version: this computer's sandbox is not qualified for untrusted repositories.";

/// How far the owner trusts a project's repository content (DEC-049): Reviewed is today's
/// behavior for projects the owner made or knows; Untrusted narrows what the engine lets
/// agents, checks, engine git and the IDE do with the repository, and never widens anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    #[default]
    Reviewed,
    Untrusted,
}

impl Trust {
    /// Every level, from the most to the least trusted.
    pub const ALL: [Self; 2] = [Self::Reviewed, Self::Untrusted];

    /// The level a project cloned from a URL starts at (Owner call 8, plan proposal).
    /// Projects the owner creates or imports start at [`Trust::Reviewed`].
    pub const CLONED: Self = Self::Untrusted;

    /// The level's stable id, as stored and sent to clients.
    pub fn id(self) -> &'static str {
        match self {
            Self::Reviewed => "reviewed",
            Self::Untrusted => "untrusted",
        }
    }

    /// The name shown to the owner.
    pub fn label(self) -> &'static str {
        match self {
            Self::Reviewed => "Reviewed",
            Self::Untrusted => "Untrusted",
        }
    }

    /// One sentence on what the level means, shown beside its name.
    pub fn description(self) -> &'static str {
        match self {
            Self::Reviewed => {
                "You made this project or know its code. Agents and checks get the access you choose."
            }
            Self::Untrusted => {
                "Code you have not reviewed, such as a repository cloned from the internet. The engine locks the rules below."
            }
        }
    }

    /// Tells whether moving from `from` to this level raises trust, the one change that
    /// needs the owner's confirm. Input: the current level. Output: true for a raise.
    pub fn raises(self, from: Self) -> bool {
        self == Self::Reviewed && from == Self::Untrusted
    }

    fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.id() == text)
    }
}

/// Why a project has its trust level: no saved level (the default for a project the owner
/// made or imported, and for projects saved before the setting existed), the clone default,
/// or the owner's own choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    #[default]
    Default,
    Cloned,
    Owner,
}

impl Origin {
    fn id(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Cloned => "cloned",
            Self::Owner => "owner",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [Self::Default, Self::Cloned, Self::Owner]
            .into_iter()
            .find(|origin| origin.id() == text)
    }
}

/// What one trust level lets the engine do, read by every place that enforces it: the
/// widest network any check of the project gets, the ceiling the check queue applies over
/// each check's own `network`; the widest agent access allowed; whether agent network is
/// limited to the model endpoints;
/// whether repository harness files are only labelled text, hidden from the harness and
/// never read by a harness without a CrazyAgent adapter; whether the IDE trusts the
/// workspace and may run the project's git hooks; whether agents and masters may propose
/// check profiles; and whether this platform may run the level's agents and checks at all.
/// Memory needs no field: an untrusted project's agent memory is project-scoped
/// (`memory::library::agent_remember`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Enforcement {
    pub check_network: crate::checks::profile::NetworkMode,
    pub access: AgentAccess,
    pub model_endpoints_only: bool,
    pub repo_config_as_text: bool,
    pub ide_workspace_trust: bool,
    pub agent_profile_proposals: bool,
    pub platform_refusal: Option<&'static str>,
}

impl Enforcement {
    /// The enforcement table. Input: the level. Output: what it allows; Untrusted never
    /// allows more than Reviewed in any field.
    pub fn for_level(trust: Trust) -> Self {
        match trust {
            Trust::Reviewed => Self {
                check_network: crate::checks::profile::NetworkMode::Remotes,
                access: AgentAccess::Full,
                model_endpoints_only: false,
                repo_config_as_text: false,
                ide_workspace_trust: true,
                agent_profile_proposals: true,
                platform_refusal: None,
            },
            Trust::Untrusted => Self {
                check_network: crate::checks::profile::NetworkMode::Off,
                access: AgentAccess::Sandboxed,
                model_endpoints_only: true,
                repo_config_as_text: true,
                ide_workspace_trust: false,
                agent_profile_proposals: false,
                platform_refusal: (!cfg!(target_os = "macos")).then_some(UNQUALIFIED_PLATFORM),
            },
        }
    }

    /// The network one agent gets: open for a tools-on agent of a reviewed project, and only
    /// its model endpoint otherwise, so an untrusted project's agent is never open (threat
    /// model N2). Inputs: whether the agent's tools are on and its model endpoint. Output:
    /// the network.
    pub fn agent_network(
        &self,
        tools: bool,
        model: crate::sandbox::SandboxRemote,
    ) -> crate::sandbox::SandboxNetwork {
        let network = match tools && !self.model_endpoints_only {
            true => crate::sandbox::SandboxNetwork::Open,
            false => crate::sandbox::SandboxNetwork::Only(vec![model]),
        };
        assert!(
            !self.model_endpoints_only || !matches!(network, crate::sandbox::SandboxNetwork::Open)
        );
        network
    }

    /// Tells whether agents may run at `access` under this enforcement. Input: the saved
    /// level. Output: true when it is no wider than the allowed one.
    pub fn allows(&self, access: AgentAccess) -> bool {
        !access.wider_than(self.access)
    }

    /// Why an agent at `access` cannot start under this enforcement, or None when it can:
    /// the platform refusal first, then the access ceiling. Input: the saved level.
    /// Output: the owner's line and its refusal code.
    pub fn refusal(&self, access: AgentAccess) -> Option<(&'static str, &'static str)> {
        self.platform_refusal
            .map(|line| (line, PLATFORM_CODE))
            .or_else(|| (!self.allows(access)).then_some((UNTRUSTED_ACCESS, ACCESS_CODE)))
    }
}

/// One rule the untrusted level locks, with why it is locked, as every client shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    pub text: String,
    pub reason: String,
}

/// One gap no trust level closes, as the threat model states it, with its effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gap {
    pub id: String,
    pub text: String,
    pub effect: String,
}

/// One trust level as a client offers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevelView {
    pub trust: Trust,
    pub label: String,
    pub description: String,
}

/// One project's repository trust as a client shows it: the level, why it has it, every
/// level, the rules Untrusted locks, the gaps no level closes, and the version a save names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustView {
    pub trust: Trust,
    pub origin: Origin,
    pub levels: Vec<LevelView>,
    pub rules: Vec<Rule>,
    pub gaps: Vec<Gap>,
    pub version: u64,
}

fn unreadable(text: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        format!("repository trust {text}").into(),
    )
}

fn engine_error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

/// The versioned item a trust save names, so a save from a screen that read an older
/// level never lands. Input: the project. Output: the item.
pub fn item(project: &ProjectId) -> versioned::Item {
    let item = versioned::Item::new(&format!("repository-trust:{}", project.as_str()));
    assert!(item.is_ok(), "a project id always makes a valid item");
    item.expect("checked above")
}

/// Reads a project's saved trust and why it has it; None when nothing was saved. Inputs:
/// the open connection or transaction and the project id. Output: the saved pair, or the
/// database error for a row holding an unknown level or origin, so a tampered row never
/// reads as Reviewed.
pub fn stored(connection: &Connection, project: &str) -> rusqlite::Result<Option<(Trust, Origin)>> {
    let present: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='project_trust')",
        [],
        |row| row.get(0),
    )?;
    if !present {
        return Ok(None);
    }
    let row: Option<(String, String)> = connection
        .query_row(
            "SELECT level, origin FROM project_trust WHERE project=?1",
            [project],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((level, origin)) = row else {
        return Ok(None);
    };
    let trust = Trust::parse(&level).ok_or_else(|| unreadable("level unknown"))?;
    let origin = Origin::parse(&origin).ok_or_else(|| unreadable("origin unknown"))?;
    Ok(Some((trust, origin)))
}

/// Reads a project's trust level: Reviewed when none was saved. Inputs: the open
/// connection or transaction and the project id. Output: the level, or the database error.
pub fn level(connection: &Connection, project: &str) -> rusqlite::Result<Trust> {
    Ok(stored(connection, project)?.map_or(Trust::Reviewed, |(trust, _)| trust))
}

/// Saves a project's trust level and why it has it. Inputs: the open connection or
/// transaction, the project id, the level and the origin. Output: done, or the database error.
pub(crate) fn save(
    connection: &Connection,
    project: &str,
    trust: Trust,
    origin: Origin,
) -> rusqlite::Result<()> {
    assert!(
        origin != Origin::Default,
        "a saved level always has a reason"
    );
    connection.execute(TABLE, [])?;
    connection.execute(
        "INSERT INTO project_trust (project, level, origin, changed_ms) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(project) DO UPDATE SET level=excluded.level, origin=excluded.origin,
         changed_ms=excluded.changed_ms",
        params![project, trust.id(), origin.id(), crate::team::now_ms()],
    )?;
    Ok(())
}

/// Deletes a project's trust when the project is removed. Inputs: the open connection or
/// transaction and the project id. Output: done, or the database error.
pub(crate) fn remove(connection: &Connection, project: &str) -> rusqlite::Result<()> {
    connection.execute(TABLE, [])?;
    connection.execute("DELETE FROM project_trust WHERE project=?1", [project])?;
    Ok(())
}

/// Builds what a client shows for a project's trust. Inputs: the open connection or
/// transaction and the project. Output: the view, or the plain database code.
pub fn view(connection: &Connection, project: &ProjectId) -> Result<TrustView, EngineError> {
    let (trust, origin) = stored(connection, project.as_str())
        .map_err(|_| engine_error("repository_trust_unreadable"))?
        .unwrap_or_default();
    let version = versioned::current(connection, &item(project))?.version;
    let view = TrustView {
        trust,
        origin,
        levels: Trust::ALL
            .into_iter()
            .map(|trust| LevelView {
                trust,
                label: trust.label().to_owned(),
                description: trust.description().to_owned(),
            })
            .collect(),
        rules: RULES
            .into_iter()
            .map(|(id, text, reason)| Rule {
                id: id.to_owned(),
                text: text.to_owned(),
                reason: reason.to_owned(),
            })
            .collect(),
        gaps: GAPS
            .into_iter()
            .map(|(id, text, effect)| Gap {
                id: id.to_owned(),
                text: text.to_owned(),
                effect: effect.to_owned(),
            })
            .collect(),
        version,
    };
    assert_eq!(view.levels.len(), Trust::ALL.len());
    assert_eq!(view.rules.len(), RULES.len());
    assert_eq!(view.gaps.len(), GAPS.len());
    Ok(view)
}

/// Saves the owner's trust choice when their screen showed the current version. A raise
/// from Untrusted to Reviewed lands only with the owner's confirm; lowering never needs
/// one. Lowering to Untrusted also lowers a saved Own branch or Full access to Sandboxed in
/// the same transaction, so the team runtime stops wider live agents on its next step;
/// raising never raises access. Inputs: the transaction, the project, the version read,
/// the level and whether the owner confirmed a raise. Output: Applied, or a plain rejection
/// (`invalid_project`, `trust_raise_unconfirmed`, `stale_revision`).
pub(crate) fn set(
    tx: &Transaction<'_>,
    project: &ProjectId,
    version: u64,
    trust: Trust,
    confirm_raise: bool,
) -> Result<Outcome, EngineError> {
    let rejected = |code: &str| Ok(Outcome::Rejected { code: code.into() });
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(engine_error)?;
    if !exists {
        return rejected("invalid_project");
    }
    let now =
        level(tx, project.as_str()).map_err(|_| engine_error("repository_trust_unreadable"))?;
    if trust.raises(now) && !confirm_raise {
        return rejected(RAISE_CODE);
    }
    if versioned::owner_write(tx, &item(project), version)?.is_err() {
        return rejected(versioned::STALE_CODE);
    }
    save(tx, project.as_str(), trust, Origin::Owner).map_err(engine_error)?;
    let enforcement = Enforcement::for_level(trust);
    let mut policy = crate::sandbox::policy::load(tx, project.as_str()).map_err(engine_error)?;
    if !enforcement.allows(policy.access) {
        policy.access = enforcement.access;
        versioned::retire(tx, &versioned::agent_policy_item(project))?;
        crate::sandbox::policy::save(tx, project.as_str(), &policy).map_err(engine_error)?;
    }
    assert!(
        enforcement.allows(
            crate::sandbox::policy::load(tx, project.as_str())
                .map_err(engine_error)?
                .access
        )
    );
    Ok(Outcome::Applied)
}

/// Saves the clone default for a folder the owner set up as a project: Untrusted when the
/// engine cloned it ([`CLONED_MARKER`]), nothing otherwise, so an imported folder reads
/// Reviewed. A level the owner already chose is kept. Inputs: the transaction, the project
/// and its folder. Output: done, or the database error.
pub(crate) fn record_onboarded(
    tx: &Transaction<'_>,
    project: &ProjectId,
    folder: &Path,
) -> rusqlite::Result<()> {
    if !cloned(folder) || stored(tx, project.as_str())?.is_some() {
        return Ok(());
    }
    save(tx, project.as_str(), Trust::CLONED, Origin::Cloned)
}

/// The level an agent run over a folder works under, whether or not it is a project yet:
/// the level saved for the project when there is one, else the clone default of
/// [`record_onboarded`], so onboarding maps a fresh clone untrusted before its project
/// exists. Inputs: the connection, the project if any and its folder. Output: the level, or
/// the database error for an unreadable row.
pub(crate) fn of_folder(
    connection: &Connection,
    project: Option<&str>,
    folder: &Path,
) -> rusqlite::Result<Trust> {
    let saved = match project {
        Some(project) => stored(connection, project)?,
        None => None,
    };
    Ok(match saved {
        Some((trust, _)) => trust,
        None if cloned(folder) => Trust::CLONED,
        None => Trust::Reviewed,
    })
}

/// Tells whether the engine cloned this folder: its `.git` is a real folder holding the
/// engine's plain marker file. Input: the project folder. Output: the answer.
pub fn cloned(folder: &Path) -> bool {
    let git = folder.join(".git");
    let real_folder = std::fs::symlink_metadata(&git).is_ok_and(|found| found.is_dir());
    real_folder
        && std::fs::symlink_metadata(git.join(CLONED_MARKER))
            .is_ok_and(|found| found.file_type().is_file())
}

/// Writes the clone marker into a fresh clone's `.git` folder, readable only by this user.
/// Inputs: the checkout and the `owner/name` it was cloned from. Output: the io result; an
/// existing marker is an error, so nothing is ever overwritten.
pub(crate) fn mark_cloned(checkout: &Path, full_name: &str) -> std::io::Result<()> {
    assert!(!full_name.contains('\n'));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(checkout.join(".git").join(CLONED_MARKER))?;
    file.write_all(format!("cloned {full_name}\n").as_bytes())?;
    file.sync_all()
}
