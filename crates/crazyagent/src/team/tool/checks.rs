use super::{Call, REPLY_TEXT_MAX};
use crate::checks::profile::ID_BYTES_MAX;
use crate::checks::queue;
use crate::checks::record::{self, CheckRun};
use crate::checks::{self as engine_checks, Actor, AdmittedCheck, Trigger, settings};
use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::landing::{self, AgentKind, ChangeSetId, ChangeSetView, ChangeState};
use crate::protocol::ProjectId;
use crate::team::bound_text;
use rusqlite::{Connection, Transaction, TransactionBehavior};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use trackwright_core::model::Actor as CoreActor;
use trackwright_core::ops;
use trackwright_core::requests::Request;
use trackwright_core::runs::Verdict;

pub const CHECKS_RUN_TOOL: &str = "checks_run";
pub const CHECKS_RESULTS_TOOL: &str = "checks_results";
/// The checks tools (V3b VE-6) a server lists after `QUESTION_TOOLS`: `checks_run` queues
/// the project's checks on the caller's own change set within the owner's hourly limit, and
/// `checks_results` reads the newest run of each check there, holdout runs outcome only.
pub const CHECKS_TOOLS: [&str; 2] = [CHECKS_RUN_TOOL, CHECKS_RESULTS_TOOL];
/// The most check ids one `checks_run` call names: every check and holdout a profile holds.
pub const CHECKS_NAMED_MAX: usize =
    crate::checks::profile::CHECKS_MAX + crate::checks::profile::HOLDOUTS_MAX;
/// The most failing results `checks_results` lists for one run.
pub const FAILURES_SHOWN_MAX: usize = 20;

const RESULT_TEXT_BYTES_MAX: usize = 300;
const SHOW_REPLY_BYTES_MAX: usize = 16 * 1024;
const MASTER_SESSIONS_MAX: i64 = 64;
const SHORT_COMMIT: usize = 12;
const NO_CHANGE_SET: &str = "Not done: you have no change set in this project yet. Checks run on the changes your turns committed, so make and keep a change first.";
const NO_PROFILE: &str = "Not run: this project has no approved check profile. The owner approves one in Project Settings > Checks.";
const REQUESTS_OFF: &str = "Not run: the owner turned agent check requests off for this project. The owner's own runs and the turn-end checks still run.";
const UNKNOWN_CHECK: &str = "Not run: name checks of the project's current profile, or leave checks out for every check it runs on request.";
const NOT_ON_REQUEST: &str = "Not run: a check you named runs only at turn end or before the owner accepts, not on request. Leave checks out to ask for every check you may request, or name only those.";
const NOTHING_TO_RUN: &str = "Not run: every check you named already waits or runs on your current changes, or the profile runs no check on request. Call checks_results once they finish.";
const QUEUE_FULL: &str =
    "Not run: the project's check queue is full. Try again once some runs finish.";
const CLOSED: &str =
    "Not run: your change set was accepted or discarded. Checks run on your next change set.";
const NOT_QUEUED: &str = "Not run: the engine could not queue the checks. Try again in a moment.";
const NOT_READ: &str = "Not read: the engine could not read the check runs. Try again in a moment.";
const READ_FIRST: &str =
    "Read the failing results above and fix them before you ask for another run.";

const _: () = assert!(CHECKS_NAMED_MAX == 24);
const _: () = assert!(FAILURES_SHOWN_MAX * (2 * RESULT_TEXT_BYTES_MAX + 64) < REPLY_TEXT_MAX);
const _: () = assert!(SHOW_REPLY_BYTES_MAX >= trackwright_core::reply::REPLY_BYTES_DEFAULT);
const _: () = assert!(SHOW_REPLY_BYTES_MAX <= trackwright_core::reply::REPLY_BYTES_MAX);

/// The `checks_run` tool's listing. Output: its name, description and input schema.
pub(super) fn run_listing() -> Value {
    json!({
        "name": CHECKS_RUN_TOOL,
        "description": "Ask the engine to run the project's checks on your own change set: the changes your turns committed, never another agent's. Leave checks out for every check the owner's profile runs on request, or name some. The runs go on in the background and each request counts against your hourly limit, so read checks_results before you ask again.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "checks": {"type": "array", "items": {"type": "string"}, "maxItems": CHECKS_NAMED_MAX, "description": "Check ids from the project's profile, such as unit or lint; left out, every check it runs on request."},
            },
            "additionalProperties": false,
        },
    })
}

/// The `checks_results` tool's listing. Output: its name, description and input schema.
pub(super) fn results_listing() -> Value {
    json!({
        "name": CHECKS_RESULTS_TOOL,
        "description": "Read the newest run of each check on your own change set: its outcome and the tests that failed, with their messages where the owner lets you see them. Holdout checks show their outcome only.",
        "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
    })
}

/// Reads one `checks_run` call: at most `CHECKS_NAMED_MAX` distinct check ids, none for
/// every check the profile runs on request. Input: the arguments. Output: the call, or none
/// when a key is unknown or an id is not a plain id of up to `ID_BYTES_MAX` bytes.
pub(super) fn run_call(arguments: Option<&Value>) -> Option<Call> {
    let arguments = match arguments {
        None | Some(Value::Null) => return Some(Call::ChecksRun { checks: Vec::new() }),
        Some(arguments) => arguments.as_object()?,
    };
    if arguments.keys().any(|key| key != "checks") {
        return None;
    }
    let checks: Vec<String> = match arguments.get("checks") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) if items.len() <= CHECKS_NAMED_MAX => items
            .iter()
            .map(|item| item.as_str().filter(|id| check_id(id)).map(str::to_owned))
            .collect::<Option<_>>()?,
        Some(_) => return None,
    };
    let distinct: BTreeSet<&String> = checks.iter().collect();
    if distinct.len() != checks.len() {
        return None;
    }
    assert!(checks.len() <= CHECKS_NAMED_MAX);
    Some(Call::ChecksRun { checks })
}

/// Whether a text is a check id as a profile writes one: 1 to `ID_BYTES_MAX` ASCII
/// letters, digits, `-` or `_`. Input: the text. Output: the answer.
fn check_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= ID_BYTES_MAX
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
}

/// Reads one `checks_results` call, which takes no arguments. Input: the arguments.
/// Output: the call, or none when any argument is given.
pub(super) fn results_call(arguments: Option<&Value>) -> Option<Call> {
    match arguments {
        None | Some(Value::Null) => Some(Call::ChecksResults),
        Some(Value::Object(arguments)) if arguments.is_empty() => Some(Call::ChecksResults),
        Some(_) => None,
    }
}

/// What a checks call answered: the sentence the caller reads, whether it is an error, and
/// for a queued run the change set whose runners the engine starts next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answered {
    pub text: String,
    pub error: bool,
    pub queued: Option<ChangeSetId>,
}

/// Answers a master's or sub-agent's checks call and starts the runners of a run it
/// queued. Inputs and output: as `answer`, without the queued change set.
pub(in crate::team) fn serve(
    db: &Connection,
    project: &ProjectId,
    caller: Actor,
    call: Call,
) -> (String, bool) {
    let answered = answer(db, project, caller, call);
    if let Some(queued) = &answered.queued {
        queue::start(db, queued);
    }
    (answered.text, answered.error)
}

/// Answers a master's or sub-agent's checks call on its own change set in the project: the
/// newest one whose agent is the caller, never one the call names. A holdout check is never
/// named to the caller, and an id it names that only a holdout has is refused as unknown.
/// Inputs: the engine database, the project, the caller as the engine named it and the
/// call, `checks_run` or `checks_results`. Output: the answer.
pub(in crate::team) fn answer(
    db: &Connection,
    project: &ProjectId,
    caller: Actor,
    call: Call,
) -> Answered {
    assert!(caller != Actor::Owner, "the owner uses the owner protocol");
    let own = match own_change_set(db, project, caller) {
        Ok(Some(own)) => own,
        Ok(None) => return refused(NO_CHANGE_SET),
        Err(_) => return refused(NOT_READ),
    };
    match call {
        Call::ChecksRun { checks } => run(db, project, caller, &own, &checks),
        Call::ChecksResults => results(db, caller, &own),
        other => unreachable!("{} is not a checks tool", other.name()),
    }
}

fn refused(text: &str) -> Answered {
    Answered {
        text: text.to_owned(),
        error: true,
        queued: None,
    }
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(SHORT_COMMIT)]
}

/// The caller's newest change set in the project, open ones first as `landing::list`
/// orders them: a sub-agent's or a team master's by its team agent id, and for a master
/// also those of the master sessions whose conversation it continues. Inputs: the database,
/// the project and the caller. Output: the change set, none, or a store failure.
fn own_change_set(
    db: &Connection,
    project: &ProjectId,
    caller: Actor,
) -> Result<Option<ChangeSetView>, EngineError> {
    let (agent, sessions) = match caller {
        Actor::Agent(agent) => (agent, BTreeSet::new()),
        Actor::Master(agent) => (agent, master_sessions(db, project, agent)),
        Actor::Owner => unreachable!("the owner has no team agent id"),
    };
    let agent = agent.to_string();
    Ok(landing::list(db, project)?
        .into_iter()
        .find(|set| match set.agent.kind {
            AgentKind::Sub => set.agent.id == agent,
            AgentKind::Master => sessions.contains(&set.agent.id),
        }))
}

/// The master sessions of the project whose harness conversation a team master continues,
/// at most `MASTER_SESSIONS_MAX`; none when the engine keeps no such table, so a master
/// then reaches only its own team change sets. Inputs: the database, the project and the
/// master's team agent id. Output: the session ids.
fn master_sessions(db: &Connection, project: &ProjectId, agent: i64) -> BTreeSet<String> {
    let read = || -> rusqlite::Result<BTreeSet<String>> {
        db.prepare(
            "SELECT session FROM team_master_conversations WHERE project=?1 AND agent=?2
             ORDER BY session LIMIT ?3",
        )?
        .query_map(
            rusqlite::params![project.as_str(), agent, MASTER_SESSIONS_MAX],
            |row| row.get(0),
        )?
        .collect()
    };
    read().unwrap_or_default()
}

/// Queues the named checks, or every on-request check, on the caller's own open change set
/// as the caller. Inputs: the database, the project, the caller, its change set and the
/// check ids. Output: the answer, with the change set to start for a queued batch.
fn run(
    db: &Connection,
    project: &ProjectId,
    caller: Actor,
    own: &ChangeSetView,
    checks: &[String],
) -> Answered {
    if !matches!(
        own.state,
        ChangeState::Pending | ChangeState::Conflict | ChangeState::Updating
    ) {
        return refused(CLOSED);
    }
    let admitted: Vec<AdmittedCheck> = match engine_checks::current_profile(db, project) {
        Ok(Some(profile)) => profile.checks(),
        Ok(None) => return refused(NO_PROFILE),
        Err(_) => return refused(NOT_QUEUED),
    };
    let holdout = |id: &str| {
        admitted
            .iter()
            .any(|check| check.spec.id == id && check.holdout.is_some())
    };
    let named = |id: &String| {
        admitted
            .iter()
            .any(|check| check.spec.id == *id && check.holdout.is_none())
    };
    if !checks.iter().all(named) {
        return refused(UNKNOWN_CHECK);
    }
    let batch = match queue::submit(db, &own.id, Trigger::OnRequest, caller, checks) {
        Ok(batch) => batch,
        Err(problem) => {
            return match problem.0.as_str() {
                "checks_rate_limited" => {
                    let allowed = settings::effective(db, project)
                        .map_or(0, |settings| settings.agent_requests);
                    refused(&format!(
                        "Not run: each agent may ask for {allowed} check requests per hour in this project and you asked for {allowed} in the last hour. Read checks_results, fix what failed, and ask again later."
                    ))
                }
                "checks_requests_off" => refused(REQUESTS_OFF),
                "checks_no_profile" => refused(NO_PROFILE),
                "checks_check_unknown" => refused(UNKNOWN_CHECK),
                "checks_not_on_request" => refused(NOT_ON_REQUEST),
                "checks_nothing_to_run" => refused(NOTHING_TO_RUN),
                "checks_queue_full" => refused(QUEUE_FULL),
                "checks_change_set_discarded"
                | "checks_change_set_accepted"
                | "checks_change_set_unknown" => refused(CLOSED),
                _ => refused(NOT_QUEUED),
            };
        }
    };
    let queued: Vec<String> = queue::jobs(db, &own.id)
        .unwrap_or_default()
        .into_iter()
        .filter(|job| job.batch == batch)
        .map(|job| job.check)
        .collect();
    let hidden = queued.iter().filter(|id| holdout(id)).count();
    let shown: Vec<&str> = queued
        .iter()
        .filter(|id| !holdout(id))
        .map(String::as_str)
        .collect();
    let mut text = format!(
        "Queued {} check{} on your change set at {}",
        queued.len(),
        if queued.len() == 1 { "" } else { "s" },
        short(&own.head)
    );
    if !shown.is_empty() {
        text.push_str(&format!(": {}", shown.join(", ")));
    }
    if hidden > 0 {
        text.push_str(&format!(
            "{}{hidden} holdout check{}",
            if shown.is_empty() { ": " } else { " and " },
            if hidden == 1 { "" } else { "s" }
        ));
    }
    text.push_str(". They run in the background; call checks_results once they finish.");
    Answered {
        text: bound_text(&text, REPLY_TEXT_MAX),
        error: false,
        queued: Some(own.id.clone()),
    }
}

/// What one run came to: its verdict, or its state while it has none. Input: the run.
/// Output: the word.
fn outcome(run: &CheckRun) -> &'static str {
    run.verdict.map_or(run.state.as_str(), Verdict::as_str)
}

/// Reads the newest run of each check on the caller's own change set. A holdout run, or a
/// run whose integrity record the agent may not rely on, shows its outcome only, and a
/// holdout's id and commit are never named; another run lists its failing results through
/// the tracker's `run_show` as the caller, which withholds a protected case's message.
/// Inputs: the database, the caller and its change set. Output: the answer.
fn results(db: &Connection, caller: Actor, own: &ChangeSetView) -> Answered {
    let runs = match record::latest(db, &own.id) {
        Ok(runs) => runs,
        Err(_) => return refused(NOT_READ),
    };
    if runs.is_empty() {
        return Answered {
            text: format!(
                "No check has finished on your change set at {} yet. Ask for a run with checks_run, or wait for the turn-end checks.",
                short(&own.head)
            ),
            error: false,
            queued: None,
        };
    }
    let actor = CoreActor::new(&caller.name()).expect("a team actor name is a tracker actor");
    let mut lines = vec![format!(
        "Newest check runs on your change set (head {}):",
        short(&own.head)
    )];
    let mut holdouts = Vec::new();
    let mut failed = false;
    for run in &runs {
        failed |= run
            .verdict
            .is_some_and(|verdict| verdict != Verdict::Passed);
        if run.holdout {
            holdouts.push(outcome(run));
            continue;
        }
        if !agent_sees_all(db, run.run) {
            lines.push(format!(
                "- {}: {} (outcome only: the run has no integrity record)",
                run.check,
                outcome(run)
            ));
            continue;
        }
        lines.push(format!(
            "- {}: {} on {}",
            run.check,
            outcome(run),
            short(&run.commit)
        ));
        if run.verdict == Some(Verdict::Passed) {
            continue;
        }
        match failing(db, &actor, run) {
            Ok(found) => lines.extend(found),
            Err(_) => lines.push("  (the failing results could not be read)".to_owned()),
        }
    }
    if !holdouts.is_empty() {
        lines.push(format!(
            "- {} holdout check{}: {} (outcome only)",
            holdouts.len(),
            if holdouts.len() == 1 { "" } else { "s" },
            holdouts.join(", ")
        ));
    }
    if failed {
        lines.push(READ_FIRST.to_owned());
    }
    Answered {
        text: bound_text(&lines.join("\n"), REPLY_TEXT_MAX),
        error: false,
        queued: None,
    }
}

/// Whether an agent may see a whole run, as the integrity record says. Inputs: the
/// database and the `tk_runs` row. Output: true only for a run the record shows in full;
/// false on any failure to read it.
#[cfg(unix)]
fn agent_sees_all(db: &Connection, run: i64) -> bool {
    use crate::checks::integrity::{self, Reader, RunSight};
    matches!(integrity::sight(db, run, Reader::Agent), Ok(RunSight::Full))
}

/// Off Unix no integrity record is kept, so an agent sees outcomes only.
#[cfg(not(unix))]
fn agent_sees_all(_: &Connection, _: i64) -> bool {
    false
}

/// The failing results of one run as the caller may read them: the first page of the
/// tracker's `run_show` as that actor, at most `FAILURES_SHOWN_MAX` names, each with its
/// message on one line unless the tracker withheld it. Inputs: the database, the caller's
/// tracker actor and the run. Output: the lines, or the tracker's refusal.
fn failing(db: &Connection, actor: &CoreActor, run: &CheckRun) -> Result<Vec<String>, EngineError> {
    let unread = || failure("checks_unavailable");
    let args = serde_json::to_vec(&json!({"id": run.id.to_string()})).map_err(|_| unread())?;
    let request = Request::parse("run_show", &args)
        .map_err(|_| unread())?
        .with_reply_bytes_max(SHOW_REPLY_BYTES_MAX);
    let reply = {
        let tx =
            Transaction::new_unchecked(db, TransactionBehavior::Deferred).map_err(|_| unread())?;
        ops::dispatch(&tx, &crate::tracker::Host, actor, request)
    };
    let envelope: Value = serde_json::from_str(reply.json()).map_err(|_| unread())?;
    if envelope["ok"] != json!(true) {
        return Err(unread());
    }
    let rows = envelope["data"]["results"].as_array().ok_or_else(unread)?;
    let failed: Vec<&Value> = rows
        .iter()
        .filter(|row| matches!(row["outcome"].as_str(), Some("failed" | "errored")))
        .collect();
    let one_line = |text: &str| {
        let flat: String = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        bound_text(flat.trim(), RESULT_TEXT_BYTES_MAX)
    };
    let mut lines = Vec::new();
    for row in failed.iter().take(FAILURES_SHOWN_MAX) {
        let name = one_line(row["name"].as_str().unwrap_or("unnamed"));
        let verb = row["outcome"].as_str().unwrap_or("failed");
        lines.push(match row["message"].as_str() {
            _ if row["withheld"] == json!(true) => {
                format!("  {verb}: {name} (message withheld: a protected case)")
            }
            Some(message) => format!("  {verb}: {name}: {}", one_line(message)),
            None => format!("  {verb}: {name}"),
        });
    }
    if failed.len() > FAILURES_SHOWN_MAX {
        lines.push(format!(
            "  and {} more not shown",
            failed.len() - FAILURES_SHOWN_MAX
        ));
    }
    if !envelope["cursor"].is_null() {
        lines.push("  (more results than one page; the owner sees them all)".to_owned());
    }
    assert!(lines.len() <= FAILURES_SHOWN_MAX + 2);
    Ok(lines)
}

#[cfg(all(test, target_os = "macos"))]
#[path = "checks/tests.rs"]
mod tests;
