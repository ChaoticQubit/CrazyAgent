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

/// Checks the model a spawn picked for a new task against the owner's choices for its
/// harness (DEC-150) and pins an allowed pick with its reason to the task, in the caller's
/// transaction, so a refusal rolls the task back. No pick pins nothing: the task runs the
/// default its start reads. Rows of tasks the project no longer keeps are dropped. Takes
/// the transaction, the project, the task, its harness and the pick. Returns the text the
/// reply adds after the harness, " on <model>" for a pinned pick, or the refusal sentence,
/// or a SQLite failure.
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
    let exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='team_task_models')",
            [],
            |row| row.get(0),
        )
        .map_err(super::error)?;
    if !exists {
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
