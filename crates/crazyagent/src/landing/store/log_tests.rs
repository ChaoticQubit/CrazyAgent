use super::*;
use crate::landing_log::{
    AGENT_BRANCH_FORGOTTEN, AGENT_BRANCH_SET, BASE_FORGOTTEN, BASE_SET, CHANGE_SET_NUMBERED,
    CHANGE_SET_SAVED, OWNER_EDIT_KEPT, OWNER_EDITS_DROPPED, OWNER_EDITS_FORGOTTEN,
    OWNER_EDITS_SETTLED, PUBLISH_PROMPT_NEVER, PUBLISH_PROMPT_SNOOZED,
    WORKTREE_OWNER_EDITS_FORGOTTEN, rebuild, seed,
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

/// V250-L9: retire's forget of a gone worktree's base is logged with a closed body (the
/// worktree), and the table rebuilt from the log holds no base for it.
#[test]
fn a_forgotten_base_is_logged_and_rebuilds_to_no_row() {
    let (root, db) = store();
    let worktree = root.path().join("repo/.wt/one");
    let path = worktree.to_str().unwrap();
    set_base(&db, &worktree, &"a".repeat(40)).unwrap();
    forget_base(&db, path).unwrap();
    let logged = events(&db);
    assert_eq!(logged.len(), 2, "one event per base write");
    assert_eq!(
        logged[1],
        (BASE_FORGOTTEN.to_owned(), json!({"worktree": path}))
    );
    assert!(rows(&db, "landing_bases").unwrap().is_empty());
    rebuild(&db).unwrap();
    assert!(rows(&db, "landing_bases").unwrap().is_empty());
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

/// V250-L9: the numbering pass logs one `ChangeSetNumbered` event per set it numbers, all at
/// one revision, in the order the sets were made; a pass with nothing to number moves no
/// revision; the numbers rebuilt from the log equal the live rows.
#[test]
fn a_numbering_pass_logs_one_event_per_set_at_one_revision_and_rebuilds() {
    let (_root, db) = store();
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, body TEXT NOT NULL);
         INSERT INTO projects(id, body) VALUES ('project-1', '{}');",
    )
    .unwrap();
    for (digit, made) in [('b', 2), ('a', 1)] {
        db.execute(
            "INSERT INTO landing_change_sets(id,project,agent_kind,agent_id,label,repository,\
             worktree,git_dir,base,head,state,last_outcome,turn,prompt_line,files,added,\
             removed,conflicts,created_ms,updated_ms) VALUES (?1,'project-1','sub','agent-1',\
             'Agent 1','/r','/w','/g','b','h','discarded','succeeded','t','Fix',1,1,0,'[]',?2,?2)",
            params![digit.to_string().repeat(32), made],
        )
        .unwrap();
    }
    let before = revision(&db);
    assert_eq!(number_unnumbered(&db).unwrap(), 2);
    assert_eq!(revision(&db), before + 1, "one revision for the pass");
    let logged = events(&db);
    assert_eq!(logged.len(), 2);
    assert!(logged.iter().all(|(kind, _)| kind == CHANGE_SET_NUMBERED));
    let first = json!({"change_set": "a".repeat(32), "project": "project-1", "number": 1});
    let second = json!({"change_set": "b".repeat(32), "project": "project-1", "number": 2});
    assert_eq!((&logged[0].1, &logged[1].1), (&first, &second));
    assert_eq!(number_unnumbered(&db).unwrap(), 0);
    assert_eq!(
        revision(&db),
        before + 1,
        "nothing to number moves no revision"
    );
    let live = rows(&db, "landing_change_set_numbers").unwrap();
    assert_eq!(live.len(), 2);
    rebuild(&db).unwrap();
    assert_eq!(rows(&db, "landing_change_set_numbers").unwrap(), live);
}

/// V250-L9: each answer to a project's publish prompt appends one event with a closed body
/// (Not now with the time it asks again, Don't ask again), and the prompt table rebuilt from
/// the log equals the live row.
#[cfg(unix)]
#[test]
fn publish_prompt_answers_are_logged_and_rebuild_to_the_live_row() {
    use crate::github::{GitHubPromptAnswer, SNOOZE_MS, keep_prompt_answer};
    let (root, db) = store();
    let project = ProjectId::new("project-1").unwrap();
    keep_prompt_answer(root.path(), &project, GitHubPromptAnswer::NotNow);
    keep_prompt_answer(root.path(), &project, GitHubPromptAnswer::Never);
    let snoozed = json!({"project": "project-1", "until_ms": SNOOZE_MS});
    let expected = vec![
        (PUBLISH_PROMPT_SNOOZED.to_owned(), snoozed),
        (
            PUBLISH_PROMPT_NEVER.to_owned(),
            json!({"project": "project-1"}),
        ),
    ];
    assert_eq!(events(&db), expected);
    let live = rows(&db, "github_publish_prompt").unwrap();
    assert_eq!(live.len(), 1, "one row per project");
    rebuild(&db).unwrap();
    assert_eq!(rows(&db, "github_publish_prompt").unwrap(), live);
}

/// V250-L9: a project Delete in the log drops the project's prompt answer at its place, so
/// the prompt table rebuilt from the log equals the live one; a Remove from dashboard keeps
/// the answer (DEC-178 add.1, L6 Q10).
#[cfg(unix)]
#[test]
fn a_logged_project_delete_drops_its_prompt_answer_on_rebuild_and_a_remove_keeps_it() {
    use crate::github::{GitHubPromptAnswer, keep_prompt_answer};
    let (root, db) = store();
    let kept = ProjectId::new("kept").unwrap();
    let deleted = ProjectId::new("deleted").unwrap();
    keep_prompt_answer(root.path(), &kept, GitHubPromptAnswer::Never);
    keep_prompt_answer(root.path(), &deleted, GitHubPromptAnswer::Never);
    removal(&db, &kept, true);
    removal(&db, &deleted, false);
    crate::github::forget_prompt(&db, &deleted).unwrap();
    let live = rows(&db, "github_publish_prompt").unwrap();
    assert_eq!(
        live.len(),
        1,
        "the Remove keeps its answer, the Delete forgets it"
    );
    rebuild(&db).unwrap();
    assert_eq!(rows(&db, "github_publish_prompt").unwrap(), live);
}

/// Appends a project removal to the log as an owner request does, behind the test host,
/// without its projection. Inputs: the store, the project and whether its notes are kept.
fn removal(db: &Connection, project: &ProjectId, kept: bool) {
    use crate::project_setup::removed::event::{KIND, ProjectRemoved};
    use crate::state_log::{Aggregate, Appender, Draft};
    let behavior = rusqlite::TransactionBehavior::Immediate;
    let tx = rusqlite::Transaction::new_unchecked(db, behavior).unwrap();
    let fence = crate::worktrees::log::store_fence(&tx).unwrap();
    let mut log = Appender::own(&tx, fence).unwrap();
    let removed = ProjectRemoved {
        project: project.clone(),
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
    tx.commit().unwrap();
}

/// An engine store with the landing tables and no engine host, as the upgrade step finds
/// it. Output: the store, at revision 0.
fn unhosted() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        CREATE TABLE events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);
        INSERT INTO state VALUES (1,0);",
    )
    .unwrap();
    initialize(&db).unwrap();
    db
}

/// The landing tables the first run rows write, as they stand. Input: the store. Output:
/// each table's rows.
fn landing_rows(db: &Connection) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    let tables = [
        "landing_agent_branches",
        "landing_bases",
        "landing_owner_edits",
        "github_publish_prompt",
    ];
    tables
        .iter()
        .map(|table| rows(db, table).unwrap())
        .collect()
}

/// Runs the first run in one upgrade transaction and commits it. Input: the store. Output:
/// whether a run was appended.
fn first_run(db: &Connection) -> bool {
    use crate::storage::opening::Opening;
    let behavior = rusqlite::TransactionBehavior::Immediate;
    let tx = rusqlite::Transaction::new_unchecked(db, behavior).unwrap();
    let appended = crate::landing_log::first_run(&tx, Opening::for_test(3)).unwrap();
    tx.commit().unwrap();
    appended
}

/// V250-L9 step 14: landing rows written before the log reach replay through one snapshot
/// run at one revision, through the open's proof; integers and empty cells come back as
/// they were, and a table the store lacks (verification_current) is an empty chunk.
#[test]
fn the_first_run_puts_landing_rows_from_before_the_log_on_it() {
    let db = unhosted();
    db.execute_batch(crate::landing::owner_edit::SCHEMA)
        .unwrap();
    db.execute_batch(crate::github::PROMPT_SCHEMA).unwrap();
    let base = "a".repeat(40);
    db.execute_batch(&format!(
        "INSERT INTO landing_agent_branches VALUES ('/repo/.wt/one','/repo','{BRANCH}');
        INSERT INTO landing_bases VALUES ('/repo/.wt/one','{base}');
        INSERT INTO landing_owner_edits VALUES ('/repo/.wt/one','src/a.rs',5,'');
        INSERT INTO github_publish_prompt VALUES ('app',NULL,1);
        INSERT INTO github_publish_prompt VALUES ('web',7,0);"
    ))
    .unwrap();
    let live = landing_rows(&db);
    assert!(first_run(&db));
    assert_eq!(revision(&db), 1, "one run, one revision");
    let kinds: Vec<String> = events(&db).into_iter().map(|(kind, _)| kind).collect();
    assert_eq!(
        kinds.len(),
        8,
        "a chunk per table and the closing event: {kinds:?}"
    );
    assert_eq!(landing_rows(&db), live, "the run changes no row");
    rebuild(&db).unwrap();
    assert_eq!(landing_rows(&db), live);
}

/// V250-L9 step 14: a store with no landing row, or without the `state` table, gets no run,
/// so the step moves no revision.
#[test]
fn the_first_run_writes_nothing_without_landing_rows() {
    let db = unhosted();
    assert!(!first_run(&db));
    assert_eq!(revision(&db), 0);
    assert!(events(&db).is_empty());

    let stateless = Connection::open_in_memory().unwrap();
    initialize(&stateless).unwrap();
    stateless
        .execute(
            "INSERT INTO landing_bases VALUES ('/repo/.wt/one', ?1)",
            ["a".repeat(40)],
        )
        .unwrap();
    assert!(!first_run(&stateless), "no state table");
    assert!(events(&stateless).is_empty());
}

/// A pending change set of `project-1` in a worktree under the root, with owner text in its
/// prompt line, message and summary. Inputs: the root and the digit its id repeats. Output:
/// the record.
fn change_set(root: &std::path::Path, digit: char) -> Record {
    Record {
        id: ChangeSetId::new(digit.to_string().repeat(32)).unwrap(),
        project: ProjectId::new("project-1").unwrap(),
        agent: ChangeAgent {
            kind: AgentKind::Sub,
            id: "7".into(),
            label: "Agent 7".into(),
        },
        repository: root.join("repo"),
        worktree: root.join(format!("repo/.wt/{digit}")),
        git_dir: root.join(format!("repo/.git/worktrees/{digit}")),
        base: "a".repeat(40),
        head: "b".repeat(40),
        state: ChangeState::Pending,
        last_outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "the owner's secret prompt sk-ant-api03-key".into(),
        files: 1,
        added: 2,
        removed: 0,
        conflicts: Vec::new(),
        created_ms: 1,
        updated_ms: 2,
        restorable_until_ms: None,
        pruned: false,
        landed: None,
        intent: None,
        message: "Add a secret page".into(),
        waiting: false,
        outside: false,
        made: false,
        undone: None,
        landed_on: None,
        restored: false,
        regenerated_head: None,
        regenerated_files: Vec::new(),
        accepted_in: None,
        summary: "Added the secret page".into(),
        landing_result: LandingResult::None,
    }
}

/// The digest a logged change set save names. Input: the event body. Output: the digest.
fn row_digest(body: &serde_json::Value) -> crate::owner_texts::TextDigest {
    serde_json::from_value(body["row"].clone()).unwrap()
}

/// Both change set tables as they stand. Input: the store. Output: their rows.
fn change_set_rows(db: &Connection) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    ["landing_change_sets", "landing_change_set_numbers"]
        .iter()
        .map(|table| rows(db, table).unwrap())
        .collect()
}

/// V250-L9 (QUESTION-1056 A): a save, a re-save, a summary and a landing result each log
/// one ChangeSetSaved whose body holds only the set's id, project, number and the digest of
/// its stored row, and both change set tables rebuilt from the log equal the live rows.
#[test]
fn a_saved_change_set_logs_its_row_digest_and_rebuilds_to_the_live_row() {
    let (root, db) = store();
    let mut record = change_set(root.path(), 'c');
    save(&db, &record).unwrap();
    record.updated_ms = 3;
    save(&db, &record).unwrap();
    set_summary(&db, &record.id, "Summed up").unwrap();
    set_landing_result(&db, &record.id, LandingResult::Refused).unwrap();
    let logged = events(&db);
    let kinds: Vec<&str> = logged.iter().map(|(kind, _)| kind.as_str()).collect();
    assert_eq!(kinds, [CHANGE_SET_SAVED; 4]);
    for (_, body) in &logged {
        let mut keys: Vec<&str> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["change_set", "number", "project", "row"]);
        assert_eq!(body["number"], 1);
        assert!(!body.to_string().contains("secret"), "{body}");
    }
    let live = change_set_rows(&db);
    assert_eq!(live[0].len(), 1);
    rebuild(&db).unwrap();
    assert_eq!(change_set_rows(&db), live);
}

/// V250-L9 (QUESTION-1056 A): a re-save releases the old row text, so replay skips the
/// superseded save and the rebuilt row is the last one saved.
#[test]
fn a_resave_releases_the_old_row_text_and_replay_skips_the_superseded_save() {
    let (root, db) = store();
    let mut record = change_set(root.path(), 'c');
    save(&db, &record).unwrap();
    record.message = "Add a second page".into();
    save(&db, &record).unwrap();
    let logged = events(&db);
    assert_eq!(logged.len(), 2);
    let (first, last) = (row_digest(&logged[0].1), row_digest(&logged[1].1));
    assert_ne!(first, last);
    assert_eq!(
        crate::owner_texts::text(&db, &first).unwrap(),
        None,
        "released"
    );
    assert!(crate::owner_texts::text(&db, &last).unwrap().is_some());
    let live = change_set_rows(&db);
    rebuild(&db).unwrap();
    assert_eq!(change_set_rows(&db), live);
}

/// Rewrites the kept row text a logged save names, as a store from another build or a
/// damaged one could hold it. Inputs: the store, the digest and the cells to keep.
fn plant_row(db: &Connection, digest: &crate::owner_texts::TextDigest, cells: &serde_json::Value) {
    let changed = db
        .execute(
            "UPDATE kept_texts SET text=?2 WHERE digest=?1",
            params![digest.as_str(), serde_json::to_vec(cells).unwrap()],
        )
        .unwrap();
    assert_eq!(changed, 1);
}

/// V250-L9 (QUESTION-1056 A): a kept row with a cell the table has no column for fails the
/// rebuild closed, and one without a cell takes that column's default.
#[test]
fn a_row_cell_the_table_lacks_fails_replay_and_a_missing_cell_takes_its_default() {
    let (root, db) = store();
    let record = change_set(root.path(), 'c');
    save(&db, &record).unwrap();
    let digest = row_digest(&events(&db)[0].1);
    let kept = crate::owner_texts::text(&db, &digest).unwrap().unwrap();
    let cells: serde_json::Value = serde_json::from_slice(&kept).unwrap();
    assert_eq!(cells["summary"], "Added the secret page");

    let mut widened = cells.clone();
    widened["column_from_a_later_build"] = json!(1);
    plant_row(&db, &digest, &widened);
    assert_eq!(rebuild(&db).unwrap_err().0, "landing_log_row_cell");

    let mut narrowed = cells.clone();
    narrowed.as_object_mut().unwrap().remove("summary");
    plant_row(&db, &digest, &narrowed);
    rebuild(&db).unwrap();
    let summary: String = db
        .query_row("SELECT summary FROM landing_change_sets", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(summary, "", "the column default");
}

/// V250-L9 step 14 (QUESTION-1056 A): a change set stored before the log reaches replay
/// through the run as its id, project and the digest of its row, kept as an owner text, so
/// no text rides in a chunk; a later save releases that text and the rebuild takes its row.
#[test]
fn the_first_run_carries_a_change_set_as_its_row_digest() {
    let db = unhosted();
    let mut record = change_set(std::path::Path::new("/repo"), 'c');
    write(&db, &record).unwrap();
    db.execute(
        "INSERT INTO landing_change_set_numbers VALUES (?1,'project-1',1)",
        [record.id.as_str()],
    )
    .unwrap();
    let live = change_set_rows(&db);
    assert!(first_run(&db));
    let logged = events(&db);
    assert_eq!(logged.len(), 8, "a chunk per table and the closing event");
    assert!(
        logged
            .iter()
            .all(|(_, body)| !body.to_string().contains("secret"))
    );
    let tables: Vec<&str> = logged
        .iter()
        .filter_map(|(_, body)| body["table"].as_str())
        .collect();
    assert_eq!(
        tables[0], "landing_change_sets",
        "rows before their numbers"
    );
    let rows = logged[0].1["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], record.id.as_str());
    assert_eq!(rows[0][1], "project-1");
    let digest: crate::owner_texts::TextDigest =
        serde_json::from_value(rows[0][2].clone()).unwrap();
    assert!(crate::owner_texts::text(&db, &digest).unwrap().is_some());
    rebuild(&db).unwrap();
    assert_eq!(change_set_rows(&db), live);

    crate::landing_log::seed(&db);
    record.message = "Add a second page".into();
    save(&db, &record).unwrap();
    assert_eq!(
        crate::owner_texts::text(&db, &digest).unwrap(),
        None,
        "the save released it"
    );
    let saved = change_set_rows(&db);
    rebuild(&db).unwrap();
    assert_eq!(change_set_rows(&db), saved);
}

/// V250-L9 (QUESTION-1115): a change set whose conflicting paths total past one kept text's
/// bound, as git's merge listing allows, still saves, logs and rebuilds to the live row.
#[test]
fn a_change_set_past_one_text_saves_logs_and_rebuilds() {
    let (root, db) = store();
    let mut record = change_set(root.path(), 'c');
    let path = format!("src/{}.rs", "p".repeat(1_000));
    let count = crate::owner_texts::TEXT_BYTES_MAX / path.len() + 64;
    record.conflicts = (0..count).map(|index| format!("{path}{index}")).collect();
    record.state = ChangeState::Conflict;
    save(&db, &record).unwrap();
    assert_eq!(events(&db).len(), 1);
    let live = change_set_rows(&db);
    rebuild(&db).unwrap();
    assert_eq!(change_set_rows(&db), live);
}

/// V250-L9: a project Delete leaves its change sets and their kept row texts, as its
/// projection does, so a rebuild across the logged Delete equals the live rows.
#[test]
fn a_deleted_projects_change_sets_rebuild_to_the_live_rows() {
    let (root, db) = store();
    let record = change_set(root.path(), 'c');
    save(&db, &record).unwrap();
    removal(&db, &record.project, false);
    let live = change_set_rows(&db);
    assert_eq!(live[0].len(), 1);
    rebuild(&db).unwrap();
    assert_eq!(change_set_rows(&db), live);
}
