use crazyagent::protocol::{FRAME_BYTES_MAX, RequestId, SessionId, encode_frame};
use crazyagent::rcli_host::{Control, Event, EventSequence, encode_control, read_event};
use serde_json::{Value, json};
use std::io::Cursor;
use std::process::Command;

fn observation(body: Value) -> Value {
    json!({"protocol_version":1,"session_id":"session-a","instance_id":"instance-a",
        "sequence":2,"event":body})
}

fn tool_observation(payload: Value) -> Value {
    let scope = if payload["type"] == "tool:requested" {
        "explicit_sdk_turn"
    } else {
        "turn_unattributed"
    };
    json!({"kind":"tool_observation","scope":scope,"event":payload})
}

fn usage_observation(usage: Value) -> Value {
    json!({"kind":"usage_observation","scope":"turn_unattributed",
        "provenance":"sdk_reported","accounting":"UNKNOWN","usage":usage})
}

#[test]
fn actual_node_projection_and_codec_frames_round_trip_in_rust() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new("node")
        .args([
            "--experimental-strip-types",
            "scripts/rcli/host.observations.frames.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len() < 262144);
    let mut input = Cursor::new(output.stdout);
    let mut order = sequence();
    let mut count = 0;
    while input.position() < input.get_ref().len() as u64 {
        let start = input.position() as usize;
        let value = read_event(&mut input).unwrap();
        order.accept(&value).unwrap();
        let original: Value =
            serde_json::from_slice(&input.get_ref()[start + 4..input.position() as usize]).unwrap();
        assert_eq!(serde_json::to_value(&value).unwrap(), original);
        let repeated: Event = serde_json::from_slice(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(value, repeated);
        count += 1;
    }
    assert_eq!(count, 16);
    assert_eq!(order.last_sequence(), 16);
}

#[test]
fn usage_zero_decimal_and_absence_survive_without_accounting_inference() {
    for usage in [
        json!({}),
        json!({"costUsd":0}),
        json!({"costUsd":0.000001,"inputTokens":0}),
        json!({"contextLimit":9007199254740991_u64,"provider":"","model":""}),
    ] {
        let original = observation(usage_observation(usage));
        let value: Event = serde_json::from_value(original.clone()).unwrap();
        assert_eq!(serde_json::to_value(value).unwrap(), original);
    }
}

#[test]
fn observation_shapes_scope_and_authority_are_closed() {
    let pending = tool_observation(json!({"type":"tool:pending","toolCallId":"opaque/id",
        "name":"Read","inputSummary":""}));
    let valid: Event = serde_json::from_value(observation(pending.clone())).unwrap();
    assert_eq!(
        serde_json::to_value(valid).unwrap(),
        observation(pending.clone())
    );
    let mut wrong_scope = pending.clone();
    wrong_scope["scope"] = json!("explicit_sdk_turn");
    let mut authority = pending.clone();
    authority["event"]["approved"] = json!(true);
    let mut host_turn = pending;
    host_turn["turn_id"] = json!("guessed");
    for body in [
        wrong_scope,
        authority,
        host_turn,
        usage_observation(json!({"inputTokens":null})),
        usage_observation(json!({"costUsd":-0.5})),
        usage_observation(json!({"costUsd":9007199254740992_u64})),
        usage_observation(json!({"inputTokens":0.5})),
        usage_observation(json!({"remainingQuota":0})),
        json!({"kind":"observation_gap","scope":"turn_unattributed","source_type":"tool:pending","reason":"unsupported_event"}),
        json!({"kind":"observation_gap","scope":"turn_unattributed","source_type":"tool:progress","reason":"unqualified_progress"}),
    ] {
        assert!(
            serde_json::from_value::<Event>(observation(body.clone())).is_err(),
            "{body}"
        );
    }
    for field in ["provenance", "accounting", "scope"] {
        let mut body = usage_observation(json!({}));
        body[field] = json!("estimated");
        assert!(serde_json::from_value::<Event>(observation(body)).is_err());
    }
}

#[test]
fn opaque_tool_input_rejects_duplicate_keys_before_value_erases_them() {
    let prefix = r#"{"protocol_version":1,"session_id":"session-a","instance_id":"instance-a","sequence":2,"event":{"kind":"tool_observation","scope":"explicit_sdk_turn","event":{"type":"tool:requested","turnId":"sdk/turn","calls":[{"id":"id","name":"Read","input":"#;
    for input in [r#"{"nested":{"x":1,"x":2}}"#, r#"[{"x":1,"\u0078":2}]"#] {
        let raw = format!("{prefix}{input}}}]}}}}}}");
        assert!(serde_json::from_str::<Value>(&raw).is_ok());
        assert!(serde_json::from_str::<Event>(&raw).is_err());
    }
    let valid = format!("{prefix}{{\"nested\":{{\"x\":1}}}}}}]}}}}}}");
    assert!(serde_json::from_str::<Event>(&valid).is_ok());
}

#[test]
fn tool_observation_payload_limits_cover_depth_nodes_shape_bytes_and_utf8() {
    let requested = |input: Value| {
        observation(tool_observation(json!({"type":"tool:requested",
        "turnId":"sdk/turn","calls":[{"id":"id","name":"Read","input":input}]})))
    };
    let mut deep = json!(0);
    for _ in 0..5 {
        deep = json!([deep]);
    }
    assert!(serde_json::from_value::<Event>(requested(deep.clone())).is_ok());
    deep = json!([deep]);
    assert!(serde_json::from_value::<Event>(requested(deep)).is_err());
    assert!(serde_json::from_value::<Event>(requested(json!([]))).is_ok());
    let many = Value::Array((0..16).map(|_| Value::Array(vec![json!(0); 64])).collect());
    let mut keys = serde_json::Map::new();
    for n in 0..33 {
        keys.insert(n.to_string(), json!(0));
    }
    for input in [
        many,
        Value::Object(keys),
        json!(vec![0; 65]),
        json!("a".repeat(16385)),
        json!(["a".repeat(16384), "a".repeat(16384)]),
        json!({"a".repeat(257):0}),
        json!(9007199254740992_u64),
    ] {
        assert!(serde_json::from_value::<Event>(requested(input)).is_err());
    }
    let at_limit = tool_observation(json!({"type":"tool:retracted","toolCallId":"é".repeat(128)}));
    assert!(serde_json::from_value::<Event>(observation(at_limit)).is_ok());
    let too_long = tool_observation(json!({"type":"tool:retracted","toolCallId":"é".repeat(129)}));
    assert!(serde_json::from_value::<Event>(observation(too_long)).is_err());
}

#[test]
fn actual_node_number_byte_edges_match_the_rust_tool_budget() {
    let script = r#"
import assert from "node:assert/strict";
import { normalizeSdkObservation } from "./scripts/rcli/host.observations.ts";
const cases = [];
const prefixes = ["x".repeat(16384), '"'.repeat(8192), "é".repeat(8192), "\u0000".repeat(2730) + "aaaa"];
for (const [index, number] of [0, -0, 1, 0.000001, 1e-7, -0.125, 5e-324, 9007199254740991].entries()) {
  const event = { type: "tool:requested", turnId: "t", calls: [
    { id: "i", name: "N", input: ["", "", number] },
  ] };
  const remaining = 32768 - Buffer.byteLength(JSON.stringify(event));
  event.calls[0].input[0] = prefixes[index % prefixes.length];
  assert.equal(Buffer.byteLength(JSON.stringify(event.calls[0].input[0])) - 2, 16384);
  event.calls[0].input[1] = "y".repeat(remaining - 16384);
  assert.equal(Buffer.byteLength(JSON.stringify(event)), 32768);
  cases.push({ event: normalizeSdkObservation(event), accepted: true });
  event.calls[0].input[1] += "y";
  assert.throws(() => normalizeSdkObservation(event), /host_observation_rejected/);
  cases.push({ event: { kind: "tool_observation", scope: "explicit_sdk_turn", event }, accepted: false });
}
process.stdout.write(JSON.stringify(cases));
"#;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new("node")
        .args([
            "--experimental-strip-types",
            "--input-type=module",
            "--eval",
            script,
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len() < 1048576);
    let cases: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(cases.len(), 16);
    for case in cases {
        let original = observation(case["event"].clone());
        let decoded = serde_json::from_value::<Event>(original.clone());
        assert_eq!(decoded.is_ok(), case["accepted"].as_bool().unwrap());
        if let Ok(value) = decoded {
            assert_eq!(serde_json::to_value(value).unwrap(), original);
        }
    }
}

#[test]
fn exactly_1024_tool_values_and_32_keys_fit_without_losing_a_node() {
    let requested = |input: Value| {
        observation(tool_observation(json!({"type":"tool:requested",
        "turnId":"sdk/turn","calls":[{"id":"id","name":"Read","input":input}]})))
    };
    let mut input = vec![Value::Array(vec![json!(0); 64]); 15];
    input.push(Value::Array(vec![json!(0); 40]));
    assert!(serde_json::from_value::<Event>(requested(json!(input))).is_ok());
    input[15].as_array_mut().unwrap().push(json!(0));
    assert!(serde_json::from_value::<Event>(requested(json!(input))).is_err());
    let mut keys = serde_json::Map::new();
    for n in 0..32 {
        keys.insert(n.to_string(), json!(null));
    }
    assert!(serde_json::from_value::<Event>(requested(Value::Object(keys))).is_ok());
}

#[test]
fn metadata_options_gaps_and_safe_counters_reject_null_and_extra_authority() {
    let result = json!({"type":"tool:result","toolCallId":"id","name":"Read","title":""});
    let detail = json!({"key":"","displayValue":"","highlighted":false});
    let agent = json!({"mode":"worker","report":"","status":"done","rounds":0});
    let child = json!({"agent":"","status":"done","rounds":0});
    let mut bad_payloads = Vec::new();
    for field in ["detail", "detailLines", "agentOutput"] {
        let mut value = result.clone();
        value[field] = Value::Null;
        bad_payloads.push(value);
    }
    for field in ["agent", "tracePath", "runId", "logPath", "children"] {
        let mut value = result.clone();
        value["agentOutput"] = agent.clone();
        value["agentOutput"][field] = Value::Null;
        bad_payloads.push(value);
    }
    for field in ["runId", "children"] {
        let mut value = result.clone();
        value["agentOutput"] = agent.clone();
        value["agentOutput"]["children"] = json!([child]);
        value["agentOutput"]["children"][0][field] = Value::Null;
        bad_payloads.push(value);
    }
    for lines in [
        json!([{"key":"","displayValue":"","highlighted":false,"kind":null}]),
        json!([{"key":"","displayValue":"","highlighted":false,"approved":true}]),
        json!(vec![detail; 33]),
    ] {
        let mut value = result.clone();
        value["detailLines"] = lines;
        bad_payloads.push(value);
    }
    for rounds in [json!(-1), json!(0.5), json!(9007199254740992_u64)] {
        let mut value = result.clone();
        value["agentOutput"] = agent.clone();
        value["agentOutput"]["rounds"] = rounds;
        bad_payloads.push(value);
    }
    for value in [
        json!({"type":"tool:start","toolCallId":"id","name":"Read","inputSummary":"","agentMode":null}),
        json!({"type":"tool:requested","turnId":"sdk/turn","calls":[]}),
        json!({"type":"tool:requested","turnId":"sdk/turn","calls":[{"id":"id","name":"Read"}]}),
        json!({"type":"agent:child_tool_start","agentToolCallId":"parent","call":{"id":"id","name":"Read","inputSummary":"","status":"done"}}),
        json!({"type":"agent:child_tool_result","agentToolCallId":"parent","childToolCallId":"child","status":"running"}),
    ] {
        bad_payloads.push(value);
    }
    for value in bad_payloads {
        assert!(
            serde_json::from_value::<Event>(observation(tool_observation(value.clone()))).is_err(),
            "{value}"
        );
    }
    for key in [
        "inputTokens",
        "outputTokens",
        "totalTokens",
        "cachedTokens",
        "cacheWriteTokens",
        "contextTokens",
        "contextLimit",
        "costUsd",
        "model",
        "provider",
    ] {
        let mut null = json!({});
        null[key] = Value::Null;
        assert!(serde_json::from_value::<Event>(observation(usage_observation(null))).is_err());
        if !matches!(key, "costUsd" | "model" | "provider") {
            for invalid in [json!(-1), json!(0.5), json!(9007199254740992_u64)] {
                let mut usage = json!({});
                usage[key] = invalid;
                assert!(
                    serde_json::from_value::<Event>(observation(usage_observation(usage))).is_err()
                );
            }
        }
    }
    for (source, reason, id, accepted) in [
        ("future:event", "unsupported_event", None, true),
        (
            "tool:progress",
            "unqualified_progress",
            Some("opaque/id"),
            true,
        ),
        ("future:event", "unsupported_event", Some("id"), false),
        ("future:event", "unqualified_progress", Some("id"), false),
        ("tool:progress", "unsupported_event", None, false),
        ("tool:progress", "unqualified_progress", Some(""), false),
    ] {
        let mut value = json!({"kind":"observation_gap","scope":"turn_unattributed",
            "source_type":source,"reason":reason});
        if let Some(id) = id {
            value["tool_call_id"] = json!(id);
        }
        assert_eq!(
            serde_json::from_value::<Event>(observation(value)).is_ok(),
            accepted
        );
    }
}

#[test]
fn duplicate_decoded_keys_are_rejected_in_the_envelope_and_typed_metadata() {
    let valid = serde_json::to_string(&observation(tool_observation(json!({"type":"tool:result",
        "toolCallId":"id","name":"Read","title":"","detailLines":[
            {"key":"key","displayValue":"text","highlighted":false}]}))))
    .unwrap();
    assert!(serde_json::from_str::<Event>(&valid).is_ok());
    for raw in [
        valid.replace("\"sequence\":2", "\"sequence\":2,\"sequence\":3"),
        valid.replace(
            "\"kind\":\"tool_observation\"",
            "\"kind\":\"tool_observation\",\"kind\":\"tool_observation\"",
        ),
        valid.replace("\"key\":\"key\"", "\"key\":\"key\",\"k\\u0065y\":\"other\""),
    ] {
        assert_ne!(raw, valid);
        assert!(serde_json::from_str::<Value>(&raw).is_ok());
        assert!(serde_json::from_str::<Event>(&raw).is_err());
    }
}

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
