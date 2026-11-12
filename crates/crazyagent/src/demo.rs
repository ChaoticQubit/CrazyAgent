use crate::config::EffectiveConfig;
use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::project_setup::{Platform, ProjectSetup, SetupAction};
use crate::protocol::{CredentialRef, ProjectId, ProjectSpec};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

const ABOUT: &str = "A small web page for a bakery, made by the CrazyAgent demo to show the master, a team agent, a question and a change set.";
const TECH_STACK: [&str; 3] = ["HTML", "CSS", "JavaScript"];
const RCLI: &str = "rcli";
const QUESTION_CONTEXT: &str = "The demo's master uses your answer for the page's starting theme.";
#[cfg(unix)]
const SEED: [(&str, &str); 5] = [
    ("index.html", include_str!("demo/seed/index.html")),
    ("style.css", include_str!("demo/seed/style.css")),
    ("app.js", include_str!("demo/seed/app.js")),
    (
        "test/app.test.js",
        include_str!("demo/seed/test/app.test.js"),
    ),
    ("README.md", include_str!("demo/seed/README.md")),
];
#[cfg(unix)]
const SEED_COMMIT: &str = "Add the demo web page";
#[cfg(unix)]
const SEED_FAILED: &str = "demo_seed_failed";
const GOAL_SENT: &str = "SELECT EXISTS(SELECT 1 FROM master_turns AS t JOIN master_sessions AS s ON s.id=t.session WHERE s.project=?1 AND t.input>(SELECT turn_floor FROM demo_projects WHERE project_id=?1))";
const AGENT_STARTED: &str = "SELECT EXISTS(SELECT 1 FROM team_agents WHERE project=?1 AND role<>'master' AND created_ms>=(SELECT created_ms FROM demo_projects WHERE project_id=?1))";
const QUESTION_ANSWERED: &str = "SELECT EXISTS(SELECT 1 FROM questions_log WHERE project=?1 AND status IN ('answered','answered_in_terminal'))";
const CHANGE_SET_ACCEPTED: &str = "SELECT EXISTS(SELECT 1 FROM landing_change_sets WHERE project=?1 AND state='accepted' AND created_ms>=(SELECT created_ms FROM demo_projects WHERE project_id=?1))";

/// The one id a demo project has (DEC-153): clients send it with CreateDemo and the engine
/// refuses any other, so at most one demo exists and a client never invents its id.
pub const PROJECT_ID: &str = "crazyagent-demo";

/// The demo project's name, which also names its folder in the projects folder.
pub const NAME: &str = "CrazyAgent demo";

/// The goal the guide gives the demo's master, word for word on desktop and in the TUI: add
/// a dark mode switch and a test to the seed web page, with nothing installed or downloaded,
/// and use the owner's answer to the planted question for the starting theme.
pub const GOAL: &str = "This is the CrazyAgent demo: a small web page for a bakery (index.html, style.css, app.js, test/app.test.js). Add a dark mode switch to the page: a button in the header that switches between light and dark colors and remembers the choice. Add a test for it beside test/app.test.js and keep every test passing with node --test. Use plain HTML, CSS and JavaScript only: add no dependencies, no package files and no downloads. Hand the work to one team agent. The page starts in the theme I answer to the demo question on the Questions tab; until I answer, it starts light.";

/// The question the engine plants in the demo when it is made, so the guide's question step
/// never waits on the model to ask one.
pub const QUESTION: &str = "Which theme should the bakery page start in?";

/// The answers the planted question offers, the recommended one first.
pub const QUESTION_OPTIONS: [&str; 3] = ["Follow the system setting", "Light", "Dark"];

/// Every refusal CreateDemo answers with besides the New Project codes it passes through
/// (projects_home_unset, home_new_project_failed, home_new_project_interrupted).
pub const CODES: [&str; 8] = [
    "demo_exists",
    "demo_name_taken",
    "demo_project_id_invalid",
    "demo_pool_missing",
    "demo_pool_not_ready",
    "demo_harness_unavailable",
    "demo_platform_unqualified",
    "demo_seed_failed",
];

/// Where the demo stands, read from live rows only (counts, never content): each step is
/// true once its row exists in the demo project. All false while there is no demo.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemoSteps {
    pub created: bool,
    pub goal_sent: bool,
    pub agent_started: bool,
    pub question_answered: bool,
    pub change_set_accepted: bool,
}

/// One saved pool as the demo dialog offers it: none when a demo can run on it, else the
/// code that says why not (`demo_pool_not_ready`, `demo_harness_unavailable`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemoPool {
    pub pool: CredentialRef,
    pub unavailable: Option<String>,
}

/// The ProjectDemo read: the demo project when one exists (its id, name and when it was
/// made), its steps, the goal words and every saved pool with whether a demo can run on it.
/// Without a demo the id and time are none and every step is false.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemoView {
    pub project_id: Option<ProjectId>,
    pub name: String,
    pub created_ms: Option<u64>,
    pub steps: DemoSteps,
    pub goal: String,
    pub pools: Vec<DemoPool>,
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

/// The demo's fixed project id as a checked id, shared by the engine and its clients.
/// Input: none. Output: the id, or `demo_project_id_invalid` if the constant ever
/// stops being a valid project id.
pub(crate) fn demo_id() -> Result<ProjectId, EngineError> {
    ProjectId::new(PROJECT_ID).map_err(|_| failure("demo_project_id_invalid"))
}

/// Makes the table that marks the demo project, one row at most. Input: the engine
/// connection. Output: done, or the database failure.
pub(crate) fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS demo_projects (
            project_id TEXT PRIMARY KEY NOT NULL CHECK(project_id = '{PROJECT_ID}'),
            created_ms INTEGER NOT NULL CHECK(created_ms > 0),
            turn_floor INTEGER NOT NULL CHECK(turn_floor >= 0)
        )"
    ))
    .map_err(error)
}

/// The New Project request a demo is made with: the demo's name, no repositories,
/// documents, research or read paths, the picked pool, and New Project's own team default.
/// Refuses an id other than PROJECT_ID and a system that runs no team agents. Inputs: the
/// id and the pool. Output: the Create action, or the plain refusal.
pub fn create_action(
    project_id: &ProjectId,
    pool: &CredentialRef,
) -> Result<SetupAction, EngineError> {
    if !cfg!(target_os = "macos") {
        return Err(failure("demo_platform_unqualified"));
    }
    if project_id.as_str() != PROJECT_ID {
        return Err(failure("demo_project_id_invalid"));
    }
    let action = SetupAction::Create {
        project_id: project_id.clone(),
        spec: ProjectSpec {
            name: NAME.into(),
            repositories: Vec::new(),
        },
        setup: Box::new(ProjectSetup {
            about: ABOUT.into(),
            tech_stack: TECH_STACK.map(String::from).to_vec(),
            platforms: vec![Platform::Website],
            research: Vec::new(),
            pool: Some(pool.clone()),
            team: None,
            documents: Vec::new(),
        }),
    };
    action.validate()?;
    Ok(action)
}

/// Says why a demo cannot run on a saved pool, or none when it can: the pool is gone
/// (`demo_pool_missing`), holds only RCLI accounts, which run no live agents until V3
/// (`demo_harness_unavailable`, DEC-141), or has no account New Project's own team check
/// can start the default master on, such as a signed-out one (`demo_pool_not_ready`).
/// Inputs: a transaction, the configuration and the pool. Output: the code or none.
pub(crate) fn pool_unavailable(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    pool: &CredentialRef,
) -> Result<Option<&'static str>, EngineError> {
    let Some(access) = config.settings.model_access.as_ref() else {
        return Ok(Some("demo_pool_missing"));
    };
    let Some(saved) = access.pools.iter().find(|item| &item.id == pool) else {
        return Ok(Some("demo_pool_missing"));
    };
    let harnesses: Vec<&str> = access
        .connections
        .iter()
        .filter(|connection| saved.members.contains(&connection.id))
        .map(|connection| connection.harness.as_str())
        .collect();
    if !harnesses.is_empty() && harnesses.iter().all(|harness| *harness == RCLI) {
        return Ok(Some("demo_harness_unavailable"));
    }
    let signed = crate::project_setup::signed_in_connections(tx, config, Some(pool))?;
    Ok(
        crate::project_setup::default_team_status(config, Some(pool), &signed)
            .map(|_| "demo_pool_not_ready"),
    )
}

/// Tells whether the demo project already exists, so a second CreateDemo is refused as
/// `demo_exists` before any folder is built. Input: a transaction. Output: the answer.
pub(crate) fn exists(tx: &Transaction<'_>) -> Result<bool, EngineError> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
        [PROJECT_ID],
        |row| row.get(0),
    )
    .map_err(error)
}

/// The demo's own code for a New Project refusal of the demo, so the dialog names it: a
/// taken id is `demo_exists`, a taken folder name `demo_name_taken`, a gone pool
/// `demo_pool_missing`; any other code passes through. Input: the code. Output: the code.
pub(crate) fn refusal(code: String) -> String {
    match code.as_str() {
        "project_exists" => "demo_exists".into(),
        "home_destination_exists" => "demo_name_taken".into(),
        "pool_missing" => "demo_pool_missing".into(),
        _ => code,
    }
}

/// Marks the project the demo created as the demo, in the transaction that records it, with
/// the newest master turn cursor so steps count only this demo's rows; a mark a deleted
/// demo left is replaced. Inputs: the transaction, the project and when. Output: done.
pub(crate) fn record(
    tx: &Transaction<'_>,
    project_id: &ProjectId,
    now_ms: i64,
) -> Result<(), EngineError> {
    assert_eq!(project_id.as_str(), PROJECT_ID);
    assert!(now_ms > 0);
    let turn_floor: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(input),0) FROM master_turns",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    assert!(turn_floor >= 0);
    tx.execute(
        "INSERT INTO demo_projects(project_id,created_ms,turn_floor) VALUES(?1,?2,?3)
         ON CONFLICT(project_id) DO UPDATE SET created_ms=excluded.created_ms,
         turn_floor=excluded.turn_floor",
        params![PROJECT_ID, now_ms, turn_floor],
    )
    .map_err(error)?;
    Ok(())
}

/// Asserts the demo's agents start confined right after New Project recorded it (DEC-153,
/// DM-L1): Sandboxed access, no access grants, no read folders and no hidden folders. A
/// wider start is a broken invariant, never an owner choice. Inputs: the connection or
/// transaction that recorded the demo and its id. Output: done, or the database error.
pub(crate) fn assert_confined(db: &Connection, project_id: &ProjectId) -> Result<(), EngineError> {
    assert_eq!(project_id.as_str(), PROJECT_ID);
    let policy = crate::sandbox::policy::load(db, PROJECT_ID).map_err(error)?;
    let read = crate::sandbox::extra_paths::load(db, PROJECT_ID).map_err(error)?;
    let hidden = crate::sandbox::hidden_paths::load(db, PROJECT_ID).map_err(error)?;
    assert_eq!(
        policy.access,
        crate::sandbox::policy::AgentAccess::Sandboxed
    );
    assert!(policy.grants.is_empty(), "demo_policy_grants");
    assert!(read.is_empty(), "demo_read_paths");
    assert!(hidden.is_empty(), "demo_hidden_paths");
    Ok(())
}

/// Plants the demo's question once the demo is recorded, so the guide's question step has
/// one whatever the model does; the owner's answer reaches the master as an engine message.
/// Asking again while it waits returns the same question. Input: the engine connection.
/// Output: the question id.
pub(crate) fn plant_question(db: &Connection) -> Result<u64, EngineError> {
    let ask = crate::questions::Ask {
        question: QUESTION.into(),
        kind: crate::questions::QuestionKind::Single,
        options: QUESTION_OPTIONS.map(String::from).to_vec(),
        recommended: Some(QUESTION_OPTIONS[0].into()),
        context: Some(QUESTION_CONTEXT.into()),
    };
    crate::questions::raise(db, &demo_id()?, &crate::questions::EngineSource::Demo, &ask)
}

fn any(tx: &Transaction<'_>, query: &str) -> Result<bool, EngineError> {
    tx.query_row(query, [PROJECT_ID], |row| row.get(0))
        .map_err(error)
}

/// Reads the demo: the demo project while both its mark and its project exist, each step
/// from whether its row exists (a master turn, a team agent that is not the master, an
/// answered question, an accepted change set), counting only rows made since this demo was
/// marked, since Delete everything keeps some of them; the goal; every saved pool with why
/// a demo cannot run on it. Inputs: a transaction and the configuration. Output: the view.
pub(crate) fn view(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
) -> Result<DemoView, EngineError> {
    let created: Option<i64> = tx
        .query_row(
            "SELECT d.created_ms FROM demo_projects AS d JOIN projects AS p ON p.id=d.project_id
             WHERE d.project_id=?1",
            [PROJECT_ID],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let steps = match created {
        None => DemoSteps::default(),
        Some(_) => DemoSteps {
            created: true,
            goal_sent: any(tx, GOAL_SENT)?,
            agent_started: any(tx, AGENT_STARTED)?,
            question_answered: any(tx, QUESTION_ANSWERED)?,
            change_set_accepted: any(tx, CHANGE_SET_ACCEPTED)?,
        },
    };
    let mut pools = Vec::new();
    for saved in config
        .settings
        .model_access
        .iter()
        .flat_map(|access| access.pools.iter())
    {
        let unavailable = pool_unavailable(tx, config, &saved.id)?.map(String::from);
        pools.push(DemoPool {
            pool: saved.id.clone(),
            unavailable,
        });
    }
    let view = DemoView {
        project_id: created.map(|_| demo_id()).transpose()?,
        name: NAME.into(),
        created_ms: created.map(u64::try_from).transpose().map_err(error)?,
        steps,
        goal: GOAL.into(),
        pools,
    };
    assert_eq!(view.project_id.is_some(), view.steps.created);
    Ok(view)
}

/// Writes the demo web page into a staged New Project folder and commits it on top of the
/// start commit, so every agent worktree has it; a file already there is refused, never
/// replaced. git runs with a private home in the folder's `.crazyagent/`, removed after.
/// Input: the staging folder, a repository the engine made. Output: done, or
/// `demo_seed_failed`.
#[cfg(unix)]
pub(crate) fn seed(staging: &Path) -> Result<(), &'static str> {
    use std::os::unix::fs::DirBuilderExt;
    let state = crate::projects_home::state_folder(staging);
    if state == staging {
        return Err(SEED_FAILED);
    }
    let home = state.join("demo-git-home");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .map_err(|_| SEED_FAILED)?;
    let done = write_seed(staging).and_then(|()| commit_seed(staging, &home));
    let _ = std::fs::remove_dir_all(&home);
    done
}

#[cfg(unix)]
fn write_seed(staging: &Path) -> Result<(), &'static str> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    std::fs::DirBuilder::new()
        .mode(0o755)
        .create(staging.join("test"))
        .map_err(|_| SEED_FAILED)?;
    for (path, text) in SEED {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(staging.join(path))
            .map_err(|_| SEED_FAILED)?;
        file.write_all(text.as_bytes()).map_err(|_| SEED_FAILED)?;
    }
    Ok(())
}

#[cfg(unix)]
fn commit_seed(staging: &Path, home: &Path) -> Result<(), &'static str> {
    let git = |arguments: &[&str]| {
        crate::research::runner::engine_git(staging, home, arguments).map_err(|_| SEED_FAILED)
    };
    let mut add = vec!["add", "--"];
    add.extend(SEED.iter().map(|(path, _)| *path));
    let committed = git(&add)?
        && git(&[
            "-c",
            "user.name=CrazyAgent",
            "-c",
            "user.email=engine@crazyagent.invalid",
            "commit",
            "-q",
            "-m",
            SEED_COMMIT,
        ])?;
    if committed { Ok(()) } else { Err(SEED_FAILED) }
}

/// Refuses the demo seed where the demo is not offered; Create Demo refuses
/// first, so this only keeps the transport whole on every platform.
/// Input: the staging folder. Output: `demo_platform_unqualified`.
#[cfg(not(unix))]
pub(crate) fn seed(_staging: &Path) -> Result<(), &'static str> {
    Err("demo_platform_unqualified")
}

#[cfg(all(test, unix))]
mod tests;
