use super::tool::ModelPick;
use crate::engine::EngineError;
use crate::model_choice::store::{self, Pick};
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS team_task_models (
    task INTEGER PRIMARY KEY,
    project TEXT NOT NULL,
    model TEXT NOT NULL,
    reason TEXT NOT NULL
);";

/// Checks the model a spawn picked for a new task against the owner's choices and pick
/// rule for its harness (DEC-150) and pins an allowed pick with its reason to the task, in
/// the caller's transaction, so a refusal rolls the task back. No pick, or one the rule
/// overrules, pins nothing: the task runs the default its start reads. Rows of tasks the
/// project no longer keeps are dropped. Takes the transaction, the project, the task, its
/// harness and the pick. Returns the text the reply adds after the harness, " on <model>"
/// for a pinned pick, or the refusal sentence, or a SQLite failure.
pub(crate) fn pin(
    tx: &Transaction<'_>,
    project: &ProjectId,
    task: i64,
    harness: &str,
    pick: Option<&ModelPick>,
) -> Result<Result<String, String>, EngineError> {
    assert!(task > 0);
    let catalog = crate::model_choice::catalog(harness);
    let picked = pick.map(|pick| pick.model.as_str());
    let shown = super::bound_text(harness, 64);
    let model = match store::pick(tx, harness, &catalog, project, picked)? {
        Pick::Run(model) => model,
        Pick::Overruled(default) => {
            let on = match default {
                Some(default) => format!("the default model {default}"),
                None => "the account's own model".to_owned(),
            };
            return Ok(Ok(format!(
                " on {on}: the owner runs every agent on the default"
            )));
        }
        Pick::Unsupported => {
            return Ok(Err(format!(
                "CrazyAgent does not choose the model of a {shown} agent; leave model out (model_not_supported)."
            )));
        }
        Pick::Refused { allowed, default } => {
            let fallback = match default {
                Some(default) => format!("the default {default}"),
                None => "the account's own model".to_owned(),
            };
            let model = picked.expect("only a pick is refused");
            return Ok(Err(if allowed.is_empty() {
                format!(
                    "the owner allows no other model for {shown} agents than {fallback}; leave model out (model_not_allowed)."
                )
            } else {
                format!(
                    "{model} is not on the owner's list for {shown} agents: pick one of {}, or leave model out to run {fallback} (model_not_allowed).",
                    allowed.join(", ")
                )
            }));
        }
    };
    let Some(pick) = pick else {
        return Ok(Ok(String::new()));
    };
    assert_eq!(model.as_deref(), Some(pick.model.as_str()));
    tx.execute_batch(SCHEMA).map_err(super::error)?;
    tx.execute(
        "DELETE FROM team_task_models WHERE project=?1
         AND task NOT IN (SELECT id FROM team_tasks WHERE project=?1)",
        [project.as_str()],
    )
    .map_err(super::error)?;
    let changed = tx
        .execute(
            "INSERT INTO team_task_models(task,project,model,reason) VALUES (?1,?2,?3,?4)",
            params![task, project.as_str(), pick.model, pick.reason],
        )
        .map_err(super::error)?;
    assert_eq!(changed, 1);
    Ok(Ok(format!(" on {}", pick.model)))
}

/// Reads the model a task's spawn pinned and its reason (DEC-150), none when the spawn
/// left the model to the default. Takes the database and the task. Returns the pick, or a
/// SQLite failure.
pub(crate) fn pinned(db: &Connection, task: i64) -> Result<Option<ModelPick>, EngineError> {
    if !table(db, "team_task_models")? {
        return Ok(None);
    }
    db.query_row(
        "SELECT model,reason FROM team_task_models WHERE task=?1",
        [task],
        |row| {
            Ok(ModelPick {
                model: row.get(0)?,
                reason: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(super::error)
}

const RUN_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS team_agent_models (
    agent INTEGER PRIMARY KEY,
    project TEXT NOT NULL,
    model TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS team_model_fallbacks (
    project TEXT NOT NULL,
    harness TEXT NOT NULL,
    model TEXT NOT NULL,
    noticed_ms INTEGER NOT NULL,
    PRIMARY KEY (project, harness, model)
) WITHOUT ROWID;";

/// Reads the model a task's agent should start on (DEC-150): the spawn's pinned pick,
/// else the project's agent default for the harness, else the app's, none for the
/// account's own model. Takes the database, the task, the project and the harness.
/// Returns the model, or a store or SQLite failure.
pub(crate) fn wanted(
    db: &Connection,
    task: i64,
    project: &ProjectId,
    harness: &str,
) -> Result<Option<String>, EngineError> {
    if let Some(pick) = pinned(db, task)? {
        return Ok(Some(pick.model));
    }
    let catalog = crate::model_choice::catalog(harness);
    store::launch_model(
        db,
        harness,
        &catalog,
        Some(project),
        crate::model_choice::Slot::Agent,
    )
}

/// Reads the model one agent starts on before its account's own (DEC-150): a master the
/// owner's master model for its harness in this project, read at every start so a changed
/// setting reaches the next one, any other agent the model recorded for it. Takes the
/// database, the project, the agent, its harness and whether it is the master. Returns the
/// model, none for the account's own, or a store or SQLite failure.
pub(crate) fn start_model(
    db: &Connection,
    project: &ProjectId,
    agent: i64,
    harness: &str,
    master: bool,
) -> Result<Option<String>, EngineError> {
    assert!(agent > 0);
    if !master {
        return agent_model(db, agent);
    }
    let catalog = crate::model_choice::catalog(harness);
    store::launch_model(
        db,
        harness,
        &catalog,
        Some(project),
        crate::model_choice::Slot::Master,
    )
}

/// Tells whether a pool account can run a model: any account runs its own, a keyless
/// endpoint runs only the one model it serves, a managed profile any model its harness
/// takes. Takes the account's reach and the model, none for the account's own. Returns
/// the answer.
pub(crate) fn can_run(reach: &super::TeamReach, model: Option<&str>) -> bool {
    match (reach, model) {
        (_, None) => true,
        (super::TeamReach::Endpoint { model: own, .. }, Some(model)) => own == model,
        (super::TeamReach::Managed { .. }, Some(_)) => true,
    }
}

/// Records the model an agent starts on, so its restarts and wakes keep it and an idle
/// agent takes only tasks for the same model; none removes the record, so the account's
/// own model runs. Takes the database, the project, the agent and the model. Returns a
/// SQLite failure.
pub(crate) fn record_agent(
    db: &Connection,
    project: &ProjectId,
    agent: i64,
    model: Option<&str>,
) -> Result<(), EngineError> {
    assert!(agent > 0);
    db.execute_batch(RUN_SCHEMA).map_err(super::error)?;
    let changed = match model {
        Some(model) => db
            .execute(
                "INSERT INTO team_agent_models(agent,project,model) VALUES (?1,?2,?3)
                 ON CONFLICT(agent) DO UPDATE SET model=excluded.model",
                params![agent, project.as_str(), model],
            )
            .map_err(super::error)?,
        None => db
            .execute("DELETE FROM team_agent_models WHERE agent=?1", [agent])
            .map_err(super::error)?,
    };
    assert!(changed <= 1);
    Ok(())
}

/// Reads the model an agent was started on, none for the account's own. Takes the
/// database and the agent. Returns the model, or a SQLite failure.
pub(crate) fn agent_model(db: &Connection, agent: i64) -> Result<Option<String>, EngineError> {
    if !table(db, "team_agent_models")? {
        return Ok(None);
    }
    db.query_row(
        "SELECT model FROM team_agent_models WHERE agent=?1",
        [agent],
        |row| row.get(0),
    )
    .optional()
    .map_err(super::error)
}

/// Tells the owner and the master once that no pool account could run a model chosen for
/// a harness's agents or master, so a task or the master runs on the default (DEC-150):
/// one owner notification and one engine message to the master, then nothing more for
/// that project, harness and model until it runs again (`ran`). Takes the transaction, the
/// project, the harness, the model, the task, none for the master, and the time. Returns
/// whether this call told them, or a SQLite failure.
pub(crate) fn fell_back(
    tx: &Transaction<'_>,
    project: &ProjectId,
    harness: &str,
    model: &str,
    task: Option<i64>,
    now: i64,
) -> Result<bool, EngineError> {
    assert!(task.is_none_or(|task| task > 0));
    tx.execute_batch(RUN_SCHEMA).map_err(super::error)?;
    let inserted = tx
        .execute(
            "INSERT OR IGNORE INTO team_model_fallbacks(project,harness,model,noticed_ms)
             VALUES (?1,?2,?3,?4)",
            params![project.as_str(), harness, model, now],
        )
        .map_err(super::error)?;
    assert!(inserted <= 1);
    if inserted == 0 {
        return Ok(false);
    }
    let harness = super::bound_text(harness, 64);
    let (whom, who) = match task {
        Some(task) => (format!("{harness} agents"), format!("task {task}")),
        None => (format!("the {harness} master"), "the master".to_owned()),
    };
    let text = format!(
        "No account in this project's pool can run {model} for {whom} now, so {who} runs on the default model. You are told again only after {model} runs once more."
    );
    super::events::notify(
        tx,
        project,
        super::events::NotificationKind::ModelFallback,
        &text,
        now,
    )?;
    let _ = super::append_message(
        tx,
        project,
        super::MessageAuthor::Engine,
        &format!("{text} (model_fallback)"),
        super::MessageState::Pending,
        now,
    )?;
    Ok(true)
}

/// Forgets a told fallback once a model runs again, so a later fallback is told anew.
/// Takes the database, the project, the harness and the model. Returns a SQLite failure.
pub(crate) fn ran(
    db: &Connection,
    project: &ProjectId,
    harness: &str,
    model: &str,
) -> Result<(), EngineError> {
    if !table(db, "team_model_fallbacks")? {
        return Ok(());
    }
    db.execute(
        "DELETE FROM team_model_fallbacks WHERE project=?1 AND harness=?2 AND model=?3",
        params![project.as_str(), harness, model],
    )
    .map_err(super::error)?;
    Ok(())
}

fn table(db: &Connection, name: &str) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [name],
        |row| row.get(0),
    )
    .map_err(super::error)
}

#[cfg(all(test, unix))]
mod tests;
