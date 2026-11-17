use super::*;
use crate::landing_log::RULE_REQUEST_SAVED;
use crate::project_setup::removed::event::{KIND, ProjectRemoved};
use crate::state_log::{Aggregate, Appender, Draft};

/// Every logged rule request body, in seq order. Input: the store. Output: the bodies.
fn saved(db: &Connection) -> Vec<serde_json::Value> {
    let mut statement = db
        .prepare("SELECT body FROM state_log WHERE aggregate='landing' AND kind=?1 ORDER BY seq")
        .unwrap();
    statement
        .query_map([RULE_REQUEST_SAVED], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .map(|body| serde_json::from_slice(&body.unwrap()).unwrap())
        .collect()
}

/// The rule request rows as they stand, every column. Input: the store. Output: the rows.
fn requests(db: &Connection) -> Vec<Vec<rusqlite::types::Value>> {
    let mut statement = db
        .prepare("SELECT * FROM github_rule_requests ORDER BY id")
        .unwrap();
    let width = statement.column_count();
    statement
        .query_map([], |row| {
            (0..width)
                .map(|column| row.get::<_, rusqlite::types::Value>(column))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// A logged case: the proposals case with an engine host, so landing writes log.
fn logged_case() -> Case {
    let case = Case::new();
    crate::landing_log::seed(&case.db);
    case
}

/// Checks a logged rule request body is closed: the request id, its project and its row's
/// part digests, no agent words. Input: the body. Output: none.
fn closed(body: &serde_json::Value) {
    let mut keys: Vec<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["project", "request", "row"]);
    assert!(!body.to_string().contains("Reviews keep"), "{body}");
}

/// V250-L9 (QUESTION-1120 A): the request, its Approve, the apply's claim and end and the
/// told mark each log one RuleRequestSaved in the writer's transaction, and the landing
/// rebuild equals the live rows.
#[test]
fn every_rule_request_write_logs_its_row_and_a_rebuild_equals_the_live_rows() {
    let case = logged_case();
    case.done(REPO);
    let question = case.asked(&Asker::Agent(4), GitHubRule::PullRequest, true);
    assert_eq!(saved(&case.db).len(), 1, "the request");
    assert_eq!(case.answer(question, vec![0]), Settlement::Settled);
    assert_eq!(saved(&case.db).len(), 2, "the Approve");
    let mut set = |_: &Path, _: &str, rule: GitHubRule, on: bool| {
        Ok(page(rule, on, GitHubRuleState::Applied))
    };
    assert_eq!(apply_approved(&case.db, &mut set).unwrap(), 1);
    assert_eq!(case.statuses(), ["applied"]);
    let bodies = saved(&case.db);
    assert_eq!(bodies.len(), 5, "claim, end and told");
    bodies.iter().for_each(closed);
    let live = requests(&case.db);
    crate::landing_log::rebuild(&case.db).unwrap();
    assert_eq!(requests(&case.db), live);
}

/// V250-L9 (QUESTION-1120 A): the open's cut-short of an approved request and its told mark
/// are logged through the open's proof, before any host of the store runs again, and the
/// landing rebuild equals the live rows.
#[test]
fn the_opens_cut_short_is_logged_through_the_opening() {
    let case = Case::new();
    let host = crate::landing_log::seed(&case.db);
    case.done(REPO);
    let question = case.asked(&Asker::Agent(4), GitHubRule::PullRequest, true);
    assert_eq!(case.answer(question, vec![0]), Settlement::Settled);
    let before = saved(&case.db).len();
    crate::state_log::fence::host_stopped(&host);
    let opening = crate::storage::opening::Opening::for_test(7);
    assert_eq!(start_store(&case.db, opening).unwrap(), 1);
    assert_eq!(case.statuses(), ["refused"]);
    let bodies = saved(&case.db);
    assert_eq!(bodies.len(), before + 2, "the refusal and the told mark");
    bodies.iter().for_each(closed);
    let live = requests(&case.db);
    crate::landing_log::rebuild(&case.db).unwrap();
    assert_eq!(requests(&case.db), live);
}

/// Logs a project's removal and applies its projection in one transaction, as the engine's
/// Remove from dashboard (kept) or Delete does. Inputs: the case and whether files are kept.
fn remove(case: &Case, kept: bool) {
    let behavior = TransactionBehavior::Immediate;
    let tx = Transaction::new_unchecked(&case.db, behavior).unwrap();
    let fence = crate::worktrees::log::store_fence(&tx).unwrap();
    let mut log = Appender::own(&tx, fence).unwrap();
    let removed = ProjectRemoved {
        project: case.project.clone(),
        kept,
    };
    let body = removed.body().unwrap();
    let draft = Draft {
        aggregate: Aggregate::Projects,
        kind: KIND,
        schema: 1,
        at_ms: 1,
        body: &body,
    };
    assert!(log.append(&draft).unwrap() > 0);
    drop(log);
    crate::project_setup::removed::event::apply(&tx, &body).unwrap();
    tx.commit().unwrap();
}

/// V250-L9 (QUESTION-1120 A): a logged Remove and a logged Delete each forget the project's
/// rule requests at their place in the log, as the projection does, so the rebuild brings
/// none back.
#[test]
fn a_logged_remove_or_delete_forgets_its_rule_requests_on_rebuild() {
    for kept in [true, false] {
        let case = logged_case();
        case.done(REPO);
        case.asked(&Asker::Agent(4), GitHubRule::PullRequest, true);
        assert_eq!(saved(&case.db).len(), 1);
        remove(&case, kept);
        assert!(case.statuses().is_empty(), "kept {kept}");
        crate::landing_log::rebuild(&case.db).unwrap();
        assert!(
            case.statuses().is_empty(),
            "kept {kept}: the rebuild brings none back"
        );
    }
}
