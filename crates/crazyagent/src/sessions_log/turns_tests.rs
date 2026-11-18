use super::tests::{
    bodies, code, held, kinds, left_marks, replays_to_live, store, unhosted, view, write_session,
};
use super::*;
use crate::master_session::SESSION_DELETED;
use crate::state_log::fence::engine;
use crate::state_log::replay::{Logged, redact_matching};
use crate::state_log::{Aggregate, Appender, Draft, SNAPSHOT_CHUNK, SNAPSHOT_DONE};
use crate::storage::opening::Opening;
use rusqlite::Connection;
use serde_json::json;

/// The words every L13c body below carries as its stored text, which no event may hold.
const SECRET: &str = "Write the import guide";

/// Writes a master turn row, its body holding [`SECRET`], and marks it, as `queue_turn` and
/// `save_turn` do. Inputs: the store, the session, the ordinal, the turn id, the input and
/// the phase word. Output: none.
fn put_turn(db: &Connection, session: &str, ordinal: i64, id: &str, input: u64, phase: &str) {
    db.execute(
        "INSERT INTO master_turns VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(session,ordinal) DO UPDATE SET phase=excluded.phase, body=excluded.body",
        rusqlite::params![session, ordinal, id, input as i64, phase, json!({"input": SECRET, "phase": phase}).to_string()],
    )
    .unwrap();
    mark_turn(db, input).unwrap();
}

/// Writes a turn's preparation and marks it, as `prepare_turn` does. Inputs: the store, the
/// session and the turn id. Output: none.
fn put_preparation(db: &Connection, session: &str, turn: &str) {
    db.execute(
        "INSERT INTO master_preparations VALUES (?1,?2,?3)",
        rusqlite::params![session, turn, json!({"plan": SECRET}).to_string()],
    )
    .unwrap();
    mark_preparation(db, session, turn).unwrap();
}

/// Writes a session's waiting Resolve and marks it, as `wait_resolve` does. Inputs: the
/// store, the project, the session, the operation and the input. Output: none.
fn put_resolve(db: &Connection, project: &str, session: &str, operation: &str, input: u64) {
    db.execute(
        "INSERT OR REPLACE INTO master_waiting_resolves VALUES (?1,?2,?3,?4,?5)",
        rusqlite::params![
            session,
            project,
            operation,
            input as i64,
            json!({"ask": SECRET}).to_string()
        ],
    )
    .unwrap();
    mark_resolve(db, project, session).unwrap();
}

/// Writes a configured profile receipt and marks it, as `publish_rcli_profile` does.
/// Inputs: the store, the receipt id, the session and the profile. Output: none.
fn put_profile(db: &Connection, id: &str, session: &str, profile: &str) {
    db.execute(
        "INSERT INTO configured_master_profiles VALUES (?1,?2,?3,?4)",
        rusqlite::params![id, session, profile, json!({"home": SECRET}).to_string()],
    )
    .unwrap();
    mark_profile(db, id).unwrap();
}

/// Whether any logged event body holds the stored text. Input: the store. Output: the
/// answer.
fn text_in_log(db: &Connection) -> bool {
    db.prepare("SELECT body FROM state_log")
        .unwrap()
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .any(|body| String::from_utf8_lossy(&body.unwrap()).contains("import guide"))
}

/// The rows each snapshot chunk of one table carries, chunk by chunk. Inputs: the store and
/// the table. Output: the row counts.
fn chunk_rows(db: &Connection, table: &str) -> Vec<usize> {
    db.prepare("SELECT body FROM state_log WHERE kind=?1 ORDER BY seq")
        .unwrap()
        .query_map([SNAPSHOT_CHUNK], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .map(|body| serde_json::from_slice::<serde_json::Value>(&body.unwrap()).unwrap())
        .filter(|chunk| chunk["table"] == table)
        .map(|chunk| chunk["rows"].as_array().unwrap().len())
        .collect()
}

/// A session p1/s1 with one turn, its preparation, a waiting Resolve and a profile receipt,
/// every write marked. Input: the transaction. Output: none.
fn session_with_turn(db: &Connection) {
    write_session(db, &view("p1", "s1", 3, "configured"), 3);
    put_turn(db, "s1", 1, "turn-1", 5, "queued");
    put_preparation(db, "s1", "turn-1");
    put_resolve(db, "p1", "s1", "resolve-1", 6);
    put_profile(db, "reg-1", "s1", "cred-1");
}

/// v2.5 L13c replay row: a session, a turn, its preparation, a waiting Resolve and a
/// profile receipt close into one event each with their bodies as kept digests; a second
/// close logs the turn's new phase, the Resolve gone and a receipt replaced, the dropped
/// receipt before the new one, their bodies let go; replay equals the live tables.
#[test]
fn every_marked_turn_table_write_closes_into_events_whose_replay_equals_the_live_rows() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    session_with_turn(&tx);
    mark_session(&tx, "p1", "s1").unwrap();
    let mut log = Appender::own(&tx, engine()).unwrap();
    assert_eq!(close(&mut log).unwrap(), 5);
    drop(log);
    tx.commit().unwrap();
    assert_eq!(
        kinds(&db),
        [
            KIND_SESSION,
            KIND_TURN,
            KIND_PREPARATION,
            KIND_RESOLVE,
            KIND_PROFILE
        ]
    );
    assert_eq!(left_marks(&db), 0, "a close logs every mark it reads");
    for holder in [
        "master_turns/5/body",
        "master_preparations/5/body",
        "master_waiting_resolves/s1/body",
        "configured_master_profiles/reg-1/body",
    ] {
        assert!(held(&db, holder), "{holder}");
    }
    let turn = &bodies(&db, KIND_TURN)[0];
    assert_eq!(
        (
            &turn["project"],
            &turn["session"],
            &turn["ordinal"],
            &turn["id"],
            &turn["input"]
        ),
        (
            &json!("p1"),
            &json!("s1"),
            &json!(1),
            &json!("turn-1"),
            &json!(5)
        )
    );
    assert_eq!(turn["phase"], "queued");
    assert_eq!(turn["body"].as_str().unwrap().len(), 64, "{turn}");
    assert!(!text_in_log(&db), "no stored text in the log");

    let tx = db.transaction().unwrap();
    put_turn(&tx, "s1", 1, "turn-1", 5, "running");
    mark_resolve(&tx, "p1", "s1").unwrap();
    tx.execute("DELETE FROM master_waiting_resolves WHERE session='s1'", [])
        .unwrap();
    mark_profile(&tx, "reg-1").unwrap();
    tx.execute(
        "DELETE FROM configured_master_profiles WHERE id='reg-1'",
        [],
    )
    .unwrap();
    put_profile(&tx, "reg-2", "s1", "cred-1");
    let mut log = Appender::own(&tx, engine()).unwrap();
    assert_eq!(close(&mut log).unwrap(), 4);
    drop(log);
    tx.commit().unwrap();
    assert_eq!(
        kinds(&db)[5..],
        [
            KIND_TURN,
            KIND_RESOLVES_DROPPED,
            KIND_PROFILES_DROPPED,
            KIND_PROFILE
        ]
    );
    assert_eq!(
        bodies(&db, KIND_RESOLVES_DROPPED),
        [json!({"project": "p1", "sessions": ["s1"]})]
    );
    assert_eq!(
        bodies(&db, KIND_PROFILES_DROPPED),
        [json!({"project": "p1", "ids": ["reg-1"]})]
    );
    assert!(!held(&db, "master_waiting_resolves/s1/body"));
    assert!(!held(&db, "configured_master_profiles/reg-1/body"));
    assert!(held(&db, "configured_master_profiles/reg-2/body"));
    assert!(!text_in_log(&db));
    replays_to_live(&mut db);
}

/// v2.5 L13c with L12 (Q3): a Delete after a snapshot run lets the session's turn and
/// preparation bodies go, its SessionDeleted and redaction leave no earlier event naming
/// it, its close logs the kept Resolve and receipt again, and replay of the run, the delete
/// and the close gives no turn or preparation of the session and keeps the other two.
#[test]
fn a_delete_replays_to_no_turn_or_preparation_and_keeps_the_resolve_and_receipt() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    session_with_turn(&tx);
    let mut log = Appender::own(&tx, engine()).unwrap();
    assert_eq!(close(&mut log).unwrap(), 4);
    let run = snapshot(&mut log, 9).unwrap();
    drop(log);
    tx.commit().unwrap();
    assert_eq!(
        chunk_rows(&db, "master_turns"),
        [1],
        "the run carries the turn"
    );

    let tx = db.transaction().unwrap();
    release_session_turns(&tx, "s1").unwrap();
    tx.execute("DELETE FROM master_preparations WHERE session='s1'", [])
        .unwrap();
    tx.execute("DELETE FROM master_turns WHERE session='s1'", [])
        .unwrap();
    let mut log = Appender::own(&tx, engine()).unwrap();
    let deleted = serde_json::to_vec(&json!({"project": "p1", "session": "s1"})).unwrap();
    let seq = log
        .append(&Draft {
            aggregate: Aggregate::Sessions,
            kind: SESSION_DELETED,
            schema: 1,
            at_ms: 1,
            body: &deleted,
        })
        .unwrap();
    assert_eq!(
        redact_matching(
            &tx,
            &[Aggregate::Sessions],
            None,
            "session",
            "s1",
            seq,
            &engine()
        )
        .unwrap(),
        4,
        "the four earlier events name the session"
    );
    mark_session_rest(&tx, "p1", "s1").unwrap();
    assert_eq!(close(&mut log).unwrap(), 2);
    drop(log);
    tx.commit().unwrap();
    assert!(seq > run);
    assert_eq!(kinds(&db).last().unwrap(), KIND_PROFILE);
    assert!(!held(&db, "master_turns/5/body"));
    assert!(!held(&db, "master_preparations/5/body"));
    assert!(held(&db, "master_waiting_resolves/s1/body"));
    assert!(held(&db, "configured_master_profiles/reg-1/body"));
    replays_to_live(&mut db);
    let turns: i64 = db
        .query_row("SELECT COUNT(*) FROM master_turns", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        turns, 0,
        "replay deletes the session's turns the run carried"
    );
    let kept: i64 = db
        .query_row(
            "SELECT (SELECT COUNT(*) FROM master_waiting_resolves) + (SELECT COUNT(*) FROM configured_master_profiles)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kept, 2, "the Resolve and receipt stay");
}

/// v2.5 L13c: a turn, a preparation or a receipt of no stored session (a cleared demo's
/// leftover) is neither marked nor carried by a snapshot run beside a stored session's
/// rows, and no text of it is kept.
#[test]
fn rows_of_no_stored_session_are_neither_marked_nor_snapshotted() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    session_with_turn(&tx);
    put_turn(&tx, "ghost", 1, "turn-9", 9, "succeeded");
    put_preparation(&tx, "ghost", "turn-9");
    put_profile(&tx, "reg-9", "ghost", "cred-9");
    mark_profiles(&tx).unwrap();
    assert_eq!(left_marks(&tx), 4, "the stored session's four rows only");
    let mut log = Appender::own(&tx, engine()).unwrap();
    snapshot(&mut log, 9).unwrap();
    drop(log);
    tx.commit().unwrap();
    for table in [
        "master_turns",
        "master_preparations",
        "configured_master_profiles",
    ] {
        assert_eq!(chunk_rows(&db, table), [1], "{table}");
    }
    assert!(held(&db, "master_turns/5/body"));
    assert!(!held(&db, "master_turns/9/body"));
    assert!(!held(&db, "master_preparations/9/body"));
    assert!(!held(&db, "configured_master_profiles/reg-9/body"));
}

/// v2.5 L13c upgrade row: the step gives an older `sessions_touched` the seven-table check,
/// keeping its marks, with no run when the four tables are empty; with a turn written
/// before the log it appends one full run at one revision, and never a second one.
#[test]
fn the_turns_step_widens_the_marks_table_and_carries_pre_log_rows_once() {
    let mut db = unhosted();
    let tx = db.transaction().unwrap();
    tx.execute_batch(
        "CREATE TABLE sessions_touched (
            tbl TEXT NOT NULL CHECK (tbl IN ('master_sessions','conversation_entries','agent_texts')),
            key TEXT NOT NULL CHECK (length(key) BETWEEN 1 AND 128),
            project TEXT NOT NULL CHECK (length(project) BETWEEN 1 AND 128),
            PRIMARY KEY (tbl, key));
         INSERT INTO sessions_touched VALUES ('agent_texts','7','p1');",
    )
    .unwrap();
    assert!(
        !first_run_turns(&tx, Opening::for_test(3)).unwrap(),
        "no turn row, no run"
    );
    write_session(&tx, &view("p1", "s1", 3, "parked"), 2);
    put_turn(&tx, "s1", 1, "turn-1", 5, "succeeded");
    assert_eq!(
        left_marks(&tx),
        2,
        "the old mark stays and a turn mark fits"
    );
    tx.execute("DELETE FROM sessions_touched", []).unwrap();
    assert!(first_run_turns(&tx, Opening::for_test(3)).unwrap());
    assert!(
        !first_run_turns(&tx, Opening::for_test(4)).unwrap(),
        "the turns are on the log"
    );
    tx.commit().unwrap();
    let mut run = vec![SNAPSHOT_CHUNK; TABLES.len()];
    run.push(SNAPSHOT_DONE);
    assert_eq!(kinds(&db), run);
    let revision: i64 = db
        .query_row("SELECT revision FROM state WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(revision, 1, "one run, one revision");
    assert!(held(&db, "master_turns/5/body"));
    replays_to_live(&mut db);
}

/// v2.5 L13c and L1-N1: replay reads the six kinds only with exactly their fields; a zero
/// or out-of-range ordinal, a zero input, an unknown phase, an id with a space, text where
/// a digest goes, a key more, and an empty or oversized dropped list are refused.
#[test]
fn replay_refuses_a_turns_event_it_does_not_know() {
    let sessions = |kind: &str, body: serde_json::Value| Logged {
        seq: 1,
        revision: 1,
        aggregate: Aggregate::Sessions,
        kind: kind.into(),
        schema: 1,
        at_ms: 1,
        body: serde_json::to_vec(&body).unwrap(),
    };
    let digest = "0".repeat(64);
    let turn = json!({"project": "p1", "session": "s1", "ordinal": 1, "id": "turn-1",
        "input": 5, "phase": "queued", "body": digest});
    let preparation = json!({"project": "p1", "session": "s1", "turn": "turn-1", "input": 5,
        "body": digest});
    let resolve = json!({"project": "p1", "session": "s1", "operation": "resolve-1",
        "input": 6, "body": digest});
    let profile = json!({"project": "p1", "session": "s1", "id": "reg-1",
        "profile": "cred-1", "body": digest});
    for (kind, body) in [
        (KIND_TURN, turn.clone()),
        (KIND_PREPARATION, preparation.clone()),
        (KIND_RESOLVE, resolve.clone()),
        (
            KIND_RESOLVES_DROPPED,
            json!({"project": "p1", "sessions": ["s1"]}),
        ),
        (KIND_PROFILE, profile.clone()),
        (
            KIND_PROFILES_DROPPED,
            json!({"project": "p1", "ids": ["reg-1"]}),
        ),
    ] {
        assert!(decode(&sessions(kind, body)).is_ok(), "{kind}");
    }
    let with = |base: &serde_json::Value, key: &str, value: serde_json::Value| {
        let mut changed = base.clone();
        changed[key] = value;
        changed
    };
    let refused = [
        (KIND_TURN, with(&turn, "ordinal", json!(0))),
        (KIND_TURN, with(&turn, "ordinal", json!(1025))),
        (KIND_TURN, with(&turn, "input", json!(0))),
        (KIND_TURN, with(&turn, "phase", json!("done"))),
        (KIND_TURN, with(&turn, "id", json!("turn 1"))),
        (KIND_TURN, with(&turn, "body", json!(SECRET))),
        (KIND_TURN, with(&turn, "text", json!(digest))),
        (KIND_PREPARATION, with(&preparation, "input", json!(0))),
        (KIND_RESOLVE, with(&resolve, "input", json!(0))),
        (KIND_RESOLVE, with(&resolve, "body", json!(SECRET))),
        (KIND_PROFILE, with(&profile, "body", json!(SECRET))),
        (
            KIND_RESOLVES_DROPPED,
            json!({"project": "p1", "sessions": []}),
        ),
        (
            KIND_RESOLVES_DROPPED,
            json!({"project": "p1", "sessions": vec!["s1"; turns::NAMES_MAX + 1]}),
        ),
        (KIND_PROFILES_DROPPED, json!({"project": "p1", "ids": []})),
    ];
    let invalid = Some("sessions_log_event_invalid".to_owned());
    for (kind, body) in refused {
        assert_eq!(
            code(decode(&sessions(kind, body.clone()))),
            invalid,
            "{kind} {body}"
        );
    }
}
