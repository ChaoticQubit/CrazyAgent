use crate::config::EffectiveConfig;
use crate::engine::EngineError;
use crate::project_setup::{CONTEXT_DIRECTORY, ProjectSetup};
use crate::projects_home::CRAZYAGENT_DIRECTORY;
use crate::protocol::{CredentialRef, Outcome, ProjectId, ProjectSpec};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub(crate) mod draft;
pub(crate) mod graph;
pub mod service;

#[cfg(all(test, unix))]
mod tests;

const PROJECT_ID_BYTES_MAX: usize = 64;
const REASON_BYTES_MAX: usize = 256;

/// Where one onboarding step stands: not started, running, finished, off
/// (the graph step only, while the Graphify entry is not on in Setup), or
/// failed with a plain reason the owner can act on, such as
/// "graphify not installed".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    Waiting,
    Running,
    Done,
    Off,
    Failed { reason: String },
}

/// What the manifest scan and the mapping agent found that has no field in
/// [`ProjectSetup`]: languages, build and test commands and entry points.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappingFacts {
    pub languages: Vec<String>,
    pub build_commands: Vec<String>,
    pub test_commands: Vec<String>,
    pub entry_points: Vec<String>,
}

/// The onboarding of one found or cloned project as clients read it: the
/// graph and mapping steps, the proposed name, the pre-filled setup the owner
/// confirms with `UpdateProjectSetup`, the extra facts, `missing`, the draft
/// fields the owner must still fill such as `about` or `platforms`, and the
/// mapping agent's `.crazyagent/overview.md` once it exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnboardingView {
    pub project_id: ProjectId,
    pub graph: Step,
    pub mapping: Step,
    pub name: String,
    pub draft: ProjectSetup,
    pub facts: MappingFacts,
    pub missing: Vec<String>,
    pub overview: Option<String>,
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
    /// The stored state name and reason of this step.
    pub(crate) fn columns(&self) -> (&'static str, Option<&str>) {
        match self {
            Self::Waiting => ("waiting", None),
            Self::Running => ("running", None),
            Self::Done => ("done", None),
            Self::Off => ("off", None),
            Self::Failed { reason } => ("failed", Some(reason.as_str())),
        }
    }

    /// Rebuilds a step from its stored state name and reason.
    fn from_columns(state: &str, reason: Option<String>) -> Result<Self, EngineError> {
        Ok(match state {
            "waiting" => Self::Waiting,
            "running" => Self::Running,
            "done" => Self::Done,
            "off" => Self::Off,
            "failed" => Self::Failed {
                reason: reason.unwrap_or_default(),
            },
            _ => return Err(failure("onboarding_step_invalid")),
        })
    }

    /// A failed step with `reason` bounded and free of control characters.
    pub(crate) fn failed(reason: &str) -> Self {
        let reason: String = reason.chars().filter(|c| !c.is_control()).collect();
        let mut end = reason.len().min(REASON_BYTES_MAX);
        while !reason.is_char_boundary(end) {
            end -= 1;
        }
        Self::Failed {
            reason: reason[..end].to_owned(),
        }
    }
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

/// Creates the onboarding table: one row per imported project with its
/// folder, pool, both steps, the current draft and the worker's claim.
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
                drafted TEXT NOT NULL,
                runs INTEGER NOT NULL DEFAULT 0,
                claimer TEXT,
                heartbeat_ms INTEGER,
                created_ms INTEGER NOT NULL,
                updated_ms INTEGER NOT NULL
            );",
        )
        .map_err(error)
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

fn real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir())
}

/// Registers a found or cloned folder as a project and queues its graph and
/// mapping steps, inside the owner transaction. The folder's `.crazyagent/`
/// is created through the projects home layout, or reused with its saved
/// draft when it already exists; nothing in it is overwritten. Research stays
/// off. Starting a folder that is already onboarding returns its project and
/// retries only failed steps. Takes the transaction, configuration, engine
/// database path and action. Returns Applied or a plain rejection.
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
    if let Some(known) = known {
        if known != project_id.as_str() {
            return Ok(rejected("onboarding_project_mismatch"));
        }
        restart(tx, &project_id, pool, graph_step(tx)?)?;
        return Ok(Outcome::Applied);
    }
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
            [project_id.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    if exists {
        return Ok(rejected("project_exists"));
    }
    let layout = crate::projects_home::prepare_layout(&repo)?;
    if layout != repo.join(CRAZYAGENT_DIRECTORY)
        || !real_dir(&layout)
        || !real_dir(&layout.join(CONTEXT_DIRECTORY))
    {
        return Ok(rejected("onboarding_layout_incomplete"));
    }
    let mut drafted = draft::reused(&layout)
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
    if std::fs::symlink_metadata(layout.join(CONTEXT_DIRECTORY).join("brief.md")).is_err() {
        let stored = crate::project_setup::load_setup(tx, &project_id)?
            .ok_or_else(|| failure("project_setup_missing"))?;
        crate::project_setup::write_brief_at(&layout, &stored).map_err(failure)?;
    }
    let now = now_ms();
    tx.execute(
        "INSERT INTO onboarding(project,repo,pool,graph_state,mapping_state,drafted,created_ms,updated_ms)
         VALUES (?1,?2,?3,?6,'waiting',?4,?5,?5)",
        params![
            project_id.as_str(),
            repo_text,
            pool.as_str(),
            serde_json::to_string(&drafted).map_err(error)?,
            now,
            graph_step(tx)?.columns().0
        ],
    )
    .map_err(error)?;
    Ok(Outcome::Applied)
}

/// Where a new or restarted graph step begins: waiting while the Graphify
/// pack entry is installed and on, otherwise off so no graph is built.
fn graph_step(tx: &Transaction<'_>) -> Result<Step, EngineError> {
    let on = crate::storage::setup::entry_on(tx, crate::setup::pack::GRAPH_ENTRY)?;
    Ok(if on { Step::Waiting } else { Step::Off })
}

/// Sends the failed steps of an onboarding back to waiting on `pool`, so a
/// second start retries them, and a failed or off graph step to `graph`, so
/// switching Graphify on or off in Setup counts on the next start; finished
/// and running steps keep their state.
fn restart(
    tx: &Transaction<'_>,
    project_id: &ProjectId,
    pool: &CredentialRef,
    graph: Step,
) -> Result<(), EngineError> {
    assert!(matches!(graph, Step::Waiting | Step::Off));
    let changed = tx
        .execute(
            "UPDATE onboarding SET pool=?2,
                graph_reason=CASE WHEN graph_state IN ('failed','off') THEN NULL ELSE graph_reason END,
                graph_state=CASE WHEN graph_state IN ('failed','off') THEN ?4 ELSE graph_state END,
                mapping_reason=CASE WHEN mapping_state='failed' THEN NULL ELSE mapping_reason END,
                mapping_state=CASE WHEN mapping_state='failed' THEN 'waiting' ELSE mapping_state END,
                updated_ms=?3
             WHERE project=?1",
            params![project_id.as_str(), pool.as_str(), now_ms(), graph.columns().0],
        )
        .map_err(error)?;
    assert_eq!(changed, 1);
    Ok(())
}

/// The onboarding of `project_id`: both steps, the draft, its proposed name,
/// the extra facts, the fields still missing and the overview, read bounded
/// and never through a link. Returns `None` for a project that was not
/// onboarded.
pub(crate) fn status(
    tx: &Transaction<'_>,
    project_id: &ProjectId,
) -> Result<Option<OnboardingView>, EngineError> {
    let row = tx
        .query_row(
            "SELECT graph_state,graph_reason,mapping_state,mapping_reason,drafted,repo
             FROM onboarding WHERE project=?1",
            [project_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()
        .map_err(error)?;
    let Some((graph, graph_reason, mapping, mapping_reason, drafted, repo)) = row else {
        return Ok(None);
    };
    let drafted: draft::Drafted = serde_json::from_str(&drafted).map_err(error)?;
    let missing = drafted.missing();
    Ok(Some(OnboardingView {
        project_id: project_id.clone(),
        graph: Step::from_columns(&graph, graph_reason)?,
        mapping: Step::from_columns(&mapping, mapping_reason)?,
        name: drafted.name,
        draft: drafted.draft,
        facts: drafted.facts,
        missing,
        overview: draft::overview(&Path::new(&repo).join(CRAZYAGENT_DIRECTORY)),
    }))
}
