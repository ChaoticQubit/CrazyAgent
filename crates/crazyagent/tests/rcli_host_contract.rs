use crazyagent::protocol::{FRAME_BYTES_MAX, RequestId, SessionId, encode_frame};
use crazyagent::rcli_host::{Control, Event, EventSequence, encode_control, read_event};
use serde_json::{Value, json};
use std::io::Cursor;

fn control(body: Value) -> Value {
    json!({
        "protocol_version": 1,
        "session_id": "session-a",
        "request_id": "request-a",
        "control": body
    })
}

fn event(sequence: u64, body: Value) -> Event {
    serde_json::from_value(json!({
        "protocol_version": 1,
        "session_id": "session-a",
        "instance_id": "instance-a",
        "sequence": sequence,
        "event": body
    }))
    .unwrap()
}

fn sequence() -> EventSequence {
    EventSequence::new(
        SessionId::new("session-a").unwrap(),
        RequestId::new("instance-a").unwrap(),
    )
}

#[test]
fn every_closed_control_round_trips_through_the_existing_frame_format() {
    let bodies = [
        json!({"kind":"prompt","turn_id":"turn-a","text":"Inspect this project"}),
        json!({"kind":"steer","turn_id":"turn-a","text":"Preserve the tests"}),
        json!({"kind":"cancel","turn_id":"turn-a"}),
        json!({"kind":"answer","turn_id":"turn-a","prompt_id":"prompt-a","answers":["A"]}),
        json!({"kind":"shutdown"}),
    ];
    for body in bodies {
        let value = control(body);
        let request: Control = serde_json::from_value(value.clone()).unwrap();
        let bytes = encode_control(&request).unwrap();
        assert_eq!(bytes, encode_frame(&request).unwrap());
        assert_eq!(serde_json::from_slice::<Value>(&bytes[4..]).unwrap(), value);
        assert_eq!(serde_json::to_value(&request).unwrap(), value);
    }
}

#[test]
fn serde_cannot_bypass_text_identity_version_or_answer_bounds() {
    let maximum = "🦀".repeat(4096);
    let valid = control(json!({"kind":"prompt","turn_id":"turn-a","text":maximum}));
    assert!(serde_json::from_value::<Control>(valid.clone()).is_ok());
    for text in [String::new(), " ".into(), "🦀".repeat(4097)] {
        let mut invalid = valid.clone();
        invalid["control"]["text"] = json!(text);
        assert!(serde_json::from_value::<Control>(invalid).is_err());
    }
    for field in ["session_id", "request_id"] {
        let mut invalid = valid.clone();
        invalid[field] = json!("../another-scope");
        assert!(serde_json::from_value::<Control>(invalid).is_err());
    }
    let mut invalid = valid;
    invalid["protocol_version"] = json!(2);
    assert!(serde_json::from_value::<Control>(invalid).is_err());
    for answers in [
        json!([]),
        json!([""]),
        json!(vec!["a"; 9]),
        json!(["x".repeat(1025)]),
    ] {
        let value = control(json!({
            "kind":"answer","turn_id":"turn-a","prompt_id":"prompt-a","answers":answers
        }));
        assert!(serde_json::from_value::<Control>(value).is_err());
    }
}

#[test]
fn attachment_credential_backend_and_owner_controls_are_rejected() {
    let valid = control(json!({"kind":"prompt","turn_id":"turn-a","text":"Inspect"}));
    for key in [
        "attachments",
        "api_key",
        "provider",
        "owner_grant",
        "profile_ref",
    ] {
        let mut outer = valid.clone();
        outer[key] = json!("untrusted");
        assert!(serde_json::from_value::<Control>(outer).is_err());
        let mut inner = valid.clone();
        inner["control"][key] = json!("untrusted");
        assert!(serde_json::from_value::<Control>(inner).is_err());
    }
    for kind in [
        "approve_baseline",
        "approve_plan",
        "claim_runner",
        "launch",
        "shell",
    ] {
        assert!(serde_json::from_value::<Control>(control(json!({"kind":kind}))).is_err());
    }
}

#[test]
fn event_reader_consumes_one_exact_frame_and_leaves_the_next_intact() {
    let ready = event(1, json!({"kind":"ready"}));
    let output = event(
        2,
        json!({"kind":"output","turn_id":"turn-a","text":"Observed output"}),
    );
    let first = encode_frame(&ready).unwrap();
    let second = encode_frame(&output).unwrap();
    let mut input = Cursor::new([first.clone(), second.clone()].concat());
    assert_eq!(read_event(&mut input).unwrap(), ready);
    assert_eq!(input.position(), first.len() as u64);
    assert_eq!(read_event(&mut input).unwrap(), output);
    assert_eq!(input.position(), (first.len() + second.len()) as u64);
    assert!(read_event(&mut input).is_err());
}

#[test]
fn oversized_empty_truncated_and_non_event_frames_fail_without_false_success() {
    for size in [0_u32, FRAME_BYTES_MAX as u32 + 1, u32::MAX] {
        let mut input = Cursor::new([size.to_be_bytes().as_slice(), b"payload"].concat());
        assert!(read_event(&mut input).is_err());
        assert_eq!(input.position(), 4);
    }
    let bytes = encode_frame(&event(1, json!({"kind":"ready"}))).unwrap();
    for length in [0, 1, 3, 4, bytes.len() - 1] {
        assert!(read_event(&mut Cursor::new(&bytes[..length])).is_err());
    }
    let request = control(json!({"kind":"shutdown"}));
    assert!(read_event(&mut Cursor::new(encode_frame(&request).unwrap())).is_err());
}

#[test]
fn invalid_sequences_do_not_advance_the_accepted_stream() {
    let mut stream = sequence();
    let ready = event(1, json!({"kind":"ready"}));
    let output = event(
        2,
        json!({"kind":"output","turn_id":"turn-a","text":"First"}),
    );
    assert!(stream.accept(&output).is_err());
    assert_eq!(stream.last_sequence(), 0);
    stream.accept(&ready).unwrap();
    assert!(stream.accept(&ready).is_err());
    assert_eq!(stream.last_sequence(), 1);
    let gap = event(
        3,
        json!({"kind":"completed","turn_id":"turn-a","status":"succeeded"}),
    );
    assert!(stream.accept(&gap).is_err());
    assert_eq!(stream.last_sequence(), 1);
    stream.accept(&output).unwrap();
    stream.accept(&gap).unwrap();
    assert_eq!(stream.last_sequence(), 3);
    assert!(stream.accept(&event(4, json!({"kind":"ready"}))).is_err());
    assert_eq!(stream.last_sequence(), 3);
}

#[test]
fn another_session_or_launch_instance_cannot_rebind_the_stream() {
    for field in ["session_id", "instance_id"] {
        let mut stream = sequence();
        let ready = event(1, json!({"kind":"ready"}));
        let mut value = serde_json::to_value(&ready).unwrap();
        value[field] = json!("different-a");
        let wrong = serde_json::from_value(value).unwrap();
        assert!(stream.accept(&wrong).is_err());
        assert_eq!(stream.last_sequence(), 0);
        stream.accept(&ready).unwrap();
    }
}

#[test]
fn event_deserialization_rejects_unknown_fields_and_unbounded_data() {
    let ready = serde_json::to_value(event(1, json!({"kind":"ready"}))).unwrap();
    for field in ["owner_grant", "api_key", "attachments"] {
        let mut invalid = ready.clone();
        invalid[field] = json!("forbidden");
        assert!(serde_json::from_value::<Event>(invalid).is_err());
    }
    for (field, value) in [("sequence", json!(0)), ("protocol_version", json!(2))] {
        let mut invalid = ready.clone();
        invalid[field] = value;
        assert!(serde_json::from_value::<Event>(invalid).is_err());
    }
    for (index, body) in [
        json!({"kind":"output","turn_id":"turn-a","text":"x".repeat(16385)}),
        json!({"kind":"completed","turn_id":"turn-a","status":"approved"}),
        json!({"kind":"ready","owner_grant":"forbidden"}),
        json!({"kind":"launch","session_id":"session-b"}),
    ]
    .into_iter()
    .enumerate()
    {
        let mut invalid = ready.clone();
        invalid["event"] = body;
        assert!(
            serde_json::from_value::<Event>(invalid).is_err(),
            "case {index}"
        );
    }
}

#[test]
fn payload_free_controls_reject_additional_fields_too() {
    for key in ["owner_grant", "api_key", "attachments"] {
        let mut body = json!({"kind":"shutdown"});
        body[key] = json!("forbidden");
        assert!(
            serde_json::from_value::<Control>(control(body)).is_err(),
            "{key}"
        );
    }
}
