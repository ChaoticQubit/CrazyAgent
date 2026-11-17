use super::*;
use crate::engine::EngineError;
use crate::master_session::{MasterSessionView, SESSION_DELETED};
use crate::state_log::fence::{engine, seed, test_owner};
use crate::state_log::replay::{Logged, redact, rows};
use crate::state_log::{Aggregate, Appender, Draft, SNAPSHOT_CHUNK, SNAPSHOT_DONE, TOMBSTONE};
use crate::storage::opening::Opening;
use rusqlite::Connection;
use rusqlite::types::Value as Column;
use serde_json::json;

/// The `state` row at revision 0, the `events` table, and the three sessions tables with
/// the columns the engine's own make; replay never needs `master_turns`, so there is none.
const STATE: &str =
    "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
    CREATE TABLE events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);
    CREATE TABLE master_sessions (id TEXT PRIMARY KEY, project TEXT NOT NULL,
        created INTEGER NOT NULL UNIQUE, version INTEGER NOT NULL, phase TEXT NOT NULL,
        body TEXT NOT NULL);
    CREATE TABLE conversation_entries (project TEXT NOT NULL,
        event_cursor INTEGER NOT NULL UNIQUE, kind TEXT NOT NULL,
        brief_version INTEGER NOT NULL, text TEXT NOT NULL, PRIMARY KEY (project, event_cursor));
    CREATE TABLE agent_texts (agent INTEGER PRIMARY KEY CHECK (agent > 0),
        project TEXT NOT NULL, text TEXT NOT NULL);
    INSERT INTO state VALUES (1,0);";

/// A store with the sessions tables and the owner tables holding the test host started.
/// Output: the connection.
fn store() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(STATE).unwrap();
    seed(&db);
    db
}

/// A store with the sessions tables that no engine host runs. Output: the connection.
fn unhosted() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(STATE).unwrap();
    db
}

/// A stored master session of one project at one version and phase. Inputs: the project,
/// the session, the version and the phase word. Output: the session.
fn view(project: &str, session: &str, version: u64, phase: &str) -> MasterSessionView {
    let hex = "a".repeat(64);
    serde_json::from_value(json!({
        "project_id": project,
        "session_id": session,
        "bootstrap_task_id": "t1",
        "selection": {
            "backend": "rcli",
            "pool_ref": "pool",
            "target_profile_ref": "profile",
            "route": {
                "preferred_target": null,
                "required_target": null,
                "required_capabilities": [],
                "account_ref": "account",
                "budget_ref": "budget",
                "resources": {
                    "slots": 1, "cpu_millis": 1000, "memory_mib": 512, "processes": 4,
                    "output_bytes": 1048576, "wall_time_ms": 60000, "spend_microunits": 0
                }
            }
        },
        "config_id": "c1",
        "source_hash": hex,
        "phase": phase,
        "version": version.to_string(),
        "current_turn": null,
        "owner": {
            "origin": "native_owner",
            "owner_id": hex,
            "operation_id": "r1",
            "request_hash": hex,
            "engine_epoch": hex,
            "admitted_epoch": 1,
            "resulting_epoch": 1,
            "revision_before": "1",
            "revision_after": version.to_string(),
            "event_cursor": version.to_string()
        }
    }))
    .unwrap()
}

/// Writes a session row as the open or a save leaves it, without marking it. Inputs: the
/// store, the session and the revision it was created at. Output: none.
fn write_session(db: &Connection, session: &MasterSessionView, created: u64) {
    let phase = serde_json::to_value(session.phase).unwrap();
    db.execute(
        "INSERT INTO master_sessions VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(id) DO UPDATE SET
         version=excluded.version, phase=excluded.phase, body=excluded.body",
        rusqlite::params![
            session.session_id.as_str(),
            session.project_id.as_str(),
            created as i64,
            session.version.value() as i64,
            phase.as_str().unwrap(),
            serde_json::to_string(session).unwrap()
        ],
    )
    .unwrap();
}

/// Writes a session row and marks it, as `save_session` does. Inputs: the store, the session
/// and its created revision. Output: none.
fn put_session(db: &Connection, session: &MasterSessionView, created: u64) {
    write_session(db, session, created);
    mark_session(db, session.project_id.as_str(), session.session_id.as_str()).unwrap();
}

/// Writes a conversation entry and marks it, as `conversation::apply` does. Inputs: the
/// store, the project, the cursor, the kind word, the brief version and the text. Output:
/// none.
fn put_entry(db: &Connection, project: &str, cursor: u64, kind: &str, version: i64, text: &str) {
    db.execute(
        "INSERT INTO conversation_entries VALUES (?1,?2,?3,?4,?5)",
        rusqlite::params![project, cursor as i64, kind, version, text],
    )
    .unwrap();
    mark_entry(db, project, cursor).unwrap();
}

/// Writes an agent's own instructions and marks them, as the transport save does. Inputs:
/// the store, the agent, the project and the text. Output: none.
fn put_text(db: &Connection, agent: i64, project: &str, text: &str) {
    db.execute(
        "INSERT INTO agent_texts VALUES (?1,?2,?3) ON CONFLICT(agent) DO UPDATE SET text=excluded.text",
        rusqlite::params![agent, project, text],
    )
    .unwrap();
    mark(db, Table::Texts, &agent.to_string(), project).unwrap();
}

/// The three tables, each ordered by all its columns. Input: the store. Output: the rows.
fn tables(db: &Connection) -> Vec<Vec<Vec<Column>>> {
    TABLES
        .iter()
        .map(|table| rows(db, table).unwrap())
        .collect()
}

/// The kinds of the logged events in seq order. Input: the store. Output: the kinds.
fn kinds(db: &Connection) -> Vec<String> {
    db.prepare("SELECT kind FROM state_log ORDER BY seq")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// The bodies of the logged events of one kind in seq order. Inputs: the store and the
/// kind. Output: the bodies as JSON.
fn bodies(db: &Connection, kind: &str) -> Vec<serde_json::Value> {
    db.prepare("SELECT body FROM state_log WHERE kind=?1 ORDER BY seq")
        .unwrap()
        .query_map([kind], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .map(|body| serde_json::from_slice(&body.unwrap()).unwrap())
        .collect()
}

/// The marks no close logged yet, 0 before any key was marked. Input: the store. Output:
/// the count.
fn left_marks(db: &Connection) -> i64 {
    let made: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='sessions_touched')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if !made {
        return 0;
    }
    db.query_row("SELECT COUNT(*) FROM sessions_touched", [], |row| {
        row.get(0)
    })
    .unwrap()
}

/// Whether owner_texts holds a holder. Inputs: the store and the holder. Output: the
/// answer.
fn held(db: &Connection, holder: &str) -> bool {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM kept_text_refs WHERE holder=?1)",
        [holder],
        |row| row.get(0),
    )
    .unwrap()
}

/// The refusal code of a result, none when it succeeded. Input: the result. Output: the code.
fn code<T>(result: Result<T, EngineError>) -> Option<String> {
    result.err().map(|error| error.0)
}

/// Asserts that replay of the log rebuilds the three tables as they stand. Input: the
/// store. Output: none.
fn replays_to_live(db: &mut Connection) {
    let live = tables(db);
    let tx = db.transaction().unwrap();
    rebuild(&tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(tables(db), live, "replay equals the live tables");
}

/// v2.5 L13 replay row: the marked writes of every table close into one event each, a
/// second close logs the saved session again and the dropped entry and agent text as
/// dropped events whose texts go, L12's delete event is passed over, and replay of the log,
/// with no `master_turns` table, equals the live tables; a skipped (redacted) later save
/// shows the earlier one.
#[test]
fn every_marked_write_closes_into_events_whose_replay_equals_the_live_rows() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    put_session(&tx, &view("p1", "s1", 3, "configured"), 3);
    put_entry(&tx, "p1", 3, "message", 0, "Write the import guide");
    put_entry(&tx, "p1", 4, "brief", 1, "The brief");
    put_text(&tx, 7, "p1", "Keep answers short.\n");
    let mut log = Appender::own(&tx, engine()).unwrap();
    assert_eq!(close(&mut log).unwrap(), 4);
    drop(log);
    tx.commit().unwrap();
    assert_eq!(
        kinds(&db),
        [KIND_SESSION, KIND_ENTRY, KIND_ENTRY, KIND_TEXT]
    );
    assert_eq!(left_marks(&db), 0, "a close logs every mark it reads");
    for holder in [
        "conversation_entries/p1/3/text",
        "conversation_entries/p1/4/text",
        "agent_texts/7/text",
    ] {
        assert!(held(&db, holder), "{holder}");
    }
    let entry = &bodies(&db, KIND_ENTRY)[0];
    assert_eq!(entry["kind"], "message");
    assert!(!entry.to_string().contains("import guide"), "{entry}");

    let tx = db.transaction().unwrap();
    put_session(&tx, &view("p1", "s1", 5, "parked"), 3);
    tx.execute("DELETE FROM conversation_entries WHERE event_cursor=3", [])
        .unwrap();
    mark_entry(&tx, "p1", 3).unwrap();
    tx.execute("DELETE FROM agent_texts WHERE agent=7", [])
        .unwrap();
    mark(&tx, Table::Texts, "7", "p1").unwrap();
    let mut log = Appender::own(&tx, engine()).unwrap();
    assert_eq!(close(&mut log).unwrap(), 3);
    let deleted = serde_json::to_vec(&json!({"project": "p1", "session": "s9"})).unwrap();
    log.append(&Draft {
        aggregate: Aggregate::Sessions,
        kind: SESSION_DELETED,
        schema: 1,
        at_ms: 1,
        body: &deleted,
    })
    .unwrap();
    drop(log);
    tx.commit().unwrap();
    assert_eq!(
        kinds(&db)[4..],
        [
            KIND_SESSION,
            KIND_ENTRIES_DROPPED,
            KIND_TEXTS_DROPPED,
            SESSION_DELETED
        ]
    );
    assert_eq!(
        bodies(&db, KIND_ENTRIES_DROPPED),
        [json!({"project": "p1", "cursors": [3]})]
    );
    assert_eq!(
        bodies(&db, KIND_TEXTS_DROPPED),
        [json!({"project": "p1", "agents": [7]})]
    );
    assert!(
        !held(&db, "conversation_entries/p1/3/text"),
        "a dropped entry's text goes"
    );
    assert!(
        !held(&db, "agent_texts/7/text"),
        "a dropped agent text goes"
    );
    assert!(held(&db, "conversation_entries/p1/4/text"));
    let live = tables(&db);
    assert_eq!(live.iter().map(Vec::len).collect::<Vec<_>>(), [1, 1, 0]);
    replays_to_live(&mut db);

    let parked: i64 = db
        .query_row(
            "SELECT MAX(seq) FROM state_log WHERE kind=?1",
            [KIND_SESSION],
            |row| row.get(0),
        )
        .unwrap();
    let tx = db.transaction().unwrap();
    redact(&tx, parked as u64, &engine()).unwrap();
    rebuild(&tx).unwrap();
    tx.commit().unwrap();
    let phase: String = db
        .query_row("SELECT phase FROM master_sessions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(phase, "configured", "a skipped save shows the earlier one");
}

/// v2.5 L13 (L8 I1): a close at a revision another writer already logged logs nothing and
/// leaves its marks; the next close logs them as they then stand.
#[test]
fn a_close_at_a_revision_another_writer_logged_leaves_its_marks_for_the_next_close() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let mut other = Appender::own(&tx, engine()).unwrap();
    other
        .append(&Draft {
            aggregate: Aggregate::Worktrees,
            kind: "Noted",
            schema: 1,
            at_ms: 1,
            body: b"{}",
        })
        .unwrap();
    let revision = other.revision();
    drop(other);
    put_text(&tx, 7, "p1", "Keep answers short.\n");
    assert_eq!(close_owner(&tx, revision, &test_owner()).unwrap(), 0);
    assert_eq!(close_engine(&tx, revision).unwrap(), 0);
    tx.commit().unwrap();
    assert_eq!(left_marks(&db), 1, "the mark waits for the next close");

    let tx = db.transaction().unwrap();
    put_text(&tx, 7, "p1", "Keep answers shorter.\n");
    assert_eq!(
        close_own(&tx).unwrap(),
        1,
        "one event per key, as it stands"
    );
    tx.commit().unwrap();
    assert_eq!(left_marks(&db), 0);
    assert_eq!(kinds(&db), ["Noted", KIND_TEXT]);
    replays_to_live(&mut db);
}

/// v2.5 L13: with no owner runtime active an engine writer's close logs nothing and keeps
/// its marks for a fenced close, while a writer that moves its own revision is refused, as
/// L10's are, and moves nothing.
#[test]
fn without_an_owner_runtime_an_engine_close_keeps_its_marks_and_an_own_close_is_refused() {
    let mut db = unhosted();
    let tx = db.transaction().unwrap();
    tx.execute("UPDATE state SET revision=1 WHERE id=1", [])
        .unwrap();
    put_session(&tx, &view("p1", "s1", 1, "configured"), 1);
    assert_eq!(close_engine(&tx, 1).unwrap(), 0);
    assert_eq!(code(close_own(&tx)), Some("owner_runtime_stale".into()));
    tx.commit().unwrap();
    assert_eq!(left_marks(&db), 1);
    let revision: i64 = db
        .query_row("SELECT revision FROM state WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(revision, 1, "the refused close moved no revision");
    let logged: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='state_log')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!logged, "nothing reached the log");
}

/// v2.5 L13: dropped keys past one event's list split into events of one project each, at
/// most [`DROPPED_MAX`] keys, and each replays.
#[test]
fn dropped_keys_past_one_list_split_into_events_of_one_project_each() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    for cursor in 1..=DROPPED_MAX as u64 + 1 {
        mark_entry(&tx, "p1", cursor).unwrap();
    }
    mark_entry(&tx, "p2", 9_000).unwrap();
    let mut log = Appender::own(&tx, engine()).unwrap();
    assert_eq!(close(&mut log).unwrap(), 3);
    drop(log);
    tx.commit().unwrap();
    let dropped = bodies(&db, KIND_ENTRIES_DROPPED);
    let sizes: Vec<(String, usize)> = dropped
        .iter()
        .map(|body| {
            (
                body["project"].as_str().unwrap().to_owned(),
                body["cursors"].as_array().unwrap().len(),
            )
        })
        .collect();
    assert_eq!(
        sizes,
        [
            ("p1".to_owned(), DROPPED_MAX),
            ("p1".to_owned(), 1),
            ("p2".to_owned(), 1)
        ]
    );
    replays_to_live(&mut db);
}

/// v2.5 L13 upgrade row: rows written before the log reach replay through one snapshot run
/// at one revision through the open's proof, their texts kept under their holders, and a
/// store with no row or no `state` table gets no run.
#[test]
fn the_first_run_carries_rows_written_before_the_log() {
    let mut db = unhosted();
    let tx = db.transaction().unwrap();
    assert!(
        !first_run(&tx, Opening::for_test(3)).unwrap(),
        "no row, no run"
    );
    write_session(&tx, &view("p1", "s1", 3, "parked"), 2);
    tx.execute(
        "INSERT INTO conversation_entries VALUES ('p1',3,'message',0,'Write the import guide')",
        [],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO agent_texts VALUES (7,'p1','Keep answers short.\n')",
        [],
    )
    .unwrap();
    assert!(first_run(&tx, Opening::for_test(3)).unwrap());
    tx.commit().unwrap();
    assert_eq!(
        kinds(&db),
        [
            SNAPSHOT_CHUNK,
            SNAPSHOT_CHUNK,
            SNAPSHOT_CHUNK,
            SNAPSHOT_DONE
        ]
    );
    let revision: i64 = db
        .query_row("SELECT revision FROM state WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(revision, 1, "one run, one revision");
    assert!(held(&db, "conversation_entries/p1/3/text"));
    assert!(held(&db, "agent_texts/7/text"));
    assert_eq!(left_marks(&db), 0, "a run marks nothing");
    replays_to_live(&mut db);

    let mut stateless = Connection::open_in_memory().unwrap();
    stateless
        .execute_batch(&STATE.replace("INSERT INTO state VALUES (1,0);", ""))
        .unwrap();
    stateless.execute("DROP TABLE state", []).unwrap();
    let tx = stateless.transaction().unwrap();
    write_session(&tx, &view("p1", "s1", 3, "parked"), 2);
    assert!(
        !first_run(&tx, Opening::for_test(3)).unwrap(),
        "no state table"
    );
}

/// v2.5 L13: a snapshot run of more rows than one body holds and the events after it in the
/// same writer replay to the same rows; rebuilding twice gives the same rows.
#[test]
fn a_snapshot_run_and_the_events_after_it_replay_to_the_same_rows() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    write_session(&tx, &view("p1", "s1", 3, "parked"), 2);
    for cursor in 1..=1_000_i64 {
        tx.execute(
            "INSERT INTO conversation_entries VALUES ('p1',?1,'message',0,?2)",
            rusqlite::params![cursor, format!("Line {cursor} of the plan")],
        )
        .unwrap();
    }
    let mut log = Appender::own(&tx, engine()).unwrap();
    let done = snapshot(&mut log, 9).unwrap();
    put_entry(&tx, "p1", 1_001, "brief", 1, "The brief");
    assert_eq!(close(&mut log).unwrap(), 1);
    drop(log);
    tx.commit().unwrap();
    crate::state_log::verify_snapshot(&db, done).unwrap();
    let chunks = kinds(&db)
        .iter()
        .filter(|kind| kind.as_str() == SNAPSHOT_CHUNK)
        .count();
    assert!(
        chunks > TABLES.len(),
        "{chunks} chunks: 1000 entries outgrow one body"
    );
    let live = tables(&db);
    assert_eq!(live[1].len(), 1_001);
    for _ in 0..2 {
        replays_to_live(&mut db);
    }
    assert_eq!(tables(&db), live);
}

/// v2.5 L13 and L1-N1: replay reads only the sessions kinds at schema 1 with exactly their
/// fields; another aggregate or schema, L12's delete, an entry whose kind and version
/// disagree, an empty, oversized or zero-bearing list, agent 0 and a session body naming
/// another session are refused.
#[test]
fn replay_refuses_a_sessions_event_it_does_not_know() {
    let logged = |aggregate: Aggregate, kind: &str, schema: u16, body: serde_json::Value| Logged {
        seq: 1,
        revision: 1,
        aggregate,
        kind: kind.into(),
        schema,
        at_ms: 1,
        body: serde_json::to_vec(&body).unwrap(),
    };
    let sessions = |kind: &str, body: serde_json::Value| logged(Aggregate::Sessions, kind, 1, body);
    let digest = "0".repeat(64);
    let session = json!({"project": "p1", "session": "s1", "created": 3,
        "view": view("p1", "s1", 3, "configured")});
    let entry = json!({"project": "p1", "cursor": 3, "kind": "message", "version": 0,
        "text": digest});
    assert!(decode(&sessions(KIND_SESSION, session.clone())).is_ok());
    assert!(decode(&sessions(KIND_ENTRY, entry.clone())).is_ok());
    let invalid = Some("sessions_log_event_invalid".to_owned());
    let refused = [
        logged(Aggregate::Settings, KIND_SESSION, 1, session.clone()),
        logged(Aggregate::Sessions, KIND_SESSION, 2, session.clone()),
        sessions("SessionGone", session.clone()),
        sessions(SESSION_DELETED, json!({"project": "p1", "session": "s1"})),
        sessions(
            KIND_SESSION,
            json!({"project": "p1", "session": "s2", "created": 3,
                "view": view("p1", "s1", 3, "configured")}),
        ),
        sessions(
            KIND_ENTRY,
            json!({"project": "p1", "cursor": 3, "kind": "message", "version": 2,
                "text": digest}),
        ),
        sessions(
            KIND_ENTRY,
            json!({"project": "p1", "cursor": 3, "kind": "brief", "version": 0,
                "text": digest}),
        ),
        sessions(
            KIND_ENTRY,
            json!({"project": "p1", "cursor": 3, "kind": "message", "version": 0,
                "text": "Write the import guide"}),
        ),
        sessions(
            KIND_ENTRIES_DROPPED,
            json!({"project": "p1", "cursors": []}),
        ),
        sessions(
            KIND_ENTRIES_DROPPED,
            json!({"project": "p1", "cursors": [0]}),
        ),
        sessions(
            KIND_ENTRIES_DROPPED,
            json!({"project": "p1", "cursors": vec![1; DROPPED_MAX + 1]}),
        ),
        sessions(
            KIND_TEXT,
            json!({"project": "p1", "agent": 0, "text": digest}),
        ),
        sessions(KIND_TEXTS_DROPPED, json!({"project": "p1", "agents": []})),
    ];
    for event in refused {
        assert_eq!(code(decode(&event)), invalid, "{}", event.kind);
    }
}

/// v2.5 L13: a demo clear of an earlier id tombstones every sessions event naming that
/// project at its top level and no event of another project.
#[test]
fn a_demo_clear_tombstones_every_sessions_event_of_its_project_and_no_other() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    for project in ["p1", "p2"] {
        put_session(
            &tx,
            &view(project, &format!("s-{project}"), 3, "configured"),
            3,
        );
        put_entry(
            &tx,
            project,
            if project == "p1" { 3 } else { 4 },
            "message",
            0,
            "Hello",
        );
        put_text(
            &tx,
            if project == "p1" { 7 } else { 8 },
            project,
            "Keep answers short.\n",
        );
    }
    let mut log = Appender::own(&tx, engine()).unwrap();
    assert_eq!(close(&mut log).unwrap(), 6);
    drop(log);
    assert_eq!(forget_project(&tx, "p1", &engine()).unwrap(), 3);
    tx.commit().unwrap();
    let mut statement = db
        .prepare("SELECT body FROM state_log ORDER BY seq")
        .unwrap();
    let bodies: Vec<Vec<u8>> = statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let (gone, kept): (Vec<_>, Vec<_>) =
        bodies.iter().partition(|body| body.as_slice() == TOMBSTONE);
    assert_eq!(gone.len(), 3);
    assert!(
        kept.iter()
            .all(|body| String::from_utf8_lossy(body).contains("\"project\":\"p2\"")),
        "only p1's events are tombstoned"
    );
}
