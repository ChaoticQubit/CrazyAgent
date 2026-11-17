use super::*;
use crate::project_setup::removed::event as removed;
use crate::protocol::{Outcome, ProjectId};
use crate::state_log::fence::{engine, seed};
use crate::state_log::replay::{redact, rows};
use crate::state_log::{Aggregate, Appender, Draft};
use crate::storage::opening::Opening;
use rusqlite::Connection;
use rusqlite::types::Value;

/// A text shaped like a credential, which no event or snapshot chunk may hold.
const SECRET: &str = "sk-ant-api03-L8SECRETVALUE0123456789";

/// Every row of the 14 memory tables, table by table, each ordered by all its columns.
type Tables = Vec<Vec<Vec<Value>>>;

/// A store no engine host runs: the `state` and `events` tables, projects p1 and p2, the
/// 14 memory tables and, when `logged`, the touched-key table and its triggers.
/// Input: whether the memory log is on. Output: the connection.
fn unhosted(logged: bool) -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        CREATE TABLE events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);
        INSERT INTO state VALUES (1,0);
        CREATE TABLE projects (id TEXT PRIMARY KEY, body TEXT NOT NULL);
        INSERT INTO projects VALUES ('p1','{}'),('p2','{}');",
    )
    .unwrap();
    crate::memory::initialize(&db).unwrap();
    crate::assets::initialize(&db).unwrap();
    if logged {
        initialize(&db).unwrap();
    }
    db
}

/// A store whose engine host runs, with the memory log on. Output: the connection.
fn hosted() -> Connection {
    let db = unhosted(true);
    seed(&db);
    db
}

/// The 14 memory tables as they stand. Input: the store. Output: their rows.
fn tables(db: &Connection) -> Tables {
    TABLES
        .iter()
        .map(|table| rows(db, table.name()).unwrap())
        .collect()
}

/// The `state` revision and the logged event count. Input: the store. Output: both.
fn moved(db: &Connection) -> (i64, i64) {
    let revision = db
        .query_row("SELECT revision FROM state WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let logged: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='state_log')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let events = if logged {
        db.query_row("SELECT COUNT(*) FROM state_log", [], |row| row.get(0))
            .unwrap()
    } else {
        0
    };
    (revision, events)
}

/// Runs one owner request's memory writes and closes it at the next revision, as
/// `owner_apply_with` does before it persists the request's event. Inputs: the store and
/// the writes. Output: none.
fn request(db: &mut Connection, writes: &str) {
    let tx = db.transaction().unwrap();
    let revision: i64 = tx
        .query_row("SELECT revision FROM state WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    tx.execute_batch(writes).unwrap();
    close(&tx, Action::Knowledge, Some(revision as u64 + 1), None).unwrap();
    tx.commit().unwrap();
}

/// Runs one project removal as the live request does: the request's other writes, the
/// memory part of the removal, the settle of its touched rows, then the `ProjectRemoved`
/// event at a moved revision. Inputs: the store, the project, whether its notes are kept
/// and the other writes. Output: none.
fn remove(db: &mut Connection, project: &str, kept: bool, writes: &str) {
    let project = ProjectId::new(project).unwrap();
    let tx = db.transaction().unwrap();
    tx.execute_batch(writes).unwrap();
    forget_removed(&tx, &project, kept).unwrap();
    close(&tx, Action::Project, None, Some(&project)).unwrap();
    let body = removed::ProjectRemoved {
        project: project.clone(),
        kept,
    }
    .body()
    .unwrap();
    let mut log = Appender::own(&tx, engine()).unwrap();
    log.append(&Draft {
        aggregate: Aggregate::Projects,
        kind: removed::KIND,
        schema: 1,
        at_ms: 1,
        body: &body,
    })
    .unwrap();
    tx.commit().unwrap();
}

/// Asserts that replay of the log rebuilds the 14 tables as they stand. Input: the store.
/// Output: the digests replay named missing.
fn replays_to_live(db: &mut Connection) -> Vec<crate::owner_texts::TextDigest> {
    let live = tables(db);
    let tx = db.transaction().unwrap();
    let rebuilt = rebuild(&tx).unwrap();
    assert_eq!(tables(&tx), live, "replay equals the live tables");
    tx.rollback().unwrap();
    rebuilt.missing
}

/// Every logged event body as text. Input: the store. Output: the bodies.
fn bodies(db: &Connection) -> Vec<String> {
    let mut statement = db.prepare("SELECT body FROM state_log").unwrap();
    statement
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .map(|body| String::from_utf8_lossy(&body.unwrap()).into_owned())
        .collect()
}

/// One row in each of the 14 tables, the activity written and then filled as `capture`
/// does, with a credential-shaped string in a note, an entry body and an asset name.
fn first_writes() -> String {
    format!(
        "INSERT INTO knowledge_principals(id,body) VALUES ('pr1','{{\"principal\":1}}');
        INSERT INTO knowledge_sources(project,id) VALUES ('p1','s1');
        INSERT INTO knowledge_activity(id,project,source,body) VALUES ('a1','p1','s1','');
        UPDATE knowledge_activity SET body='{{\"activity\":1}}' WHERE id='a1';
        INSERT INTO knowledge_nominations VALUES ('n1','p1','{{\"nomination\":1}}');
        INSERT INTO knowledge_entries VALUES ('e1',1,'p1','{{\"text\":\"{SECRET}\"}}');
        INSERT INTO knowledge_heads VALUES ('e1',1,'p1');
        INSERT INTO knowledge_globals(id,entry,version,hash,body)
            VALUES ('g1','e1',1,'h1','{{\"global\":1}}');
        INSERT INTO knowledge_jobs VALUES ('j1','p1','dest1','{{\"job\":1}}');
        INSERT INTO knowledge_watermarks VALUES ('p1','dest1',1);
        INSERT INTO memory_library(id,project,origin,text) VALUES
            ('note-1','p1','{{\"kind\":\"written\"}}','Owner note {SECRET}'),
            ('agent-1','p1','{{\"kind\":\"agent\",\"agent\":\"Ada\"}}','Agent note'),
            ('note-2',NULL,'{{\"kind\":\"written\"}}','App note');
        INSERT INTO memory_settings VALUES (1,'ask');
        INSERT INTO memory_agent_scope VALUES (1,'agent');
        INSERT INTO asset_sources(id,scope,name,body)
            VALUES ('as1','{{\"project\":\"p1\"}}','{SECRET}','{{\"asset\":1}}');
        INSERT INTO asset_snapshots VALUES ('sn1','{{\"snapshot\":1}}');"
    )
}

/// Updates, upserts and deletes over the rows [`first_writes`] left.
const SECOND_WRITES: &str =
    "INSERT INTO knowledge_entries VALUES ('e1',2,'p1','{\"text\":\"two\"}');
    UPDATE knowledge_heads SET version=2 WHERE id='e1';
    UPDATE knowledge_globals SET body='{\"global\":2}', approved=1 WHERE id='g1';
    DELETE FROM knowledge_jobs WHERE id='j1';
    UPDATE knowledge_watermarks SET ordinal=5 WHERE project='p1';
    UPDATE memory_library SET project='p2' WHERE id='note-2';
    UPDATE knowledge_principals SET revoked=1 WHERE id='pr1';
    UPDATE asset_sources SET revoked=1 WHERE id='as1';
    INSERT INTO memory_settings VALUES (1,'save')
        ON CONFLICT(singleton) DO UPDATE SET agent_writes=excluded.agent_writes;";

/// v2.5 L8 replay row: writes, updates and deletes over all 14 tables, a Remove from
/// dashboard (notes kept) and a Delete everything (notes forgotten) replay to the live
/// tables, every column equal, each text read back from the kept-text store, and a text a
/// later event replaced or deleted is named missing.
#[test]
fn replay_rebuilds_every_memory_table_as_the_writes_left_it() {
    let mut db = hosted();
    request(&mut db, &first_writes());
    request(&mut db, SECOND_WRITES);
    remove(&mut db, "p1", true, "");
    request(
        &mut db,
        "INSERT INTO knowledge_sources(project,id) VALUES ('p2','s2');
        INSERT INTO memory_library(id,project,origin,text)
            VALUES ('agent-2','p2','{\"kind\":\"agent\",\"agent\":\"Bo\"}','Second agent note');",
    );
    remove(&mut db, "p2", false, "");
    let live = tables(&db);
    let library = &live[TABLES
        .iter()
        .position(|table| table.name() == "memory_library")
        .unwrap()];
    assert_eq!(library.len(), 1, "only p1's owner note is left");
    assert!(matches!(&library[0][1], Value::Text(id) if id == "note-1"));
    let missing = replays_to_live(&mut db);
    assert!(
        !missing.is_empty(),
        "the replaced global body and the deleted job body are named missing"
    );
}

/// v2.5 L8: a skipped event makes replay differ from the live tables, so the replay row
/// above checks the log and not a copy of the tables.
#[test]
fn a_redacted_memory_event_makes_replay_differ() {
    let mut db = hosted();
    request(&mut db, &first_writes());
    request(&mut db, SECOND_WRITES);
    let live = tables(&db);
    let last: i64 = db
        .query_row(
            "SELECT MAX(seq) FROM state_log WHERE kind=?1",
            [KIND],
            |row| row.get(0),
        )
        .unwrap();
    let tx = db.transaction().unwrap();
    redact(&tx, last as u64, &engine()).unwrap();
    rebuild(&tx).unwrap();
    assert_ne!(tables(&tx), live, "the skipped change is not replayed");
}

/// v2.5 L8 secret row: a note, an entry body and an asset name holding a credential-shaped
/// string leave no trace of it in any event, the first run's snapshot chunks included.
#[test]
fn a_secret_shaped_text_leaves_no_trace_in_the_log() {
    let mut db = hosted();
    request(&mut db, &first_writes());
    request(&mut db, SECOND_WRITES);
    let mut fresh = unhosted(false);
    fresh.execute_batch(&first_writes()).unwrap();
    let tx = fresh.transaction().unwrap();
    assert!(first_run(&tx, Opening::for_test(2)).unwrap());
    tx.commit().unwrap();
    for store in [&db, &fresh] {
        let logged = bodies(store);
        assert!(!logged.is_empty());
        assert!(
            logged.iter().all(|body| !body.contains("L8SECRETVALUE")),
            "no event holds the text"
        );
    }
}

/// v2.5 L8: one owner request writes one revision, its events at it; a request that wrote
/// no memory row logs nothing and moves nothing.
#[test]
fn one_request_logs_its_change_at_its_own_revision() {
    let mut db = hosted();
    request(&mut db, &first_writes());
    let (revision, events) = moved(&db);
    assert_eq!(revision, 1);
    assert!(events >= 1);
    let tx = db.transaction().unwrap();
    close(&tx, Action::Knowledge, Some(2), None).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        moved(&db),
        (revision, events),
        "nothing touched, nothing logged"
    );
}

/// v2.5 L8: a memory row a request that applied nothing left stays touched, and the next
/// request that applies logs it as it stands, so replay still equals the live tables.
#[test]
fn a_row_a_refused_request_left_is_logged_by_the_next_applied_one() {
    let mut db = hosted();
    let tx = db.transaction().unwrap();
    tx.execute(
        "INSERT INTO knowledge_sources(project,id) VALUES ('p1','s1')",
        [],
    )
    .unwrap();
    assert!(!close(&tx, Action::Knowledge, None, None).unwrap());
    tx.commit().unwrap();
    assert_eq!(moved(&db), (0, 0), "nothing logged yet");
    request(&mut db, "");
    assert_eq!(moved(&db), (1, 1), "the next applied request logs it");
    replays_to_live(&mut db);
}

/// v2.5 L8 R1: a removal settles the rows it deleted; a row the removal request wrote and
/// kept stays touched and is logged by the next request that applies.
#[test]
fn a_removal_settles_only_the_rows_it_deleted() {
    let mut db = hosted();
    request(&mut db, &first_writes());
    remove(
        &mut db,
        "p1",
        false,
        "INSERT INTO knowledge_sources(project,id) VALUES ('p2','s9');",
    );
    let pending: i64 = db
        .query_row("SELECT COUNT(*) FROM memory_touched", [], |row| row.get(0))
        .unwrap();
    assert_eq!(pending, 1, "only the standing p2 row stays touched");
    request(&mut db, "");
    replays_to_live(&mut db);
}

/// v2.5 L8 E5: an action that does not apply rolls back every memory write it made, its
/// touched keys included, so a refusal after a write leaves nothing to log.
#[test]
fn an_action_that_is_refused_leaves_no_write() {
    let mut db = hosted();
    let tx = db.transaction().unwrap();
    let outcome = atomic(&tx, || {
        tx.execute(
            "INSERT INTO knowledge_sources(project,id) VALUES ('p1','s1')",
            [],
        )
        .map_err(error)?;
        Ok(Outcome::Rejected {
            code: "invalid_nomination".into(),
        })
    })
    .unwrap();
    assert!(matches!(outcome, Outcome::Rejected { .. }));
    close(&tx, Action::Knowledge, None, None).unwrap();
    let left: i64 = tx
        .query_row("SELECT COUNT(*) FROM knowledge_sources", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(left, 0);
}

/// v2.5 L8 R1: the open sweep forgets agent memories of projects no longer recorded, logs
/// them through the open's proof at one revision, and replay equals the live tables; a
/// second open changes nothing and moves nothing.
#[test]
fn an_open_sweep_logs_the_orphan_agent_memories_it_forgets() {
    let mut db = unhosted(true);
    db.execute_batch(
        "INSERT INTO memory_library(id,project,origin,text) VALUES
            ('agent-gone','gone','{\"kind\":\"agent\",\"agent\":\"Ada\"}','Gone agent note'),
            ('note-gone','gone','{\"kind\":\"written\"}','Gone owner note'),
            ('agent-1','p1','{\"kind\":\"agent\",\"agent\":\"Ada\"}','Live agent note');",
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    assert!(first_run(&tx, Opening::for_test(1)).unwrap());
    tx.commit().unwrap();
    let (before, logged) = moved(&db);
    assert!(sweep(&db, Opening::for_test(2)).unwrap());
    assert_eq!(
        moved(&db),
        (before + 1, logged + 1),
        "one revision, one event"
    );
    let ids: Vec<String> = db
        .prepare("SELECT id FROM memory_library ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(ids, ["agent-1", "note-gone"]);
    replays_to_live(&mut db);
    assert!(!sweep(&db, Opening::for_test(3)).unwrap());
    assert_eq!(moved(&db), (before + 1, logged + 1));
}

/// v2.5 L8 STEPS row: rows written before the log reach replay through one snapshot run at
/// one revision through the open's proof; a store with no memory row gets no run.
#[test]
fn the_first_run_puts_memory_rows_written_before_the_log_on_it() {
    let mut db = unhosted(false);
    let tx = db.transaction().unwrap();
    assert!(!first_run(&tx, Opening::for_test(1)).unwrap());
    tx.commit().unwrap();
    assert_eq!(moved(&db), (0, 0));
    db.execute_batch(&first_writes()).unwrap();
    let tx = db.transaction().unwrap();
    assert!(first_run(&tx, Opening::for_test(2)).unwrap());
    tx.commit().unwrap();
    let (revision, events) = moved(&db);
    assert_eq!(revision, 1);
    assert!(events >= 2, "chunks and the closing event");
    initialize(&db).unwrap();
    replays_to_live(&mut db);
}

/// v2.5 L8 STEPS row: a store whose memory is already on the log gets no second run, so the
/// step run again over it, as an upgrade from a reset version runs it, moves nothing.
#[test]
fn the_first_run_skips_a_store_whose_memory_is_already_logged() {
    let mut db = hosted();
    request(&mut db, &first_writes());
    let before = moved(&db);
    let tx = db.transaction().unwrap();
    assert!(!first_run(&tx, Opening::for_test(2)).unwrap());
    tx.commit().unwrap();
    assert_eq!(moved(&db), before, "no second snapshot run");
}

/// v2.5 L8 L1-N1: a change body decodes only with its three fields, closed tables, cells
/// and digests; free text in any field or one more field is refused.
#[test]
fn a_change_body_is_closed() {
    let good = r#"{"action":"knowledge","deleted":[{"table":"knowledge_jobs","key":[{"id":"j1"}]}],"put":[]}"#;
    assert!(serde_json::from_str::<Changed>(good).is_ok());
    for bad in [
        r#"{"action":"free text","deleted":[],"put":[]}"#,
        r#"{"action":"knowledge","deleted":[{"table":"knowledge_jobs","key":[{"id":"has space"}]}],"put":[]}"#,
        r#"{"action":"knowledge","deleted":[],"put":[{"table":"memory_library","cells":[{"text":"not a digest"}]}]}"#,
        r#"{"action":"knowledge","deleted":[],"put":[],"extra":1}"#,
        r#"{"action":"knowledge","deleted":[{"table":"projects","key":[{"id":"p1"}]}],"put":[]}"#,
    ] {
        assert!(serde_json::from_str::<Changed>(bad).is_err(), "{bad}");
    }
}
