use super::*;
use crate::master_session::MasterActivityLine;

#[test]
#[should_panic(expected = "master turn activity is never stored")]
fn a_turn_read_with_its_live_activity_is_never_saved_back() {
    let db = Connection::open_in_memory().unwrap();
    let tx = db.unchecked_transaction().unwrap();
    let turn = MasterTurnView {
        id: RequestId::new("turn-1").unwrap(),
        session_id: SessionId::new("session-1").unwrap(),
        ordinal: 1,
        input_cursor: Cursor::new(1),
        phase: MasterTurnPhase::Running,
        reason: None,
        attempt: None,
        runtime: None,
        reply: None,
        accounts: Vec::new(),
        tasks: Vec::new(),
        changes: None,
        engine_request: None,
        activity: vec![MasterActivityLine {
            at_ms: 1,
            text: "Reading src/app.ts".into(),
        }],
        detail: None,
        cause: None,
    };
    let _ = save_turn(&tx, &turn);
}
