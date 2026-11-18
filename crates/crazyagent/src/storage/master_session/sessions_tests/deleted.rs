use super::*;
use crate::master_session::{SESSION_DELETED, SessionDeleted};
use crate::state_log::fence::Fence;
use crate::state_log::replay::replay;
use crate::state_log::{Aggregate, Appender, Draft, TOMBSTONE};
use serde_json::json;

const TEAM: &str = "SELECT COUNT(*) FROM team_master_conversations WHERE session=?1";

type LogRow = (i64, i64, String, String, Vec<u8>);

/// Parks a used session behind a fresh live one and keeps a harness conversation for it
/// whose record is not on disk, so its Delete has a team conversation row to drop. Input:
/// the case. Output: the parked session and the live one.
async fn parked_with_conversation(case: &Case) -> (SessionId, SessionId) {
    assert_eq!(case.open("open-parked").await, OwnerResult::Applied);
    let parked = case.shown().await.session_id;
    assert_eq!(
        case.submit("parked-line", &parked, "Write the import guide")
            .await,
        OwnerResult::Applied
    );
    assert_eq!(case.renew("renew-parked", true).await, OwnerResult::Applied);
    let live = case.shown().await.session_id;
    let home = crate::team::runtime::agent_home(&case.root.path().join("team"), 4);
    let folder = home.join(".claude/projects/-session-repository");
    std::fs::create_dir_all(&folder).unwrap();
    keep_conversation(
        case,
        &parked,
        4,
        "conversation-parked",
        &folder.join("conversation-parked.jsonl"),
    );
    (parked, live)
}

/// Every logged event as seq, revision, aggregate, kind and body, in seq order; none in a
/// store whose log was never written. Input: the case. Output: the events.
fn log_rows(case: &Case) -> Vec<LogRow> {
    let db = case.db();
    let present: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='state_log')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if !present {
        return Vec::new();
    }
    let mut statement = db
        .prepare("SELECT seq, revision, aggregate, kind, body FROM state_log ORDER BY seq")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// The body logged at one seq. Inputs: the events and the seq. Output: the body.
fn body_at(rows: &[LogRow], seq: u64) -> Vec<u8> {
    rows.iter()
        .find(|row| row.0 as u64 == seq)
        .map(|row| row.4.clone())
        .unwrap()
}

/// Plants events of kind `Noted` in the log through the engine host's own appender, as
/// other writers leave them before a delete. Inputs: the case and each event's aggregate
/// and body. Output: their seqs.
fn plant(case: &Case, events: &[(Aggregate, serde_json::Value)]) -> Vec<u64> {
    let mut db = case.db();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx, Fence::Engine(case.started.host.clone())).unwrap();
    let seqs = events
        .iter()
        .map(|(aggregate, body)| {
            let body = serde_json::to_vec(body).unwrap();
            log.append(&Draft {
                aggregate: *aggregate,
                kind: "Noted",
                schema: 1,
                at_ms: 1,
                body: &body,
            })
            .unwrap()
        })
        .collect();
    drop(log);
    tx.commit().unwrap();
    seqs
}

/// Sends one Delete through the storage actor and hands back the engine's answer as it is,
/// an error included, where the case's own call unwraps it. Inputs: the case, the request
/// id and the session. Output: the answer.
async fn raw_delete(
    case: &Case,
    id: &str,
    session: &SessionId,
) -> Result<OwnerApplied, EngineError> {
    let basis = case.read(OwnerRead::Capabilities).await;
    let (send, receive) = oneshot::channel();
    case.storage
        .sender
        .try_send(Job::OwnerApply(
            case.started.owners[0].admission.clone(),
            OwnerRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: RequestId::new(id).unwrap(),
                expected_revision: basis.revision,
                operation: OwnerOperation::Apply {
                    mutation: OwnerMutation::ProjectMaster {
                        action: MasterOwnerAction::DeleteSession {
                            project_id: case.project.clone(),
                            session_id: session.clone(),
                        },
                    },
                },
            },
            send,
        ))
        .unwrap_or_else(|_| panic!("owner apply queue unavailable"));
    receive.await.unwrap()
}

/// Whether a file holds the marker bytes anywhere. Inputs: the path and the marker.
/// Output: true when found.
fn holds(path: &Path, marker: &[u8]) -> bool {
    std::fs::read(path)
        .unwrap()
        .windows(marker.len())
        .any(|window| window == marker)
}

#[tokio::test]
async fn deleting_a_session_keeps_the_learning_tick_its_owner_messages_gave() {
    let case = Case::new().await;
    assert_eq!(case.open("open-first").await, OwnerResult::Applied);
    let first = case.shown().await;
    assert_eq!(
        case.submit("first-line", &first.session_id, "Write the import guide")
            .await,
        OwnerResult::Applied
    );
    assert_eq!(case.renew("renew-first", true).await, OwnerResult::Applied);
    let master_done = |case: &Case| {
        let mut db = case.db();
        let tx = db.transaction().unwrap();
        crate::learning::progress(&tx)
            .unwrap()
            .steps
            .iter()
            .any(|step| step.id == crate::learning::StepId::Master && step.done())
    };
    assert!(master_done(&case));
    assert_eq!(
        case.delete("delete-first", &first.session_id).await,
        OwnerResult::Applied
    );
    let messages: i64 = case
        .db()
        .query_row(
            "SELECT COUNT(*) FROM conversation_entries WHERE kind='message'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(messages, 0, "the deleted session's messages are gone");
    assert!(master_done(&case), "the tick outlives the messages");
    let kept: i64 = case
        .db()
        .query_row(
            "SELECT COUNT(*) FROM learning_marks WHERE step='master'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kept, 1, "the delete kept the master step as a mark");
    case.close().await;
}

/// L12: an owner Delete logs one SessionDeleted at the request's revision naming only the
/// project and the session, the team projection drops the session's kept conversation in
/// the same transaction, and a replay of the log drops a stale row again while a replay
/// that skips the event keeps it.
#[tokio::test]
async fn a_deleted_session_leaves_no_team_conversation_row_after_a_replay_of_the_log() {
    let case = Case::new().await;
    let (parked, _) = parked_with_conversation(&case).await;
    assert_eq!(count(&case, TEAM, &parked), 1);
    assert_eq!(
        case.delete("delete-parked", &parked).await,
        OwnerResult::Applied
    );
    assert_eq!(count(&case, TEAM, &parked), 0);
    let deleted: Vec<LogRow> = log_rows(&case)
        .into_iter()
        .filter(|row| row.3 == SESSION_DELETED)
        .collect();
    assert_eq!(deleted.len(), 1, "one event per delete");
    let (seq, revision, aggregate, _, body) = &deleted[0];
    let state: i64 = case
        .db()
        .query_row("SELECT revision FROM state WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(*revision, state, "logged at the request's revision");
    assert_eq!(aggregate, "sessions");
    let expected = SessionDeleted {
        project: case.project.clone(),
        session: parked.clone(),
    };
    assert_eq!(body, &serde_json::to_vec(&expected).unwrap());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(body).unwrap(),
        json!({ "project": case.project.as_str(), "session": parked.as_str() }),
        "ids only, at the top level"
    );
    let replayed = |skip: Option<u64>| {
        keep_conversation(
            &case,
            &parked,
            4,
            "conversation-parked",
            Path::new("/stale/record.jsonl"),
        );
        let db = case.db();
        replay(&db, Aggregate::Sessions, 0, |event| {
            if Some(event.seq) == skip {
                return Ok(());
            }
            crate::team::master::project_session_deleted(&db, event)
        })
        .unwrap();
        let left = count(&case, TEAM, &parked);
        db.execute(
            "DELETE FROM team_master_conversations WHERE session=?1",
            [parked.as_str()],
        )
        .unwrap();
        left
    };
    assert_eq!(replayed(None), 0, "a replay drops the stale row");
    assert_eq!(
        replayed(Some(*seq as u64)),
        1,
        "a replay that skips the event keeps it"
    );
    case.close().await;
}

/// L1-N1: a Delete redacts every earlier Sessions and Team event whose top-level session is
/// the deleted one, keeps its own event and every other one, and leaves no copy of a
/// redacted body in the database file or its WAL.
#[tokio::test]
async fn a_delete_redacts_the_earlier_events_naming_its_session_and_keeps_the_rest() {
    let case = Case::new().await;
    let (parked, live) = parked_with_conversation(&case).await;
    let marker = "l12-redact-marker-5c1e";
    let planted = plant(
        &case,
        &[
            (
                Aggregate::Sessions,
                json!({ "session": parked.as_str(), "text": marker }),
            ),
            (
                Aggregate::Team,
                json!({ "session": parked.as_str(), "turn": 1 }),
            ),
            (Aggregate::Sessions, json!({ "session": live.as_str() })),
            (Aggregate::Worktrees, json!({ "session": parked.as_str() })),
        ],
    );
    let before = log_rows(&case);
    assert_eq!(
        case.delete("delete-parked", &parked).await,
        OwnerResult::Applied
    );
    let after = log_rows(&case);
    for (index, seq) in planted.iter().enumerate() {
        let expected = if index < 2 {
            TOMBSTONE.to_vec()
        } else {
            body_at(&before, *seq)
        };
        assert_eq!(body_at(&after, *seq), expected, "event {index}");
    }
    let own: Vec<&LogRow> = after
        .iter()
        .filter(|row| row.3 == SESSION_DELETED)
        .collect();
    assert_eq!(own.len(), 1);
    assert_ne!(own[0].4, TOMBSTONE, "the delete keeps its own event");
    let file = case.root.path().join("engine.db");
    for path in [file.clone(), file.with_extension("db-wal")] {
        assert!(
            !path.exists() || !holds(&path, marker.as_bytes()),
            "{}",
            path.display()
        );
    }
    case.close().await;
}

/// A Delete the engine rejects, of the live session or of a missing one, logs nothing and
/// redacts nothing.
#[tokio::test]
async fn a_rejected_delete_logs_nothing_and_redacts_nothing() {
    let case = Case::new().await;
    let (parked, live) = parked_with_conversation(&case).await;
    plant(
        &case,
        &[(Aggregate::Sessions, json!({ "session": live.as_str() }))],
    );
    let before = log_rows(&case);
    assert_eq!(
        case.delete("delete-live", &live).await,
        OwnerResult::Rejected {
            code: "master_session_live".into()
        }
    );
    let missing = SessionId::new("no-such-session").unwrap();
    assert_eq!(
        case.delete("delete-missing", &missing).await,
        OwnerResult::Rejected {
            code: "master_session_missing".into()
        }
    );
    assert_eq!(log_rows(&case), before);
    assert_eq!(count(&case, TEAM, &parked), 1);
    case.close().await;
}

/// A Delete whose fence fails, the engine host no longer running in this process, rolls the
/// whole request back: the session, its kept conversation and its turns stay and nothing is
/// logged; the same Delete applies once the host runs again.
#[tokio::test]
async fn a_delete_refused_by_its_fence_changes_nothing() {
    let case = Case::new().await;
    let (parked, _) = parked_with_conversation(&case).await;
    let before = (
        snapshot(&case),
        log_rows(&case),
        count(&case, TEAM, &parked),
    );
    crate::state_log::fence::host_stopped(&case.started.host);
    let refused = raw_delete(&case, "delete-fenced", &parked).await;
    crate::state_log::fence::host_started(&case.started.host);
    let Err(EngineError(code)) = refused else {
        panic!("the delete applied without its fence");
    };
    assert_eq!(code, "master_transaction_rolled_back");
    assert_eq!(
        (
            snapshot(&case),
            log_rows(&case),
            count(&case, TEAM, &parked)
        ),
        before
    );
    assert_eq!(
        case.delete("delete-after", &parked).await,
        OwnerResult::Applied
    );
    case.close().await;
}

/// rv-sec3 #296 N2: a Delete that fails after its redaction, here at the owner record its
/// request writes last, rolls the whole request back, so every earlier event naming the
/// session keeps its body; the same Delete then redacts them once it applies.
#[tokio::test]
async fn a_delete_that_fails_after_its_redaction_leaves_every_earlier_event_whole() {
    let case = Case::new().await;
    let (parked, _) = parked_with_conversation(&case).await;
    let planted = plant(
        &case,
        &[
            (Aggregate::Sessions, json!({ "session": parked.as_str() })),
            (Aggregate::Team, json!({ "session": parked.as_str() })),
        ],
    );
    let before = (
        snapshot(&case),
        log_rows(&case),
        count(&case, TEAM, &parked),
    );
    case.db()
        .execute_batch(
            "CREATE TRIGGER l12_fails_after_redaction BEFORE INSERT ON owner_operations
            BEGIN SELECT RAISE(ABORT, 'l12_injected'); END;",
        )
        .unwrap();
    let failed = raw_delete(&case, "delete-fails", &parked).await;
    case.db()
        .execute_batch("DROP TRIGGER l12_fails_after_redaction;")
        .unwrap();
    let Err(EngineError(code)) = failed else {
        panic!("the delete applied past its injected failure");
    };
    assert_eq!(code, "master_transaction_rolled_back");
    assert_eq!(
        (
            snapshot(&case),
            log_rows(&case),
            count(&case, TEAM, &parked)
        ),
        before
    );
    assert_eq!(
        case.delete("delete-after", &parked).await,
        OwnerResult::Applied
    );
    let after = log_rows(&case);
    for seq in &planted {
        assert_eq!(body_at(&after, *seq), TOMBSTONE, "{seq}");
    }
    case.close().await;
}

/// Deleting a deleted session again applies, as before L12: it logs a second SessionDeleted
/// and redacts the first, since that one names the session too, and replay still drops the
/// kept conversation.
#[tokio::test]
async fn deleting_a_deleted_session_again_logs_a_second_event_and_redacts_the_first() {
    let case = Case::new().await;
    let (parked, _) = parked_with_conversation(&case).await;
    for id in ["delete-first", "delete-again"] {
        assert_eq!(case.delete(id, &parked).await, OwnerResult::Applied, "{id}");
    }
    let deleted: Vec<LogRow> = log_rows(&case)
        .into_iter()
        .filter(|row| row.3 == SESSION_DELETED)
        .collect();
    assert_eq!(deleted.len(), 2);
    assert_eq!(deleted[0].4, TOMBSTONE);
    assert_ne!(deleted[1].4, TOMBSTONE);
    assert_eq!(count(&case, TEAM, &parked), 0);
    case.close().await;
}

/// The master sessions, conversation entries, turns, preparations, waiting Resolves and
/// profile receipts as they stand, then as a replay of the sessions events rebuilds them,
/// both read in one write transaction that is rolled back. Input: the case. Output: the
/// live rows and the rebuilt rows.
fn live_and_rebuilt(
    case: &Case,
) -> (
    Vec<Vec<Vec<rusqlite::types::Value>>>,
    Vec<Vec<Vec<rusqlite::types::Value>>>,
) {
    let mut db = case.db();
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let read = |tx: &rusqlite::Transaction<'_>| {
        [
            "master_sessions",
            "conversation_entries",
            "master_turns",
            "master_preparations",
            "master_waiting_resolves",
            "configured_master_profiles",
        ]
        .map(|table| crate::state_log::replay::rows(tx, table).unwrap())
        .to_vec()
    };
    let live = read(&tx);
    crate::sessions_log::rebuild(&tx).unwrap();
    let rebuilt = read(&tx);
    tx.rollback().unwrap();
    (live, rebuilt)
}

/// The logged sessions events of one kind whose body is not a tombstone, as seq and JSON
/// body. Inputs: the events and the kind. Output: the pairs.
fn kept_of(rows: &[LogRow], kind: &str) -> Vec<(i64, serde_json::Value)> {
    rows.iter()
        .filter(|row| row.2 == "sessions" && row.3 == kind && row.4 != TOMBSTONE)
        .map(|row| (row.0, serde_json::from_slice(&row.4).unwrap()))
        .collect()
}

/// v2.5 L13: every owner write of a session and its conversation, an open, a message and a
/// new session parking the used one, reaches the log at its request's revision, the message
/// as a digest, and a replay of the log rebuilds both tables as they stand.
#[tokio::test]
async fn every_owner_session_write_reaches_the_log_and_replays_to_the_live_rows() {
    let case = Case::new().await;
    let (parked, live) = parked_with_conversation(&case).await;
    assert_eq!(
        case.submit("live-line", &live, "Write the release notes")
            .await,
        OwnerResult::Applied
    );
    let rows = log_rows(&case);
    let saved = kept_of(&rows, crate::sessions_log::KIND_SESSION);
    for session in [&parked, &live] {
        assert!(
            saved
                .iter()
                .any(|(_, body)| body["session"] == session.as_str()),
            "{session:?} is logged"
        );
    }
    let entries = kept_of(&rows, crate::sessions_log::KIND_ENTRY);
    assert!(entries.len() >= 2, "{entries:?}");
    let turns = kept_of(&rows, crate::sessions_log::KIND_TURN);
    assert!(
        turns
            .iter()
            .any(|(_, body)| body["session"] == live.as_str()),
        "the submitted turn is logged (v2.5 L13c): {turns:?}"
    );
    assert!(
        rows.iter()
            .all(|row| !String::from_utf8_lossy(&row.4).contains("release notes")),
        "no prompt text in the log"
    );
    let (live_rows, rebuilt) = live_and_rebuilt(&case);
    assert!(!live_rows[0].is_empty() && !live_rows[1].is_empty() && !live_rows[2].is_empty());
    assert_eq!(rebuilt, live_rows, "replay equals the live tables");
    case.close().await;
}

/// v2.5 L13 with L12: a Delete logs the session's final save after its SessionDeleted and
/// redaction, so only that save names the session, logs its turns' messages dropped, and a
/// replay equals the live tables; a second Delete of the closed session does the same.
#[tokio::test]
async fn a_delete_logs_the_final_save_after_its_redaction_and_drops_the_session_s_messages() {
    let case = Case::new().await;
    let (parked, _) = parked_with_conversation(&case).await;
    let inputs: Vec<u64> = {
        let db = case.db();
        let mut statement = db
            .prepare("SELECT input FROM master_turns WHERE session=?1 ORDER BY input")
            .unwrap();
        statement
            .query_map([parked.as_str()], |row| row.get::<_, i64>(0))
            .unwrap()
            .map(|input| input.unwrap() as u64)
            .collect()
    };
    assert!(!inputs.is_empty(), "the parked session has a message");
    for id in ["delete-parked", "delete-again"] {
        assert_eq!(case.delete(id, &parked).await, OwnerResult::Applied, "{id}");
        let rows = log_rows(&case);
        let deleted = kept_of(&rows, SESSION_DELETED);
        assert_eq!(deleted.len(), 1, "{id}");
        let saves: Vec<i64> = kept_of(&rows, crate::sessions_log::KIND_SESSION)
            .into_iter()
            .filter(|(_, body)| body["session"] == parked.as_str())
            .map(|(seq, _)| seq)
            .collect();
        assert_eq!(
            saves.len(),
            1,
            "{id}: only the final save names the session"
        );
        assert!(saves[0] > deleted[0].0, "{id}: logged after the delete");
        let (live_rows, rebuilt) = live_and_rebuilt(&case);
        assert_eq!(rebuilt, live_rows, "{id}: replay equals the live tables");
    }
    let dropped: Vec<u64> = kept_of(&log_rows(&case), crate::sessions_log::KIND_ENTRIES_DROPPED)
        .into_iter()
        .flat_map(|(_, body)| {
            body["cursors"]
                .as_array()
                .unwrap()
                .iter()
                .map(|cursor| cursor.as_u64().unwrap())
                .collect::<Vec<_>>()
        })
        .collect();
    for input in &inputs {
        assert!(dropped.contains(input), "message {input} is logged dropped");
    }
    case.close().await;
}

/// v2.5 L13: a master request the engine rejects, a message to a parked session, logs no
/// event.
#[tokio::test]
async fn a_rejected_master_request_logs_no_sessions_event() {
    let case = Case::new().await;
    let (parked, _) = parked_with_conversation(&case).await;
    let before = log_rows(&case);
    let refused = case
        .submit("parked-again", &parked, "Write the import guide")
        .await;
    assert!(
        matches!(refused, OwnerResult::Rejected { .. }),
        "{refused:?}"
    );
    assert_eq!(log_rows(&case), before);
    case.close().await;
}
