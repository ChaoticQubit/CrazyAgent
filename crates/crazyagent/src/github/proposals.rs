use super::{GitHubPublishStep, GitHubRepoRules, GitHubRule, GitHubRuleState, state};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::questions::{Ask, Asker, EngineSource, QuestionKind};
use crate::team::{MessageAuthor, MessageState};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

#[cfg(all(test, target_os = "macos"))]
mod proposals_tests;

/// The asker name prefix of the questions agents' rule proposals raise.
pub const ASKER_PREFIX: &str = "engine:github-rule:";
/// The option an owner picks to approve a proposal, always the first.
pub const APPROVE: &str = "Approve";
/// The option an owner picks to decline a proposal.
pub const DECLINE: &str = "Decline";
/// The longest reason an agent may give with a proposal; a longer one is cut.
pub const WHY_BYTES_MAX: usize = 300;
/// The most proposals one project has open at once: waiting for the owner or being applied.
pub const OPEN_PER_PROJECT_MAX: i64 = 4;
/// The most proposals one pass applies or tells, so a pass stays bounded.
pub const PASS_MAX: i64 = 16;

const UNAVAILABLE: &str = "github_rule_requests_unavailable";
const TABLE: &str = "CREATE TABLE IF NOT EXISTS github_rule_requests (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project TEXT NOT NULL,
    asker TEXT NOT NULL,
    rule TEXT NOT NULL,
    turn_on INTEGER NOT NULL CHECK(turn_on IN (0,1)),
    why TEXT NOT NULL,
    repo TEXT NOT NULL,
    question INTEGER,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','approved','applying','applied','refused','declined')),
    told INTEGER NOT NULL DEFAULT 0 CHECK(told IN (0,1)),
    created_ms INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS github_rule_requests_open
    ON github_rule_requests(project, asker, rule) WHERE status IN ('pending','approved','applying');";
const OPEN: &str = "status IN ('pending','approved','applying')";
const ENDED: &str = "status IN ('applied','refused','declined')";

const _: () = assert!(OPEN_PER_PROJECT_MAX <= PASS_MAX);
const _: () = assert!(WHY_BYTES_MAX <= crate::questions::CONTEXT_BYTES_MAX / 4);

/// How a proposal ended for the agent that made it: applied after the owner approved it,
/// refused when it could not be applied as approved, or declined by the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Applied,
    Refused,
    Declined,
}

impl Ended {
    fn status(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Refused => "refused",
            Self::Declined => "declined",
        }
    }

    fn words(self) -> &'static str {
        match self {
            Self::Applied => "approved and applied",
            Self::Refused => "refused",
            Self::Declined => "declined",
        }
    }
}

/// One proposal as the engine keeps it, read back for applying and telling.
struct Row {
    id: i64,
    project: ProjectId,
    asker: String,
    rule: GitHubRule,
    on: bool,
    repo: String,
}

/// Tells whether an agent may propose a rule: one GitHub's settings hold. Dependabot
/// version updates and the security policy are files in the project, so they are not.
/// Input: the rule. Output: the answer.
pub fn proposable(rule: GitHubRule) -> bool {
    rule.on_github()
}

/// The rule's name as agents and the owner read it, its snake_case name. Input: the rule.
/// Output: the name.
pub fn rule_name(rule: GitHubRule) -> String {
    match serde_json::to_value(rule) {
        Ok(serde_json::Value::String(name)) => name,
        _ => unreachable!("a GitHub rule is named by a string"),
    }
}

/// Turns an agent's proposal into a real owner question (DEC-182 S7). The repository is
/// the one the asker's own project was published to, read from its local publish record
/// at Done with no sign-in and no GitHub call, and it is kept with the proposal so an
/// Approve applies to it alone. The checks, the proposal and its question are written in
/// one transaction, so a failure leaves neither (RV gh-s7 F1). The question's words come from the engine; the agent's
/// reason is shown only as its own quoted words. Inputs: the engine connection, the
/// project, who asked, the rule, whether it is to be on, and the reason. Output: the
/// question id, or `github_rule_asker_invalid`, `github_rule_invalid`,
/// `github_rule_why_invalid`, `github_rules_not_published`, `github_rule_request_open`,
/// `github_rule_requests_full` or `github_rule_requests_unavailable`.
pub fn request(
    db: &Connection,
    project: &ProjectId,
    asker: &Asker,
    rule: GitHubRule,
    on: bool,
    why: &str,
) -> Result<u64, String> {
    if !matches!(asker, Asker::Master(id) | Asker::Agent(id) if *id > 0) {
        return Err("github_rule_asker_invalid".into());
    }
    if !proposable(rule) {
        return Err("github_rule_invalid".into());
    }
    let why = plain_why(why).ok_or("github_rule_why_invalid")?;
    let root = crate::sandbox::grants::data_root(db).ok_or(UNAVAILABLE)?;
    let folder = folder(db, project).map_err(|problem| problem.0)?;
    let repo = published(&root, &folder)
        .map_err(|_| UNAVAILABLE)?
        .ok_or("github_rules_not_published")?;
    let tx =
        Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(|_| UNAVAILABLE)?;
    table(&tx).map_err(|_| UNAVAILABLE)?;
    let counted = |sql: &str, values: &[&dyn rusqlite::ToSql]| -> Result<i64, String> {
        tx.query_row(sql, values, |row| row.get(0))
            .map_err(|_| UNAVAILABLE.to_owned())
    };
    let name = rule_name(rule);
    let mine = counted(
        &format!(
            "SELECT COUNT(*) FROM github_rule_requests WHERE project=?1 AND asker=?2 AND rule=?3 AND {OPEN}"
        ),
        &[&project.as_str(), &asker.name(), &name],
    )?;
    if mine > 0 {
        return Err("github_rule_request_open".into());
    }
    let open = counted(
        &format!("SELECT COUNT(*) FROM github_rule_requests WHERE project=?1 AND {OPEN}"),
        &[&project.as_str()],
    )?;
    if open >= OPEN_PER_PROJECT_MAX {
        return Err("github_rule_requests_full".into());
    }
    tx.execute(
        "INSERT INTO github_rule_requests(project,asker,rule,turn_on,why,repo,created_ms) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![project.as_str(), asker.name(), name, on, why, repo, crate::team::now_ms()],
    )
    .map_err(|_| UNAVAILABLE)?;
    let id = tx.last_insert_rowid();
    assert!(id > 0);
    let switch = if on { "on" } else { "off" };
    let ask = Ask {
        question: format!(
            "{} proposes turning the GitHub rule {name} {switch} for {repo}. Approve it?",
            capital(&crate::questions::asker_label(&asker.name()))
        ),
        kind: QuestionKind::Single,
        options: vec![APPROVE.to_owned(), DECLINE.to_owned()],
        recommended: Some(if on { APPROVE } else { DECLINE }.to_owned()),
        context: Some(format!(
            "Its reason, in its own words: {}. Approving changes this one rule on {repo} at once with your GitHub sign-in; you can change it back in the project's GitHub settings. The agent learns only whether it was applied, refused or declined.",
            serde_json::to_string(&why).map_err(|_| UNAVAILABLE)?
        )),
    };
    let source = EngineSource::GitHubRule(id.to_string());
    let question =
        crate::questions::raise_in(&tx, project, &source, &ask).map_err(|problem| problem.0)?;
    let kept = tx
        .execute(
            "UPDATE github_rule_requests SET question=?2 WHERE id=?1",
            params![id, i64::try_from(question).unwrap_or(i64::MAX)],
        )
        .map_err(|_| UNAVAILABLE)?;
    assert_eq!(kept, 1);
    tx.commit().map_err(|_| UNAVAILABLE)?;
    crate::questions::refresh_page(db, project);
    Ok(question)
}

/// Settles the proposal behind an engine question the owner answered or dismissed, in the
/// caller's transaction: an Approve marks it approved for [`apply_approved`], which runs
/// after the answer is recorded; anything else declines it and tells the agent. A question
/// of another asker is left alone. Inputs: the transaction, the project, the question's
/// asker and whether the owner approved. Output: done, or the store error.
pub(crate) fn decide(
    tx: &Transaction<'_>,
    project: &ProjectId,
    asker: &str,
    approved: bool,
) -> Result<(), EngineError> {
    let Some(id) = request_id(asker) else {
        return Ok(());
    };
    table(tx).map_err(|_| failure(UNAVAILABLE))?;
    let status = if approved { "approved" } else { "declined" };
    let changed = tx
        .execute(
            "UPDATE github_rule_requests SET status=?3 WHERE id=?1 AND project=?2 AND status='pending'",
            params![id, project.as_str(), status],
        )
        .map_err(|_| failure(UNAVAILABLE))?;
    assert!(changed <= 1);
    if changed == 1 && !approved {
        tell_untold(tx)?;
    }
    Ok(())
}

/// Held by one apply pass at a time in the engine (rv-code gh-s7 N2): two answers that
/// settle together each start one, and a rule change reads the repository before it
/// writes the repository's record, so two at once could lose one's record.
static APPLYING: Mutex<()> = Mutex::new(());

/// Applies every approved proposal, each with the rule, the on value and the repository
/// kept at ask time, and tells each agent how it ended. A proposal is claimed before its
/// change is sent, so two passes never send one twice, and no transaction is open while
/// GitHub answers. One whose project no longer has that repository published at Done is
/// refused with nothing sent. Inputs: the engine connection and the rule change, which
/// runs with the owner's sign-in. Output: how many proposals ended, or the store error.
pub fn apply_approved(
    db: &Connection,
    set: &mut dyn FnMut(&Path, &str, GitHubRule, bool) -> Result<GitHubRepoRules, EngineError>,
) -> Result<usize, EngineError> {
    let _one = APPLYING.lock().unwrap_or_else(PoisonError::into_inner);
    table(db).map_err(|_| failure(UNAVAILABLE))?;
    let root = crate::sandbox::grants::data_root(db).ok_or_else(|| failure(UNAVAILABLE))?;
    let approved = rows(db, "status='approved'")?;
    assert!(approved.len() as i64 <= PASS_MAX);
    let mut ended = 0;
    for row in approved {
        let claimed = db
            .execute(
                "UPDATE github_rule_requests SET status='applying' WHERE id=?1 AND status='approved'",
                [row.id],
            )
            .map_err(|_| failure(UNAVAILABLE))?;
        if claimed == 0 {
            continue;
        }
        let outcome = applied(&root, db, &row, set);
        db.execute(
            "UPDATE github_rule_requests SET status=?2 WHERE id=?1 AND status='applying'",
            params![row.id, outcome.status()],
        )
        .map_err(|_| failure(UNAVAILABLE))?;
        ended += 1;
    }
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)
        .map_err(|_| failure(UNAVAILABLE))?;
    tell_untold(&tx)?;
    tx.commit().map_err(|_| failure(UNAVAILABLE))?;
    Ok(ended)
}

/// Ends every proposal an engine stop left approved or being applied as refused and tells
/// its agent, as the engine starts. Nothing is sent: the owner may have changed that rule
/// on the GitHub settings page since. A pending proposal whose question is no longer
/// pending, or was never kept, ends refused too, so it holds no slot and never blocks its
/// agent and rule (RV gh-s7 F1). Input: the start's transaction. Output: how many ended,
/// or the store error.
pub(crate) fn start(tx: &Transaction<'_>) -> Result<usize, EngineError> {
    table(tx).map_err(|_| failure(UNAVAILABLE))?;
    let stopped = tx
        .execute(
            "UPDATE github_rule_requests SET status='refused' WHERE status IN ('approved','applying')",
            [],
        )
        .map_err(|_| failure(UNAVAILABLE))?;
    let pending: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM github_rule_requests WHERE status='pending'",
            [],
            |row| row.get(0),
        )
        .map_err(|_| failure(UNAVAILABLE))?;
    let orphaned = if pending == 0 {
        0
    } else {
        tx.execute(
            "UPDATE github_rule_requests SET status='refused' WHERE status='pending' AND (question IS NULL OR NOT EXISTS (SELECT 1 FROM questions_log WHERE questions_log.id=github_rule_requests.question AND questions_log.status='pending'))",
            [],
        )
        .map_err(|_| failure(UNAVAILABLE))?
    };
    tell_untold(tx)?;
    Ok(stopped + orphaned)
}

/// Runs [`start`] in its own transaction as the engine's store opens. Input: the store's
/// connection. Output: how many proposals ended, or the store error.
pub(crate) fn start_store(db: &Connection) -> Result<usize, EngineError> {
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)
        .map_err(|_| failure(UNAVAILABLE))?;
    let ended = start(&tx)?;
    tx.commit().map_err(|_| failure(UNAVAILABLE))?;
    Ok(ended)
}

/// Forgets a removed project's proposals inside the removal's transaction, so none is
/// applied or told after it. Inputs: the transaction and the project. Output: done, or
/// `github_rule_requests_unavailable`.
pub(crate) fn forget_project(tx: &Transaction<'_>, project: &ProjectId) -> Result<(), EngineError> {
    table(tx).map_err(|_| failure(UNAVAILABLE))?;
    tx.execute(
        "DELETE FROM github_rule_requests WHERE project=?1",
        [project.as_str()],
    )
    .map_err(|_| failure(UNAVAILABLE))?;
    Ok(())
}

/// Reads the proposal id from a rule proposal question's asker name. Input: the stored
/// asker. Output: the id, or none for any other asker.
pub fn request_id(asker: &str) -> Option<i64> {
    asker
        .strip_prefix(ASKER_PREFIX)
        .and_then(|id| id.parse::<i64>().ok())
        .filter(|id| *id > 0)
}

/// Tells whether the owner's answer approves a proposal: the first option picked alone,
/// whatever comment comes with it. Input: the picks. Output: the answer.
pub fn approves(picks: &[u8]) -> bool {
    picks == [0]
}

/// Applies one claimed proposal and says how it ended: refused, with nothing sent, when
/// the project's publish record no longer names the repository kept at ask time at Done;
/// otherwise applied only when GitHub's fresh page shows the rule as wanted and applied.
fn applied(
    root: &Path,
    db: &Connection,
    row: &Row,
    set: &mut dyn FnMut(&Path, &str, GitHubRule, bool) -> Result<GitHubRepoRules, EngineError>,
) -> Ended {
    let Ok(folder) = folder(db, &row.project) else {
        return Ended::Refused;
    };
    if published(root, &folder).ok().flatten().as_deref() != Some(row.repo.as_str()) {
        return Ended::Refused;
    }
    match set(&folder, &row.repo, row.rule, row.on) {
        Ok(page) => {
            let done = page.rules.iter().any(|outcome| {
                outcome.rule == row.rule
                    && outcome.on == row.on
                    && outcome.state == GitHubRuleState::Applied
            });
            if done { Ended::Applied } else { Ended::Refused }
        }
        Err(problem) => {
            eprintln!(
                "github_rule_proposal_refused id={} code={}",
                row.id, problem.0
            );
            Ended::Refused
        }
    }
}

/// Tells the agents of up to `PASS_MAX` ended proposals how each ended, as one engine
/// message each in the project's team conversation. A message the full conversation
/// refuses stays untold for the next pass. Nothing about GitHub's answer, the repository
/// or the sign-in is in it.
fn tell_untold(tx: &Transaction<'_>) -> Result<(), EngineError> {
    let untold = rows(tx, &format!("{ENDED} AND told=0"))?;
    for row in untold {
        let status: String = tx
            .query_row(
                "SELECT status FROM github_rule_requests WHERE id=?1",
                [row.id],
                |found| found.get(0),
            )
            .map_err(|_| failure(UNAVAILABLE))?;
        let ended = match status.as_str() {
            "applied" => Ended::Applied,
            "declined" => Ended::Declined,
            _ => Ended::Refused,
        };
        let text = told(&row, ended);
        let appended = crate::team::append_message(
            tx,
            &row.project,
            MessageAuthor::Engine,
            &text,
            MessageState::Pending,
            crate::team::now_ms(),
        )?;
        if appended.is_err() {
            eprintln!("github_rule_outcome_waiting id={}", row.id);
            continue;
        }
        tx.execute(
            "UPDATE github_rule_requests SET told=1 WHERE id=?1",
            [row.id],
        )
        .map_err(|_| failure(UNAVAILABLE))?;
    }
    Ok(())
}

/// Words how a proposal ended for the agent that made it: the proposal, the asker, the
/// rule and its wanted value, and the outcome alone.
fn told(row: &Row, ended: Ended) -> String {
    let switch = if row.on { "on" } else { "off" };
    format!(
        "GitHub rule proposal {} from {} ({} {switch}): {}.",
        row.id,
        crate::questions::asker_label(&row.asker),
        rule_name(row.rule),
        ended.words()
    )
}

fn rows(db: &Connection, filter: &str) -> Result<Vec<Row>, EngineError> {
    let mut statement = db
        .prepare(&format!(
            "SELECT id,project,asker,rule,turn_on,repo FROM github_rule_requests WHERE {filter} ORDER BY id LIMIT {PASS_MAX}"
        ))
        .map_err(|_| failure(UNAVAILABLE))?;
    let read = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(|_| failure(UNAVAILABLE))?;
    let mut found = Vec::new();
    for item in read {
        let (id, project, asker, rule, on, repo) = item.map_err(|_| failure(UNAVAILABLE))?;
        let project = ProjectId::new(&project).map_err(|_| failure(UNAVAILABLE))?;
        let rule: GitHubRule = serde_json::from_value(serde_json::Value::String(rule))
            .map_err(|_| failure(UNAVAILABLE))?;
        found.push(Row {
            id,
            project,
            asker,
            rule,
            on,
            repo,
        });
    }
    assert!(found.len() as i64 <= PASS_MAX);
    Ok(found)
}

/// The repository a project's publish made, from its local record at Done, or none
/// before that. No sign-in is read and nothing is sent.
fn published(root: &Path, folder: &Path) -> Result<Option<String>, EngineError> {
    Ok(
        state::load_job(root, folder)?.and_then(|job| match job.step {
            GitHubPublishStep::Done { full_name, .. } => Some(full_name),
            _ => None,
        }),
    )
}

/// The project's folder from its stored setup, never a path from the agent.
fn folder(db: &Connection, project: &ProjectId) -> Result<PathBuf, EngineError> {
    let folder = crate::project_setup::load_setup(db, project)
        .map_err(|_| failure(UNAVAILABLE))?
        .ok_or_else(|| failure("github_rules_not_published"))?
        .project_folder();
    if !folder.is_absolute() {
        return Err(failure("github_rules_not_published"));
    }
    Ok(folder)
}

/// The agent's reason as plain words: trimmed, cut to `WHY_BYTES_MAX` bytes at a character
/// boundary, or none when it is empty or holds a control or hidden character, such as a
/// direction override or a zero-width space, so the owner reads only what shows. Input:
/// the reason. Output: the plain reason, if any.
pub fn plain_why(why: &str) -> Option<String> {
    let trimmed = why.trim();
    let hidden = |c: char| c.is_control() || crate::memory::hidden_char(c);
    if trimmed.is_empty() || trimmed.chars().any(hidden) {
        return None;
    }
    let mut end = trimmed.len().min(WHY_BYTES_MAX);
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    let cut = trimmed[..end].trim_end().to_owned();
    assert!(cut.len() <= WHY_BYTES_MAX);
    (!cut.is_empty()).then_some(cut)
}

fn capital(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

fn table(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(TABLE)
}

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}
