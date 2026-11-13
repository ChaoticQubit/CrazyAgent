use super::tool::ModelPick;
use crate::engine::EngineError;
use crate::model_choice::store::{self, Pick};
use crate::protocol::{CredentialRef, ProjectId};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS team_task_models (
    task INTEGER PRIMARY KEY,
    project TEXT NOT NULL,
    model TEXT NOT NULL,
    reason TEXT NOT NULL
);";

/// The reason an owner's own delegate pins with its model (DEC-150 addendum 7), shown on
/// the task where a master's pick shows the reason it gave.
pub(crate) const OWNER_PICK_REASON: &str = "Picked by the owner.";

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
    Ok(
        pin_or_refuse(tx, project, task, harness, pick)?.map_err(|refused| {
            assert!(refused.sentence.ends_with(&format!("({}).", refused.code)));
            refused.sentence
        }),
    )
}

/// A pick the owner's choices refused: the code an owner action answers with and the
/// sentence a master reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refused {
    pub(crate) code: &'static str,
    pub(crate) sentence: String,
}

/// Checks and pins a pick as [`pin`] does, keeping the refusal's code beside its sentence
/// for an owner action (DEC-150 addendum 7). Takes what [`pin`] takes. Returns the text a
/// reply adds after the harness, or the refusal, or a SQLite failure.
pub(crate) fn pin_or_refuse(
    tx: &Transaction<'_>,
    project: &ProjectId,
    task: i64,
    harness: &str,
    pick: Option<&ModelPick>,
) -> Result<Result<String, Refused>, EngineError> {
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
            return Ok(Err(Refused {
                code: "model_not_supported",
                sentence: format!(
                    "CrazyAgent does not choose the model of a {shown} agent; leave model out (model_not_supported)."
                ),
            }));
        }
        Pick::Refused { allowed, default } => {
            let fallback = match default {
                Some(default) => format!("the default {default}"),
                None => "the account's own model".to_owned(),
            };
            let model = picked.expect("only a pick is refused");
            let sentence = if allowed.is_empty() {
                format!(
                    "the owner allows no other model for {shown} agents than {fallback}; leave model out (model_not_allowed)."
                )
            } else {
                format!(
                    "{model} is not on the owner's list for {shown} agents: pick one of {}, or leave model out to run {fallback} (model_not_allowed).",
                    allowed.join(", ")
                )
            };
            return Ok(Err(Refused {
                code: "model_not_allowed",
                sentence,
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
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS team_model_refusals (
    project TEXT NOT NULL,
    connection TEXT NOT NULL,
    model TEXT NOT NULL,
    until_ms INTEGER NOT NULL,
    PRIMARY KEY (project, connection, model)
) WITHOUT ROWID;";

/// How long a harness's refusal of a model keeps an account from starts on that model
/// (DEC-150 F8): a plan seldom gains a model sooner, and a refusal told wrongly passes.
pub(crate) const MODEL_REFUSAL_MS: i64 = 6 * 60 * 60 * 1000;

/// The most model refusals one project keeps, as an agent's harness stream can tell any
/// number (DEC-150 F8); a new one past it drops the one closest to its end.
pub(crate) const REFUSALS_MAX: usize = 64;

/// Reads the model a task's agent should start on (DEC-150): the spawn's pinned pick
/// while the owner's choices still allow it (`still_allowed`), else the project's agent
/// default for the harness, else the app's, none for the account's own model. Takes the
/// database, the task, the project and the harness. Returns the model, or a store or
/// SQLite failure.
pub(crate) fn wanted(
    db: &Connection,
    task: i64,
    project: &ProjectId,
    harness: &str,
) -> Result<Option<String>, EngineError> {
    if let Some(pick) = pinned(db, task)? {
        return still_allowed(db, project, harness, &pick.model);
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

/// Reads the model one agent starts on before its account's own (DEC-150), at every start
/// so a changed setting reaches the next one: a master the owner's master model for its
/// harness in this project, any other agent the model recorded for it while the owner's
/// choices still allow it (`still_allowed`). Takes the database, the project, the agent,
/// its harness and whether it is the master. Returns the model, none for the account's
/// own, or a store or SQLite failure.
pub(crate) fn start_model(
    db: &Connection,
    project: &ProjectId,
    agent: i64,
    harness: &str,
    master: bool,
) -> Result<Option<String>, EngineError> {
    assert!(agent > 0);
    if !master {
        return match agent_model(db, agent)? {
            Some(model) => still_allowed(db, project, harness, &model),
            None => Ok(None),
        };
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

/// The line that heads the master's list of models a task may name (DEC-150 F9).
pub(crate) const MODELS_HEAD: &str = "Models a task may name, by harness:";

/// Writes the master's list of the models a spawn may name for each harness the plan uses
/// (DEC-150 F9), as the owner's pick rule offers them, so the master picks only from the
/// allowed list. Takes the database, the project and the harnesses, repeats allowed.
/// Returns the head and one line per harness whose model CrazyAgent chooses, empty when
/// none is, or a store or SQLite failure.
pub(crate) fn master_lines(
    db: &Connection,
    project: &ProjectId,
    harnesses: &[&str],
) -> Result<String, EngineError> {
    let mut seen: Vec<&str> = Vec::with_capacity(harnesses.len());
    let mut lines = String::new();
    for harness in harnesses {
        if seen.contains(harness) {
            continue;
        }
        seen.push(harness);
        let catalog = crate::model_choice::catalog(harness);
        let Some(offer) = store::offer(db, harness, &catalog, project)? else {
            continue;
        };
        let default = match offer.default.as_deref() {
            Some(model) => format!("the default, {model}"),
            None => "the account's own model".to_owned(),
        };
        let line = if offer.allowed.is_empty() {
            format!("- {harness}: leave model out; tasks run {default}\n")
        } else {
            let ids = offer.allowed.join(", ");
            format!("- {harness}: {ids}; leave model out for {default}\n")
        };
        lines.push_str(&line);
    }
    if lines.is_empty() {
        return Ok(lines);
    }
    Ok(format!("{MODELS_HEAD}\n{lines}"))
}

/// Writes the master's list of models a task may name for the harnesses a team plan hands
/// work to (DEC-150 F9), as `master_lines` does. Takes the database, the project and the
/// plan. Returns the text, empty when no harness's model is chosen, or a store or SQLite
/// failure.
pub(crate) fn plan_lines(
    db: &Connection,
    project: &ProjectId,
    plan: &crate::project_setup::TeamPlan,
) -> Result<String, EngineError> {
    let harnesses = super::plan_harnesses(plan);
    let names: Vec<&str> = harnesses.iter().map(String::as_str).collect();
    master_lines(db, project, &names)
}

/// Checks a model a task was pinned to or an agent was started on against the owner's
/// current choices and pick rule, as a new pick would be (MS-1): one the owner since took
/// off the list, or no longer offers, gives way to the agent default, and one the rule
/// overrules does too. The pin and the record stay as history. Takes the database, the
/// project, the harness and the model. Returns the model to start on, none for the
/// account's own, or a store or SQLite failure.
fn still_allowed(
    db: &Connection,
    project: &ProjectId,
    harness: &str,
    model: &str,
) -> Result<Option<String>, EngineError> {
    let catalog = crate::model_choice::catalog(harness);
    Ok(
        match store::pick(db, harness, &catalog, project, Some(model))? {
            Pick::Run(run) => run,
            Pick::Overruled(default) | Pick::Refused { default, .. } => default,
            Pick::Unsupported => None,
        },
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

/// Who runs on the default model after a fallback (DEC-150): a task's agent, an agent
/// that starts again with no task, the master, or an owner-host master whose own account
/// refused the model (M-10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fallen {
    Task(i64),
    Agent,
    Master,
    HostMaster,
}

/// Tells the owner once that no pool account, or an owner-host master's own (M-10), could
/// run a model chosen for a harness, so a start runs on the default (DEC-150): one Pool
/// failover notice (DEC-136a), plus one engine message to a team master but never for an
/// owner-host master, then nothing until the model runs again (`ran`). Takes the
/// transaction, project, harness, model, who falls back and time. Returns whether it told.
pub(crate) fn fell_back(
    tx: &Transaction<'_>,
    project: &ProjectId,
    harness: &str,
    model: &str,
    who: Fallen,
    now: i64,
) -> Result<bool, EngineError> {
    if let Fallen::Task(task) = who {
        assert!(task > 0);
    }
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
    let host = who == Fallen::HostMaster;
    let (whom, who) = match who {
        Fallen::Task(task) => (format!("{harness} agents"), format!("task {task}")),
        Fallen::Agent => (
            format!("{harness} agents"),
            "an agent starting again".to_owned(),
        ),
        Fallen::Master | Fallen::HostMaster => {
            (format!("the {harness} master"), "the master".to_owned())
        }
    };
    let lead = if host {
        format!("The account {whom} runs on refused {model}")
    } else {
        format!("No account in this project's pool can run {model} for {whom} now")
    };
    let text = format!(
        "{lead}, so {who} runs on the default model. You are told again only after {model} runs once more."
    );
    let prefix = format!("model_fallback:{}:{harness}:{now}:", project.as_str());
    let room = crate::notifications::SUBJECT_BYTES_MAX.saturating_sub(prefix.len());
    let cut = model
        .char_indices()
        .map(|(at, one)| at + one.len_utf8())
        .take_while(|end| *end <= room)
        .last()
        .unwrap_or(0);
    let subject = format!("{prefix}{}", &model[..cut]);
    assert!(subject.len() <= crate::notifications::SUBJECT_BYTES_MAX);
    crate::notifications::note(
        tx,
        &crate::notifications::InfoNotice {
            kind: crate::notifications::NoticeKind::PoolFailover,
            project_id: Some(project),
            subject: &subject,
            text: &text,
            target: crate::notifications::NoticeTarget::Conversation {
                project_id: project.clone(),
            },
        },
        now,
    );
    if !host {
        let _ = super::append_message(
            tx,
            project,
            super::MessageAuthor::Engine,
            &format!("{text} (model_fallback)"),
            super::MessageState::Pending,
            now,
        )?;
    }
    Ok(true)
}

/// Forgets a told fallback once a model runs again, so a later fallback is told anew; with
/// none told it only reads, so a start takes no write lock. Takes the database, the
/// project, the harness and the model. Returns a SQLite failure.
pub(crate) fn ran(
    db: &Connection,
    project: &ProjectId,
    harness: &str,
    model: &str,
) -> Result<(), EngineError> {
    if !table(db, "team_model_fallbacks")? {
        return Ok(());
    }
    let told: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM team_model_fallbacks WHERE project=?1 AND harness=?2 AND model=?3)",
            params![project.as_str(), harness, model],
            |row| row.get(0),
        )
        .map_err(super::error)?;
    if !told {
        return Ok(());
    }
    db.execute(
        "DELETE FROM team_model_fallbacks WHERE project=?1 AND harness=?2 AND model=?3",
        params![project.as_str(), harness, model],
    )
    .map_err(super::error)?;
    Ok(())
}

/// Records that an account's harness refused a model for one project (DEC-150 F8), until
/// `MODEL_REFUSAL_MS` from now: the project's expired refusals go, and past `REFUSALS_MAX`
/// the one closest to its end does. Takes the database, outside a transaction, the
/// project, the account, the model and the time. Returns a SQLite failure.
pub(crate) fn refuse(
    db: &Connection,
    project: &ProjectId,
    connection: &CredentialRef,
    model: &str,
    now: i64,
) -> Result<(), EngineError> {
    assert!(!model.is_empty() && model.len() <= crate::model_choice::MODEL_BYTES_MAX);
    let tx = db.unchecked_transaction().map_err(super::error)?;
    tx.execute_batch(RUN_SCHEMA).map_err(super::error)?;
    tx.execute(
        "DELETE FROM team_model_refusals WHERE project=?1 AND until_ms<=?2",
        params![project.as_str(), now],
    )
    .map_err(super::error)?;
    tx.execute(
        "INSERT INTO team_model_refusals(project,connection,model,until_ms) VALUES (?1,?2,?3,?4)
         ON CONFLICT(project,connection,model) DO UPDATE SET until_ms=excluded.until_ms",
        params![
            project.as_str(),
            connection.as_str(),
            model,
            now + MODEL_REFUSAL_MS
        ],
    )
    .map_err(super::error)?;
    tx.execute(
        "DELETE FROM team_model_refusals WHERE project=?1 AND (connection, model) IN (
             SELECT connection, model FROM team_model_refusals WHERE project=?1
             ORDER BY until_ms DESC, connection, model LIMIT -1 OFFSET ?2)",
        params![project.as_str(), REFUSALS_MAX as i64],
    )
    .map_err(super::error)?;
    let kept: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM team_model_refusals WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(super::error)?;
    assert!(kept >= 1 && kept <= REFUSALS_MAX as i64);
    tx.commit().map_err(super::error)
}

/// Tells whether an account's harness refused a model for a project and the refusal still
/// holds (DEC-150 F8). Takes the database, the project, the account, the model and the
/// time. Returns the answer, or a SQLite failure.
pub(crate) fn refused(
    db: &Connection,
    project: &ProjectId,
    connection: &CredentialRef,
    model: &str,
    now: i64,
) -> Result<bool, EngineError> {
    if !table(db, "team_model_refusals")? {
        return Ok(false);
    }
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM team_model_refusals
         WHERE project=?1 AND connection=?2 AND model=?3 AND until_ms>?4)",
        params![project.as_str(), connection.as_str(), model, now],
        |row| row.get(0),
    )
    .map_err(super::error)
}

const CHECK_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS team_model_checks (
    connection TEXT NOT NULL,
    model TEXT NOT NULL,
    checked_ms INTEGER NOT NULL,
    refused INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (connection, model)
) WITHOUT ROWID;";

/// The most live engine checks of said model refusals (DEC-150 addendum 9h): a live claim
/// is never dropped, so past this many a new account and model is held with no launch.
pub(crate) const CHECKS_MAX: usize = 64;

/// What a claim on the engine's own check of a model refusal a pane said answered
/// (DEC-150 addendum 9g, 9h): run the check now, the check already refused at
/// `checked_ms`, or hold, as a live check exists or the table is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Claim {
    Run,
    Refused { checked_ms: i64 },
    Held,
}

/// Claims the engine's check of an account's model, at most one per `MODEL_REFUSAL_MS`
/// (DEC-150 addendum 9g, 9h), in one immediate transaction. Takes the database, outside a
/// transaction, the account, the model and the time. Returns the claim, or a SQLite failure.
pub(crate) fn claim_check(
    db: &Connection,
    connection: &CredentialRef,
    model: &str,
    now: i64,
) -> Result<Claim, EngineError> {
    let _ = (db, connection, model, now, CHECK_SCHEMA);
    Ok(Claim::Held)
}

/// Records that the engine's check of an account's model was refused (DEC-150 addendum
/// 9g): its claim answers refused until it ends, and the project's refusal is recorded
/// from `now`. Takes the database, outside a transaction, the project, the account, the
/// model and the time. Returns a SQLite failure.
pub(crate) fn check_refused(
    db: &Connection,
    project: &ProjectId,
    connection: &CredentialRef,
    model: &str,
    now: i64,
) -> Result<(), EngineError> {
    let _ = (db, project, connection, model, now);
    Ok(())
}

/// Drops a removed project's model records (DEC-150 F6): its tasks' pinned picks, its
/// agents' started models, its told fallbacks and its accounts' model refusals, so a
/// project added again under the same id starts from none. Takes the removal's
/// transaction and the project. Returns a SQLite failure.
pub(crate) fn forget_project(tx: &Transaction<'_>, project: &ProjectId) -> Result<(), EngineError> {
    for name in [
        "team_task_models",
        "team_agent_models",
        "team_model_fallbacks",
        "team_model_refusals",
    ] {
        if table(tx, name)? {
            tx.execute(
                &format!("DELETE FROM {name} WHERE project=?1"),
                [project.as_str()],
            )
            .map_err(super::error)?;
        }
    }
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
