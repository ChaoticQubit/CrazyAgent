use super::*;
use crate::landing_log::{
    AGENT_BRANCH_FORGOTTEN, AGENT_BRANCH_SET, BASE_SET, OWNER_EDIT_KEPT, OWNER_EDITS_DROPPED,
    OWNER_EDITS_FORGOTTEN, OWNER_EDITS_SETTLED, WORKTREE_OWNER_EDITS_FORGOTTEN, rebuild, seed,
};
use crate::state_log::replay::rows;
use serde_json::json;

const BRANCH: &str = "crazyagent/task-one";

/// A landing store in a fresh engine root with the test engine host running.
/// Output: the root and the store.
fn store() -> (tempfile::TempDir, Connection) {
    let root = tempfile::tempdir().unwrap();
    let db = Connection::open(root.path().join("engine.db")).unwrap();
    initialize(&db).unwrap();
    let host = seed(&db);
    assert_eq!(host.db_instance.len(), 64);
    (root, db)
}

/// The kind and body of every logged landing event, in log order. Input: the store.
/// Output: the events.
fn events(db: &Connection) -> Vec<(String, serde_json::Value)> {
    let logged: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='state_log')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if !logged {
        return Vec::new();
    }
    let mut statement = db
        .prepare("SELECT kind, body FROM state_log WHERE aggregate='landing' ORDER BY seq")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .unwrap()
        .map(|row| {
            let (kind, body) = row.unwrap();
            (kind, serde_json::from_slice(&body).unwrap())
        })
        .collect()
}

/// The store's revision. Input: the store. Output: the revision.
fn revision(db: &Connection) -> i64 {
    db.query_row("SELECT revision FROM state WHERE id=1", [], |row| {
        row.get(0)
    })
    .unwrap()
}

/// V250-L9: each agent-branch write appends its event with a closed body in its own
/// transaction, and the table rebuilt from the log equals the live rows.
#[test]
fn an_agent_branch_set_and_forgotten_is_logged_and_rebuilds_to_the_live_rows() {
    let (root, db) = store();
    let repository = root.path().join("repo");
    let (one, two) = (repository.join(".wt/one"), repository.join(".wt/two"));
    set_agent_branch(&db, &repository, &one, BRANCH).unwrap();
    set_agent_branch(&db, &repository, &two, "crazyagent/task-two").unwrap();
    forget_agent_branch(&db, two.to_str().unwrap()).unwrap();
    let logged = events(&db);
    let kinds: Vec<&str> = logged.iter().map(|(kind, _)| kind.as_str()).collect();
    assert_eq!(
        kinds,
        [AGENT_BRANCH_SET, AGENT_BRANCH_SET, AGENT_BRANCH_FORGOTTEN]
    );
    assert_eq!(
        logged[0].1,
        json!({"worktree": one, "repository": repository, "branch": BRANCH})
    );
    assert_eq!(logged[2].1, json!({"worktree": two}));
    let live = rows(&db, "landing_agent_branches").unwrap();
    assert_eq!(live.len(), 1);
    rebuild(&db).unwrap();
    assert_eq!(rows(&db, "landing_agent_branches").unwrap(), live);
}

/// V250-L9: a store whose owner runtime is not active refuses an agent-branch write and
/// leaves no row, no event and no revision.
#[test]
fn a_store_without_a_running_host_refuses_an_agent_branch_write_and_leaves_nothing() {
    let (root, db) = store();
    db.execute("UPDATE owner_meta SET runtime_state='offline'", [])
        .unwrap();
    let before = revision(&db);
    let repository = root.path().join("repo");
    let set = set_agent_branch(&db, &repository, &repository.join(".wt/one"), BRANCH);
    assert_eq!(set.unwrap_err().0, "owner_runtime_stale");
    assert!(rows(&db, "landing_agent_branches").unwrap().is_empty());
    assert!(events(&db).is_empty());
    assert_eq!(revision(&db), before, "a refused write moves no revision");
}

/// V250-L9: a worktree's base set twice is logged once per write with a closed body (the
/// worktree and a 40 hex commit), and the table rebuilt from the log keeps the last base.
#[test]
fn a_base_set_again_is_logged_and_rebuilds_to_the_last_base() {
    let (root, db) = store();
    let worktree = root.path().join("repo/.wt/one");
    let (first, last) = ("a".repeat(40), "b".repeat(40));
    set_base(&db, &worktree, &first).unwrap();
    set_base(&db, &worktree, &last).unwrap();
    let logged = events(&db);
    assert_eq!(logged.len(), 2, "one event per base write");
    assert!(logged.iter().all(|(kind, _)| kind == BASE_SET));
    assert_eq!(logged[1].1, json!({"worktree": worktree, "base": last}));
    let live = rows(&db, "landing_bases").unwrap();
    assert_eq!(live.len(), 1);
    rebuild(&db).unwrap();
    assert_eq!(rows(&db, "landing_bases").unwrap(), live);
}

/// V250-L9: each owner-edit write appends one event with a closed body (kept, settled on a
/// change set, dropped, forgotten by change set or by worktree), and the table rebuilt from
/// the log equals the live rows after every step.
#[cfg(unix)]
#[test]
fn owner_edits_kept_settled_and_forgotten_are_logged_and_rebuild_to_the_live_rows() {
    use crate::landing::owner_edit;
    let (root, db) = store();
    let (one, two) = (
        root.path().join("repo/.wt/one"),
        root.path().join("repo/.wt/two"),
    );
    let id = crate::landing::ChangeSetId::new("c".repeat(32)).unwrap();
    owner_edit::record(&db, &one, "README", 1000).unwrap();
    owner_edit::record(&db, &one, "src/a.rs", 1001).unwrap();
    owner_edit::record(&db, &two, "x", 1002).unwrap();
    owner_edit::settle(&db, &one, Some(&id)).unwrap();
    owner_edit::record(&db, &one, "late", 1003).unwrap();
    owner_edit::settle(&db, &two, None).unwrap();
    let logged = events(&db);
    let kinds: Vec<&str> = logged.iter().map(|(kind, _)| kind.as_str()).collect();
    let kept = OWNER_EDIT_KEPT;
    let expected = [
        kept,
        kept,
        kept,
        OWNER_EDITS_SETTLED,
        kept,
        OWNER_EDITS_DROPPED,
    ];
    assert_eq!(kinds, expected);
    assert_eq!(
        logged[0].1,
        json!({"worktree": one, "path": "README", "at_ms": 1000})
    );
    assert_eq!(
        logged[3].1,
        json!({"worktree": one, "change_set": id.as_str()})
    );
    assert_eq!(logged[5].1, json!({"worktree": two}));
    let live = rows(&db, "landing_owner_edits").unwrap();
    assert_eq!(
        live.len(),
        3,
        "two settled on the set and one waiting, the dropped one gone"
    );
    rebuild(&db).unwrap();
    assert_eq!(rows(&db, "landing_owner_edits").unwrap(), live);
    owner_edit::forget(&db, &id).unwrap();
    owner_edit::forget_worktree(&db, &one).unwrap();
    let logged = events(&db);
    assert_eq!(logged.len(), 8, "one event per forget");
    assert_eq!(
        logged[6],
        (
            OWNER_EDITS_FORGOTTEN.to_owned(),
            json!({"change_set": id.as_str()})
        )
    );
    let gone = json!({"worktree": one});
    assert_eq!(logged[7], (WORKTREE_OWNER_EDITS_FORGOTTEN.to_owned(), gone));
    assert!(rows(&db, "landing_owner_edits").unwrap().is_empty());
    rebuild(&db).unwrap();
    assert!(rows(&db, "landing_owner_edits").unwrap().is_empty());
}

/// V250-L9: a landing write owns its transaction. Called inside a caller's transaction it is
/// refused and writes nothing, so no row ever changes without its event (no landing writer
/// runs inside the open's sweep; worktrees::reconcile settles leases only).
#[test]
fn a_landing_write_inside_a_caller_transaction_is_refused_and_leaves_nothing() {
    let (root, db) = store();
    let worktree = root.path().join("repo/.wt/one");
    let before = revision(&db);
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let refused = set_base(&db, &worktree, &"a".repeat(40)).unwrap_err();
    assert_eq!(refused.0, "landing_log_unavailable");
    db.execute_batch("COMMIT").unwrap();
    assert!(
        rows(&db, "landing_bases").unwrap().is_empty(),
        "no row without its event"
    );
    assert!(events(&db).is_empty());
    assert_eq!(revision(&db), before);
}
