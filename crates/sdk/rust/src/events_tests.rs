use serde_json::{Value, json};

/// The stored body of an `AgentQuitSaved` event, as the engine's settings log writes it.
const QUIT_BODY: &[u8] = br#"{"settings":{"on_quit":"stop","left_running":"never","hours":12}}"#;

#[test]
fn rust_event_to_wire_keeps_unmapped_bodies_byte_equal() {
    let wire = crate::generated::envelope::settings::event_to_wire("AgentQuitSaved", QUIT_BODY)
        .expect("settings emits AgentQuitSaved")
        .expect("the stored body converts");
    let stored: Value = serde_json::from_slice(QUIT_BODY).expect("the stored body is JSON");
    assert_eq!(wire, stored);
    assert_eq!(
        serde_json::to_string(&wire).expect("encodes"),
        serde_json::to_string(&stored).expect("encodes")
    );
}

#[test]
fn rust_event_to_wire_encodes_u64_and_bytes() {
    let body = br#"{"at":18446744073709551615,"data":[0,1,255],"blob":{"size":7,"data":[],"parts":[0,9007199254740993],"tail":[102]}}"#;
    let wire = crate::fixture_wire::envelope::wire::event_to_wire("BlobStored", body)
        .expect("wire emits BlobStored")
        .expect("the stored body converts");
    assert_eq!(
        wire,
        json!({
            "at": "18446744073709551615",
            "data": "AAH/",
            "blob": {"size": "7", "data": "", "parts": ["0", "9007199254740993"], "tail": "Zg=="},
        })
    );
}

#[test]
fn rust_event_to_wire_refuses_a_bad_body_and_ignores_a_foreign_kind() {
    let settings = crate::generated::envelope::settings::event_to_wire;
    for body in [
        &br#"{}"#[..],
        br#"{"settings":{"on_quit":"stop","left_running":"never"}}"#,
        br#"{"settings":{"on_quit":"stop","left_running":"never","hours":12},"extra":1}"#,
        br#"{"settings":{"on_quit":"stop","left_running":"never","hours":-1}}"#,
        b"not json",
        b"",
    ] {
        let answer = settings("AgentQuitSaved", body).expect("settings emits AgentQuitSaved");
        assert!(answer.is_err(), "{}", String::from_utf8_lossy(body));
    }
    assert!(
        crate::generated::envelope::secrets::event_to_wire("AgentQuitSaved", QUIT_BODY).is_none()
    );
    assert!(settings("SecretPut", QUIT_BODY).is_none());
    let wire = crate::fixture_wire::envelope::wire::event_to_wire;
    assert!(
        wire("BlobStored", br#"{"at":-1,"data":[],"blob":null}"#)
            .expect("emitted")
            .is_err()
    );
    assert!(wire("AgentQuitSaved", QUIT_BODY).is_none());
}
