use crazyagent::protocol::{RequestId, SessionId, encode_frame};
use crazyagent::rcli_host::{Event, EventSequence, read_event};
use serde_json::{Value, json};
use std::io::Cursor;

fn packet(sequence: u64, body: Value) -> Value {
    json!({
        "protocol_version": 1,
        "session_id": "session-a",
        "instance_id": "instance-a",
        "sequence": sequence,
        "event": body
    })
}

#[test]
fn all_message_operations_round_trip_through_real_rust_framing() {
    let bodies = [
        json!({"kind":"message_started","turn_id":"turn-a","message_id":"msg-1"}),
        json!({"kind":"message_delta","turn_id":"turn-a","message_id":"msg-1","text":"draft"}),
        json!({"kind":"message_reset","turn_id":"turn-a","message_id":"msg-1"}),
        json!({"kind":"message_discarded","turn_id":"turn-a","message_id":"msg-1"}),
        json!({"kind":"message_finished","turn_id":"turn-a","message_id":"msg-1"}),
    ];
    for (index, body) in bodies.into_iter().enumerate() {
        let value = packet((index + 1) as u64, body);
        let bytes = encode_frame(&value).unwrap();
        let decoded = read_event(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
    }
}

#[test]
fn delta_byte_bounds_preserve_unicode_and_whitespace() {
    for text in ["🙂".repeat(4096), " ".repeat(16384), "\n\t".to_owned()] {
        let value = packet(
            1,
            json!({"kind":"message_delta","turn_id":"turn-a","message_id":"msg-1","text":text}),
        );
        let decoded: Event = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
    }
    for text in [String::new(), "x".repeat(16385), "🙂".repeat(4097)] {
        assert!(
            serde_json::from_value::<Event>(packet(
                1,
                json!({"kind":"message_delta","turn_id":"turn-a","message_id":"msg-1","text":text})
            ))
            .is_err()
        );
    }
}

#[test]
fn message_contract_rejects_invalid_ids_unknown_fields_and_wrong_shapes() {
    let rejected = [
        json!({"kind":"message_started","turn_id":"turn-a","message_id":""}),
        json!({"kind":"message_reset","turn_id":"turn-a","message_id":"bad/identifier"}),
        json!({"kind":"message_discarded","turn_id":"turn-a","message_id":"a..b"}),
        json!({"kind":"message_finished","turn_id":"","message_id":"msg-1"}),
        json!({"kind":"message_started","turn_id":"turn-a","message_id":"msg-1","text":"unexpected"}),
        json!({"kind":"message_delta","turn_id":"turn-a","message_id":"msg-1"}),
        json!({"kind":"message_delta","turn_id":"turn-a","message_id":"msg-1","text":"x","extra":true}),
        json!({"kind":"message_unknown","turn_id":"turn-a","message_id":"msg-1"}),
    ];
    for body in rejected {
        assert!(serde_json::from_value::<Event>(packet(1, body)).is_err());
    }
}

#[test]
fn message_frames_share_the_existing_scoped_monotonic_stream() {
    let mut state = EventSequence::new(
        SessionId::new("session-a").unwrap(),
        RequestId::new("instance-a").unwrap(),
    );
    let values = [
        packet(1, json!({"kind":"ready"})),
        packet(
            2,
            json!({"kind":"message_started","turn_id":"turn-a","message_id":"msg-1"}),
        ),
        packet(
            3,
            json!({"kind":"message_delta","turn_id":"turn-a","message_id":"msg-1","text":"x"}),
        ),
        packet(
            4,
            json!({"kind":"message_finished","turn_id":"turn-a","message_id":"msg-1"}),
        ),
        packet(
            5,
            json!({"kind":"completed","turn_id":"turn-a","status":"succeeded"}),
        ),
    ];
    let mut bytes = Vec::new();
    for value in &values {
        bytes.extend(encode_frame(value).unwrap());
    }
    let mut cursor = Cursor::new(bytes);
    for value in values {
        let event = read_event(&mut cursor).unwrap();
        state.accept(&event).unwrap();
        assert_eq!(serde_json::to_value(event).unwrap(), value);
    }
    assert_eq!(state.last_sequence(), 5);
    let duplicate: Event = serde_json::from_value(packet(
        5,
        json!({"kind":"message_reset","turn_id":"turn-a","message_id":"msg-1"}),
    ))
    .unwrap();
    assert!(state.accept(&duplicate).is_err());
    let mut foreign = packet(
        6,
        json!({"kind":"message_started","turn_id":"turn-b","message_id":"msg-2"}),
    );
    foreign["instance_id"] = json!("different-instance");
    let foreign: Event = serde_json::from_value(foreign).unwrap();
    assert!(state.accept(&foreign).is_err());
    assert_eq!(state.last_sequence(), 5);
}
