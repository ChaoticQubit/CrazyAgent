use crate::config::EffectiveConfig;
use crate::engine::EngineError;
use crate::project_setup::{CONTEXT_DIRECTORY, ProjectSetup};
use crate::projects_home::CRAZYAGENT_DIRECTORY;
use crate::protocol::{CredentialRef, Outcome, ProjectId, ProjectSpec};
use crate::sandbox::policy::{AgentAccess, AgentPolicy};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub(crate) mod draft;
pub(crate) mod graph;
#[cfg(unix)]
pub(crate) mod main_graph;
pub mod service;

#[cfg(test)]
mod step_tests;
#[cfg(all(test, unix))]
mod tests;

const PROJECT_ID_BYTES_MAX: usize = 64;
const REASON_BYTES_MAX: usize = 256;
const GRAPHIFY_NOT_INSTALLED: &str = "graphify_not_installed";

/// Where one onboarding step stands: not started (with `until_ms`, the pause
/// end in ms since the epoch, while every pool account that fits is paused
/// after a limit; clients that predate it read a plain wait), running,
/// finished, off (the graph step only, while the Graphify entry is switched
/// off in Setup), not installed (the graph step only, while Setup never
/// installed the Graphify skill), or failed with a plain reason the owner can act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    Waiting {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until_ms: Option<i64>,
    },
    Running,
    Done,
    Off,
    NotInstalled,
    Failed {
        reason: String,
    },
}

/// What the manifest scan and the mapping agent found that has no field in
/// [`ProjectSetup`]: languages, build and test commands, entry points, the research the
/// project's own documents already hold and its other documents, as paths below the
/// folder. `shown` is the engine's own list of the documents the mapping prompt gave with
/// their text, the only paths an answer may name; it never leaves the engine.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappingFacts {
    pub languages: Vec<String>,
    pub build_commands: Vec<String>,
    pub test_commands: Vec<String>,
    pub entry_points: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub research: Vec<FoundResearch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub documents: Vec<String>,
    #[serde(skip)]
    pub(crate) shown: Vec<String>,
}

/// One kind of research the project's own documents already hold, as the mapping agent
/// read it from their text: the kind and the files below the folder that hold it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FoundResearch {
    pub kind: crate::project_setup::ResearchKind,
    pub files: Vec<String>,
}

/// The try a running mapping step is on: its number of `tries`, when it started in ms
/// since the epoch, and the plain reason the try before it failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappingTry {
    pub number: u32,
    pub tries: u32,
    pub started_ms: i64,
    pub earlier: Option<String>,
}

/// The onboarding of one found or cloned project as clients read it: the graph and mapping
/// steps, the try a running mapping step is on as `mapping_try`, the proposed name, the setup the owner confirms with `UpdateProjectSetup`, the
/// extra facts, the `missing` fields such as `about`, the `.crazyagent/overview.md` this
/// engine wrote as `overview` or one it did not write as `folder_overview`, `finished`
/// once the owner saved the setup, when name and draft are the saved ones, and the
/// project's own instruction files the start moved into `.crazyagent/imported/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnboardingView {
    pub project_id: ProjectId,
    pub graph: Step,
    pub mapping: Step,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapping_try: Option<MappingTry>,
    pub name: String,
    pub draft: ProjectSetup,
    pub facts: MappingFacts,
    pub missing: Vec<String>,
    pub overview: Option<String>,
    pub folder_overview: Option<String>,
    pub finished: bool,
    pub imported: Vec<crate::instruction_files::ImportFile>,
}

/// The receipt of `StartOnboarding`: the project the folder is registered as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnboardingStarted {
    pub project_id: ProjectId,
}

/// An owner change the engine records for onboarding: start onboarding the
/// project folder at `path` on the chosen `pool`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OnboardingAction {
    Start { path: String, pool: CredentialRef },
}

impl OnboardingAction {
    /// Checks the owner-supplied path text before anything touches the disk.
    /// Input: an untrusted action. Output: Ok, or a plain refusal code.
    pub fn validate(&self) -> Result<(), EngineError> {
        match self {
            Self::Start { path, .. } => {
                crate::projects_home::path_text(path, "onboarding_path_invalid")
            }
        }
    }
}

impl Step {
    /// A step that has not started and waits on no pause.
    pub const WAITING: Step = Step::Waiting { until_ms: None };

    /// The stored state name and reason of this step. A step that is not
    /// installed is stored as off with its own reason, so every query that
    /// counts an off graph as settled counts it too; a paused wait stores
    /// its end as the reason.
    pub(crate) fn columns(&self) -> (&'static str, Option<String>) {
        match self {
            Self::Waiting { until_ms } => ("waiting", until_ms.map(|until| until.to_string())),
            Self::Running => ("running", None),
            Self::Done => ("done", None),
            Self::Off => ("off", None),
            Self::NotInstalled => ("off", Some(GRAPHIFY_NOT_INSTALLED.to_owned())),
            Self::Failed { reason } => ("failed", Some(reason.clone())),
        }
    }

    /// Rebuilds a step from its stored state name and reason.
    fn from_columns(state: &str, reason: Option<String>) -> Result<Self, EngineError> {
        Ok(match state {
            "waiting" => Self::Waiting {
                until_ms: reason
                    .map(|until| until.parse())
                    .transpose()
                    .map_err(|_| failure("onboarding_step_invalid"))?,
            },
            "running" => Self::Running,
            "done" => Self::Done,
            "off" => match reason.as_deref() {
                None => Self::Off,
                Some(GRAPHIFY_NOT_INSTALLED) => Self::NotInstalled,
                Some(_) => return Err(failure("onboarding_step_invalid")),
            },
            "failed" => Self::Failed {
                reason: reason.unwrap_or_default(),
            },
            _ => return Err(failure("onboarding_step_invalid")),
        })
    }

    /// A failed step with `reason` bounded and free of control characters.
    pub(crate) fn failed(reason: &str) -> Self {
        Self::Failed {
            reason: bounded_reason(reason),
        }
    }
}

/// `reason` free of control characters and cut to [`REASON_BYTES_MAX`] bytes on a
/// character boundary, as a step's stored reason. Input: the text. Output: the reason.
pub(crate) fn bounded_reason(reason: &str) -> String {
    let reason: String = reason.chars().filter(|c| !c.is_control()).collect();
    let mut end = reason.len().min(REASON_BYTES_MAX);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason[..end].to_owned()
}

fn failure(code: &str) -> EngineError {
    EngineError(code.into())
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

fn rejected(code: &str) -> Outcome {
    Outcome::Rejected { code: code.into() }
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The columns that keep a running mapping step's try for the view, which onboarding
/// tables made before 18.15 lack: the try number, its start in ms since the epoch and
/// the reason the try before it failed.
const TRY_COLUMNS: [(&str, &str); 3] = [
    ("mapping_try", "INTEGER"),
    ("mapping_try_ms", "INTEGER"),
    ("mapping_try_earlier", "TEXT"),
];

/// The column that keeps the research and documents the mapping found beside the draft,
/// outside it, so engines up to 18.14, whose draft refuses unknown keys, still read the
/// row; onboarding tables made before 18.15 lack it.
const FOUND_COLUMN: (&str, &str) = ("mapping_found", "TEXT");

/// Creates the onboarding table: one row per imported project with its
/// folder, pool, both steps, the mapping try, the current draft, what the
/// mapping found and the worker's claim, and adds the try and found columns
/// to an older table.
pub fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS onboarding (
                project TEXT PRIMARY KEY,
                repo TEXT NOT NULL UNIQUE,
                pool TEXT NOT NULL,
                graph_state TEXT NOT NULL,
                graph_reason TEXT,
                mapping_state TEXT NOT NULL,
                mapping_reason TEXT,
                mapping_try INTEGER,
                mapping_try_ms INTEGER,
                mapping_try_earlier TEXT,
                drafted TEXT NOT NULL,
                mapping_found TEXT,
                runs INTEGER NOT NULL DEFAULT 0,
                claimer TEXT,
                heartbeat_ms INTEGER,
                created_ms INTEGER NOT NULL,
                updated_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS onboarding_saved_drafts (
                repo TEXT PRIMARY KEY,
                seal TEXT NOT NULL CHECK(length(seal)=64)
            );
            CREATE TABLE IF NOT EXISTS onboarding_saved_overviews (
                repo TEXT PRIMARY KEY,
                seal TEXT NOT NULL CHECK(length(seal)=64)
            );",
        )
        .map_err(error)?;
    for (column, kind) in TRY_COLUMNS.into_iter().chain([FOUND_COLUMN]) {
        let present: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('onboarding') WHERE name=?1)",
                [column],
                |row| row.get(0),
            )
            .map_err(error)?;
        if !present {
            connection
                .execute_batch(&format!(
                    "ALTER TABLE onboarding ADD COLUMN {column} {kind}"
                ))
                .map_err(error)?;
        }
    }
    Ok(())
}

/// The project id a folder is onboarded as: its name in lower case, every
/// run of other characters turned into one `-`, at most 64 bytes, or
/// `project` when nothing is left. Clients compute the same id from the
/// canonical path they send. Returns an error for a path with no name.
pub fn project_id_for(folder: &Path) -> Result<ProjectId, EngineError> {
    let name = folder
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| failure("onboarding_path_invalid"))?;
    let mut slug = String::with_capacity(PROJECT_ID_BYTES_MAX);
    for c in name.chars().flat_map(char::to_lowercase) {
        if slug.len() >= PROJECT_ID_BYTES_MAX {
            break;
        }
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        slug.push_str("project");
    }
    assert!(slug.len() <= PROJECT_ID_BYTES_MAX);
    ProjectId::new(slug).map_err(|problem| failure(&problem.0))
}

/// The owner's home folder from the user profile, canonical when it resolves, so
/// onboarding can refuse it. Takes nothing. Returns the folder, or none when unset.
#[cfg(not(unix))]
fn owner_home() -> Option<PathBuf> {
    let dir = std::env::home_dir()?;
    Some(dir.canonicalize().unwrap_or(dir))
}

#[cfg(unix)]
fn owner_home() -> Option<PathBuf> {
    let user = nix::unistd::User::from_uid(nix::unistd::getuid()).ok()??;
    Some(user.dir.canonicalize().unwrap_or(user.dir))
}

/// Checks the folder the owner asked to onboard: an existing, canonical,
/// real directory at least two levels below `/`, with no quote or backslash
/// in its path, outside the engine's runtime folder, and neither the owner's
/// home, a folder holding it, nor one inside `~/.codex` or `~/.claude`.
/// Takes the path and the engine database. Returns the folder or a code.
fn project_folder(path: &Path, database: &Path) -> Result<PathBuf, &'static str> {
    let text = path.to_str().ok_or("onboarding_path_invalid")?;
    if text.chars().any(|c| matches!(c, '"' | '\\' | '\'')) || path.components().count() < 3 {
        return Err("onboarding_path_invalid");
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "onboarding_folder_missing")?;
    if !metadata.file_type().is_dir() {
        return Err("onboarding_folder_missing");
    }
    let repo = path
        .canonicalize()
        .map_err(|_| "onboarding_folder_missing")?;
    if repo != path {
        return Err("onboarding_path_not_canonical");
    }
    let runtime = database
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .ok_or("onboarding_folder_unsafe")?;
    if repo.starts_with(&runtime) || runtime.starts_with(&repo) {
        return Err("onboarding_folder_unsafe");
    }
    if let Some(home) = owner_home()
        && (home.starts_with(&repo)
            || [".codex", ".claude"]
                .iter()
                .any(|private| repo.starts_with(home.join(private))))
    {
        return Err("onboarding_folder_unsafe");
    }
    Ok(repo)
}

/// Records the seal of the draft the engine saved in `repo`'s `.crazyagent/setup/draft.json`.
/// The row outlives the project, so adding the folder again reuses that draft. Takes the
/// connection, the folder and the draft. Returns any storage failure.
pub(crate) fn seal_draft(
    connection: &Connection,
    repo: &Path,
    drafted: &draft::Drafted,
) -> rusqlite::Result<()> {
    let (Some(repo), Some(seal)) = (repo.to_str(), draft::seal(drafted)) else {
        return Ok(());
    };
    connection.execute(
        "INSERT INTO onboarding_saved_drafts(repo,seal) VALUES (?1,?2)
         ON CONFLICT(repo) DO UPDATE SET seal=excluded.seal",
        params![repo, seal],
    )?;
    Ok(())
}

/// The draft in the folder's `.crazyagent/setup/draft.json` when this engine saved it
/// there: it matches the seal recorded for the folder. A draft the folder came with, or
/// one changed since, is never taken as the engine's. Takes the transaction, the folder
/// and its layout. Returns the draft, or None.
fn engine_draft(
    tx: &Transaction<'_>,
    repo: &str,
    layout: &Path,
) -> Result<Option<draft::Drafted>, EngineError> {
    let sealed: Option<String> = tx
        .query_row(
            "SELECT seal FROM onboarding_saved_drafts WHERE repo=?1",
            [repo],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(sealed) = sealed else {
        return Ok(None);
    };
    Ok(draft::reused(layout)
        .filter(|drafted| draft::seal(drafted).as_deref() == Some(sealed.as_str())))
}

/// Records the seal of the overview the engine wrote in `repo`'s `.crazyagent/overview.md`,
/// so the onboarding view shows that text as the mapping agent's. The row outlives the
/// project like the draft's. Takes the connection, the folder and the overview as it reads
/// back. Returns any storage failure.
#[cfg(unix)]
pub(crate) fn seal_overview(
    connection: &Connection,
    repo: &Path,
    overview: &str,
) -> rusqlite::Result<()> {
    let Some(repo) = repo.to_str() else {
        return Ok(());
    };
    connection.execute(
        "INSERT INTO onboarding_saved_overviews(repo,seal) VALUES (?1,?2)
         ON CONFLICT(repo) DO UPDATE SET seal=excluded.seal",
        params![repo, draft::overview_seal(overview)],
    )?;
    Ok(())
}

fn real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir())
}

/// Registers a found or cloned folder as a project and queues its graph and
/// mapping steps, inside the owner transaction. The folder's `.crazyagent/`
/// is created through the projects home layout, or reused with the draft this
/// engine saved there; a draft the folder came with is not taken as the engine's,
/// and the brief is written from the recorded setup. Research stays off. A new
/// project gets the agent access New Project preselects (Own branch, or Sandboxed
/// where this computer cannot run it), so its agents work in their own clones.
/// Starting a folder that is already onboarding returns its project and
/// retries only failed steps; an onboarding left by a removed project starts
/// over as a new project, also when it was left at another folder with the
/// same name that is gone, and is refused as `onboarding_project_mismatch`
/// while that folder is still there. A new project's own instruction files
/// (CLAUDE.md, .claude/CLAUDE.md, AGENTS.md, CLAUDE.local.md) are copied into its
/// project instructions and moved into `.crazyagent/imported/` last, and the moved
/// files are kept for the onboarding view; a file or import the engine refuses
/// stays in place for the owner's later import. Takes the transaction,
/// configuration, engine database path and action. Returns Applied or a plain
/// rejection.
pub(crate) fn execute(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    database: &Path,
    action: &OnboardingAction,
) -> Result<Outcome, EngineError> {
    action.validate()?;
    let OnboardingAction::Start { path, pool } = action;
    if !crate::project_setup::pool_exists(config, pool) {
        return Ok(rejected("pool_missing"));
    }
    let repo = match project_folder(Path::new(path), database) {
        Ok(repo) => repo,
        Err(code) => return Ok(rejected(code)),
    };
    let project_id = project_id_for(&repo)?;
    let repo_text = repo
        .to_str()
        .ok_or_else(|| failure("onboarding_path_invalid"))?;
    let known: Option<String> = tx
        .query_row(
            "SELECT project FROM onboarding WHERE repo=?1",
            [repo_text],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
            [project_id.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    if let Some(known) = known {
        if known != project_id.as_str() {
            return Ok(rejected("onboarding_project_mismatch"));
        }
        if exists {
            restart(tx, &project_id, pool, graph_step(tx)?)?;
            return Ok(Outcome::Applied);
        }
        tx.execute("DELETE FROM onboarding WHERE project=?1", [known])
            .map_err(error)?;
    }
    if exists {
        return Ok(rejected("project_exists"));
    }
    let leftover: Option<String> = tx
        .query_row(
            "SELECT repo FROM onboarding WHERE project=?1",
            [project_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    if let Some(leftover) = leftover {
        assert_ne!(leftover, repo_text);
        if std::fs::symlink_metadata(&leftover).is_ok() {
            return Ok(rejected("onboarding_project_mismatch"));
        }
        tx.execute(
            "DELETE FROM onboarding WHERE project=?1",
            [project_id.as_str()],
        )
        .map_err(error)?;
    }
    let layout = crate::projects_home::prepare_layout(&repo)?;
    if layout != repo.join(CRAZYAGENT_DIRECTORY)
        || !real_dir(&layout)
        || !real_dir(&layout.join(CONTEXT_DIRECTORY))
    {
        return Ok(rejected("onboarding_layout_incomplete"));
    }
    let mut drafted = engine_draft(tx, repo_text, &layout)?
        .unwrap_or_else(|| draft::Drafted::new(draft::folder_name(&repo), pool));
    drafted.draft.pool = Some(pool.clone());
    drafted.draft.research = Vec::new();
    drafted.draft.documents = Vec::new();
    drafted.draft.validate()?;
    let spec = ProjectSpec {
        name: drafted.name.clone(),
        repositories: Vec::new(),
    };
    spec.validate().map_err(|problem| failure(&problem.0))?;
    let team = match drafted.draft.team.clone() {
        Some(team) => team,
        None => {
            let signed = crate::project_setup::signed_in_connections(tx, config, Some(pool))?;
            crate::project_setup::default_team(config, Some(pool), &[], &signed)
        }
    };
    team.validate()?;
    let outcome = crate::project_setup::record(
        tx,
        config,
        &project_id,
        &spec,
        &drafted.draft,
        team,
        Vec::new(),
        &layout,
    )?;
    if outcome != Outcome::Applied {
        return Ok(outcome);
    }
    let access = Some(AgentAccess::NEW_PROJECT)
        .filter(|level| level.unavailable().is_none())
        .unwrap_or_default();
    let policy = AgentPolicy {
        access,
        toolchains: Vec::new(),
        network: crate::sandbox::policy::AgentNetwork::default(),
        ide_git_hooks: false,
        ide_agent_files: Default::default(),
    };
    crate::sandbox::policy::save(tx, project_id.as_str(), &policy).map_err(error)?;
    let stored = crate::project_setup::load_setup(tx, &project_id)?
        .ok_or_else(|| failure("project_setup_missing"))?;
    crate::project_setup::write_brief_at(&layout, &stored).map_err(failure)?;
    let now = now_ms();
    let graph = graph_step(tx)?;
    let (graph_state, graph_reason) = graph.columns();
    tx.execute(
        "INSERT INTO onboarding(project,repo,pool,graph_state,graph_reason,mapping_state,drafted,created_ms,updated_ms)
         VALUES (?1,?2,?3,?6,?7,'waiting',?4,?5,?5)",
        params![
            project_id.as_str(),
            repo_text,
            pool.as_str(),
            serde_json::to_string(&drafted).map_err(error)?,
            now,
            graph_state,
            graph_reason
        ],
    )
    .map_err(error)?;
    let moved = crate::instruction_files::import(&repo).unwrap_or_default();
    if !moved.is_empty() {
        crate::instruction_files::record_moved(tx, &project_id, &moved)?;
    }
    Ok(Outcome::Applied)
}

/// Where a new or restarted graph step begins: waiting while the Graphify
/// pack entry is installed and on, off while it is switched off, and not
/// installed while Setup never installed it; no graph is built unless on.
fn graph_step(tx: &Transaction<'_>) -> Result<Step, EngineError> {
    let switch = crate::storage::setup::entry_switch(tx, crate::setup::pack::GRAPH_ENTRY)?;
    Ok(match switch {
        Some(true) => Step::WAITING,
        Some(false) => Step::Off,
        None => Step::NotInstalled,
    })
}

/// Sends the failed steps of an onboarding back to waiting on `pool`, so a
/// second start retries them, and a failed or off graph step to `graph`, so
/// installing Graphify or switching it on or off in Setup counts on the next
/// start; finished and running steps keep their state.
fn restart(
    tx: &Transaction<'_>,
    project_id: &ProjectId,
    pool: &CredentialRef,
    graph: Step,
) -> Result<(), EngineError> {
    assert!(matches!(
        graph,
        Step::Waiting { until_ms: None } | Step::Off | Step::NotInstalled
    ));
    let changed = tx
        .execute(
            "UPDATE onboarding SET pool=?2,
                graph_reason=CASE WHEN graph_state IN ('failed','off') THEN ?5 ELSE graph_reason END,
                graph_state=CASE WHEN graph_state IN ('failed','off') THEN ?4 ELSE graph_state END,
                mapping_reason=CASE WHEN mapping_state='failed' THEN NULL ELSE mapping_reason END,
                mapping_state=CASE WHEN mapping_state='failed' THEN 'waiting' ELSE mapping_state END,
                updated_ms=?3
             WHERE project=?1",
            params![
                project_id.as_str(),
                pool.as_str(),
                now_ms(),
                graph.columns().0,
                graph.columns().1
            ],
        )
        .map_err(error)?;
    assert_eq!(changed, 1);
    Ok(())
}

/// The onboarding of `project_id`: both steps, the draft, its proposed name, the extra
/// facts, the fields still missing and the overview, read bounded and never through a link,
/// as the mapping agent's only while it holds the text this engine sealed on writing it.
/// Once the owner saved the setup, name, draft and missing fields come from the saved
/// setup. Returns `None` for a project that was not onboarded.
pub(crate) fn status(
    tx: &Transaction<'_>,
    project_id: &ProjectId,
) -> Result<Option<OnboardingView>, EngineError> {
    let row = tx
        .query_row(
            "SELECT graph_state,graph_reason,mapping_state,mapping_reason,drafted,repo,
                mapping_try,mapping_try_ms,mapping_try_earlier,mapping_found
             FROM onboarding WHERE project=?1",
            [project_id.as_str()],
            |row| {
                Ok((
                    (
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(9)?,
                    ),
                    (
                        row.get::<_, Option<u32>>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                    ),
                ))
            },
        )
        .optional()
        .map_err(error)?;
    let Some(((graph, graph_reason, mapping, mapping_reason, drafted, repo, found), tried)) = row
    else {
        return Ok(None);
    };
    let mapping = Step::from_columns(&mapping, mapping_reason)?;
    let mapping_try = match (&mapping, tried) {
        (Step::Running, (Some(number), Some(started_ms), earlier)) => Some(MappingTry {
            number,
            tries: service::MAPPING_TRIES,
            started_ms,
            earlier,
        }),
        _ => None,
    };
    let mut drafted: draft::Drafted = serde_json::from_str(&drafted).map_err(error)?;
    draft::restore_found(&mut drafted.facts, found.as_deref()).map_err(error)?;
    let saved = crate::project_setup::load_setup(tx, project_id)?.filter(|stored| stored.saved());
    let finished = saved.is_some();
    if let Some(stored) = saved {
        drafted.draft = stored.editable();
        drafted.name = stored.name;
    }
    let missing = drafted.missing();
    let found = draft::overview(&Path::new(&repo).join(CRAZYAGENT_DIRECTORY));
    let sealed: Option<String> = tx
        .query_row(
            "SELECT seal FROM onboarding_saved_overviews WHERE repo=?1",
            [repo.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let wrote = sealed.is_some() && found.as_deref().map(draft::overview_seal) == sealed;
    let found = found.map(|text| draft::names::without_secret_paths(&text));
    let (overview, folder_overview) = if wrote { (found, None) } else { (None, found) };
    Ok(Some(OnboardingView {
        project_id: project_id.clone(),
        graph: Step::from_columns(&graph, graph_reason)?,
        mapping,
        mapping_try,
        name: drafted.name,
        draft: drafted.draft,
        facts: drafted.facts,
        missing,
        overview,
        folder_overview,
        finished,
        imported: crate::instruction_files::moved(tx, project_id, Path::new(&repo))?,
    }))
}
