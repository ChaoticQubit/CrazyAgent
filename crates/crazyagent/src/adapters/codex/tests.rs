use super::*;

/// A line holding more JSON values than one stream line may is refused as capacity
/// before it is built, as the Claude adapter refuses it, though it fits the line length.
#[test]
fn a_line_of_more_values_than_the_bound_is_refused_before_it_is_built() {
    let mut protocol = CodexProtocol::new();
    let values = vec!["0"; super::super::claude::NODES_MAX + 1].join(",");
    let line = format!("{{\"method\":\"item/agentMessage/delta\",\"params\":[{values}]}}\n");
    assert!(line.len() <= LINE_BYTES_MAX);
    assert_eq!(
        protocol.receive(line.as_bytes()),
        Err(ProtocolError::Capacity)
    );
    assert_eq!(protocol.refused(), Some("unreadable"));
}

#[test]
#[should_panic(expected = "assertion failed: self.next_id > 0")]
fn sequence_invariant_detects_a_corrupt_counter() {
    let mut protocol = CodexProtocol::new();
    protocol.next_id = 0;
    protocol.assert_consistent();
}

#[test]
#[should_panic(expected = "assertion failed: *id > 0 && *id < self.next_id")]
fn pending_identity_invariant_detects_a_reused_counter() {
    let mut protocol = CodexProtocol::new();
    protocol.initialize().unwrap();
    protocol.next_id = 1;
    protocol.assert_consistent();
}

#[test]
#[should_panic(expected = "assertion failed: self.seen_turns.contains(&turn.id)")]
fn turn_membership_invariant_detects_an_untracked_turn() {
    let mut protocol = CodexProtocol::new();
    protocol.thread_id = Some("thread-unit".into());
    protocol.turn = Some(Turn {
        id: "orphan-unit".into(),
        status: TurnStatus::InProgress,
        _items: Vec::new(),
    });
    protocol.assert_consistent();
}

#[test]
#[should_panic(expected = "assertion failed: self.server_requests.keys()")]
fn request_membership_invariant_detects_an_id_the_wire_never_admits() {
    let mut protocol = CodexProtocol::new();
    let long = "x".repeat(ID_BYTES_MAX + 1);
    protocol.server_requests.insert(WireId::Text(long), None);
    protocol.assert_consistent();
}

#[test]
#[should_panic(expected = "assertion failed: self.interrupts")]
fn child_interrupt_invariant_detects_an_interrupt_for_an_unknown_child() {
    let mut protocol = CodexProtocol::new();
    protocol.next_id = 2;
    let orphan = "orphan-child".to_owned();
    protocol.children.interrupts.insert(1, orphan);
    protocol.assert_consistent();
}

/// The sub-agent thread store is bounded by bytes (OB-5 r2): of 5,000 finished threads
/// with ids of the longest kind the oldest are forgotten and the newest kept, a thread
/// an interrupt still waits on is never forgotten, and its answer frees it.
#[test]
fn finished_sub_agent_threads_are_forgotten_past_the_byte_bound_and_waiting_ones_never() {
    let mut children = Children::default();
    let id = |index: usize| format!("{index:0256}");
    children.adopt(id(0)).unwrap();
    children.started(&id(0), "turn-0".into());
    children.interrupt(1, &id(0)).unwrap();
    for index in 1..5_000 {
        children.adopt(id(index)).unwrap();
        children.started(&id(index), format!("turn-{index}"));
        children.ended(&id(index));
    }
    children.check();
    assert!(children.known.len() >= 4_095, "{}", children.known.len());
    assert!(children.known.len() < 5_000);
    assert!(children.contains(&id(0)), "a waiting thread is kept");
    assert!(
        !children.contains(&id(1)),
        "the oldest finished is forgotten"
    );
    assert!(children.contains(&id(4_999)));
    assert_eq!(children.turn(&id(4_999)), Some("turn-4999"));
    assert!(children.answered(1));
    assert!(!children.answered(1));
    children.adopt(id(5_000)).unwrap();
    children.adopt(id(5_001)).unwrap();
    assert!(!children.contains(&id(0)), "answered, so the oldest now");
    children.check();
}

/// Only threads still running can fill the store: with no finished thread to forget,
/// one more thread is refused by the byte bound, and it is taken once one finished.
#[test]
fn only_sub_agent_threads_still_running_can_fill_the_byte_bound() {
    let mut children = Children::default();
    let id = |index: usize| format!("{index:0256}");
    let fits = CHILDREN_BYTES_MAX / Children::thread_cost(&id(0));
    assert!(fits >= 4_096);
    for index in 0..fits {
        children.adopt(id(index)).unwrap();
    }
    assert_eq!(children.adopt(id(fits)), Err(ProtocolError::Capacity));
    assert_eq!(children.known.len(), fits);
    children.ended(&id(7));
    children.adopt(id(fits)).unwrap();
    assert!(!children.contains(&id(7)));
    assert_eq!(children.known.len(), fits);
    children.check();
}

#[test]
fn exhausted_counter_never_wraps_or_reserves_a_request() {
    let mut protocol = CodexProtocol::new();
    protocol.next_id = i64::MAX;
    assert_eq!(protocol.initialize(), Err(ProtocolError::Capacity));
    assert_eq!(protocol.state(), State::New);
    assert!(protocol.pending.is_none());
}

/// A run is never refused for how many server requests it made (OB-5e): 10,000 are
/// taken, where the 1,025th used to be refused as over capacity. One sent twice is still
/// refused, a number and a text of the same digits are two ids, and a text id longer
/// than the wire admits is refused.
#[test]
fn remembered_request_ids_have_no_count_bound_and_still_refuse_a_request_sent_twice() {
    let mut protocol = CodexProtocol::new();
    for number in 0..10_000 {
        let id = WireId::Number(number);
        let asked = protocol.server_request(id.clone(), "unsupported".into(), json!({}));
        assert!(asked.is_ok(), "request {number}");
        protocol.deny_request(&id).unwrap();
    }
    assert!(protocol.server_requests.is_empty());
    let again = |protocol: &mut CodexProtocol, id: WireId| {
        protocol.server_request(id, "unsupported".into(), json!({}))
    };
    for number in [0, 5_000, 9_999] {
        let twice = again(&mut protocol, WireId::Number(number));
        assert_eq!(twice, Err(ProtocolError::StaleMessage), "{number}");
    }
    let text = WireId::Text("9999".into());
    assert!(again(&mut protocol, text.clone()).is_ok());
    assert_eq!(
        again(&mut protocol, text),
        Err(ProtocolError::StaleMessage),
        "a request still waiting for its answer"
    );
    let long = WireId::Text("x".repeat(ID_BYTES_MAX + 1));
    assert_eq!(
        again(&mut protocol, long),
        Err(ProtocolError::InvalidMessage)
    );
    protocol.assert_consistent();
}

/// The remembered request ids are bounded by bytes: of 6,000 text ids of 200 bytes the
/// oldest are forgotten, the newest are kept, and the store stays within its bound.
#[test]
fn remembered_request_ids_forget_the_oldest_past_their_byte_bound() {
    let mut seen = SeenRequests::default();
    let id = |index: usize| WireId::Text(format!("{index:0200}"));
    for index in 0..6_000 {
        seen.insert(&id(index));
    }
    seen.check();
    assert!(!seen.contains(&id(0)), "the oldest id was forgotten");
    assert!(seen.contains(&id(5_999)));
    assert!(seen.contains(&id(2_000)), "4,000 ids back is still known");
    assert!(!seen.contains(&WireId::Number(5_999)));
}

/// W1 (farm3, 111 Windows rows): a path Codex sends on Windows is absolute only with a
/// drive prefix, so a plain drive path is valid, as the codexHome of its initialize
/// reply and the cwd of a thread. A UNC, verbatim or device prefix, a forward slash, a
/// relative drive path, `.` or `..` stays refused.
#[cfg(windows)]
#[test]
fn a_plain_drive_path_is_valid_on_windows_and_every_other_form_stays_refused() {
    for plain in [
        r"C:\Users\unit\.codex",
        r"c:\unit\worktree",
        r"D:\work\project",
    ] {
        assert_eq!(valid_path(Path::new(plain)), Some(plain), "{plain}");
    }
    for refused in [
        r"C:\",
        r"C:unit\codex",
        r"\unit\codex",
        "/unit/codex",
        r"C:/unit/codex",
        r"C:\unit/codex",
        r"C:\unit\..\escape",
        r"C:\unit\.\codex",
        r"C:\unit\codex\",
        r"\\server\share\codex",
        r"\\?\C:\codex",
        r"\\.\pipe\codex",
    ] {
        assert_eq!(valid_path(Path::new(refused)), None, "{refused}");
    }
    let mut protocol = CodexProtocol::new();
    protocol.initialize().unwrap();
    let reply = json!({"id":1,"result":{
        "userAgent":"unit-wire-data", "codexHome":r"C:\Users\unit\.codex",
        "platformFamily":"windows", "platformOs":"windows"
    }});
    let mut line = serde_json::to_vec(&reply).unwrap();
    line.push(b'\n');
    assert_eq!(protocol.receive(&line).unwrap(), vec![Event::Initialized]);
    protocol.initialized().unwrap();
    assert!(
        protocol
            .start_thread(Path::new(r"C:\unit\worktree"))
            .is_ok()
    );
}
