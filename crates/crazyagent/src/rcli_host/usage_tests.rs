use super::master_reply::TurnCapture;
use super::{Event, EventSequence, RcliMasterStatus, RcliProviderFailure};
use crate::protocol::RequestId;
use crate::usage::capture::{LINE_TOKENS_MAX, Tokens};
use serde_json::{Value, json};

fn frame(sequence: u64, event: Value) -> (Event, usize) {
    let value = json!({
        "protocol_version": 1, "session_id": "session-a", "instance_id": "instance-a",
        "sequence": sequence, "event": event
    });
    let bytes = serde_json::to_vec(&value).unwrap().len() + 4;
    (serde_json::from_value(value).unwrap(), bytes)
}

fn usage(sequence: u64, usage: Value) -> (Event, usize) {
    frame(
        sequence,
        json!({
            "kind": "usage_observation", "scope": "turn_unattributed",
            "provenance": "sdk_reported", "accounting": "UNKNOWN", "usage": usage
        }),
    )
}

fn counted(turn: &mut TurnCapture, (event, bytes): &(Event, usize)) -> Tokens {
    assert_eq!(turn.accept(event, *bytes).unwrap(), None);
    turn.take_usage()
}

fn stream(ready: &Event) -> EventSequence {
    let mut stream = EventSequence::new(ready.0.session_id.clone(), ready.0.instance_id.clone());
    stream.accept(ready).unwrap();
    stream
}

#[test]
fn rcli_usage_frames_count_each_provider_call_once() {
    let mut turn = TurnCapture::new(RequestId::new("turn-a").unwrap());
    let first = usage(
        2,
        json!({
            "inputTokens": 1200, "outputTokens": 300, "totalTokens": 1500, "cachedTokens": 800,
            "cacheWriteTokens": 100, "model": "e2e-model", "provider": "openai-compatible"
        }),
    );
    assert_eq!(
        counted(&mut turn, &first),
        Tokens {
            input: 300,
            output: 300,
            cache_read: 800,
            cache_write: 100
        },
        "input keeps the cache reads and writes apart, as every adapter counts them inside it"
    );
    assert_eq!(
        counted(&mut turn, &first),
        Tokens::default(),
        "a repeated frame counts nothing"
    );
    let second = usage(3, json!({"inputTokens": 50, "outputTokens": 7}));
    assert_eq!(
        counted(&mut turn, &second),
        Tokens {
            input: 50,
            output: 7,
            cache_read: 0,
            cache_write: 0
        }
    );
    assert_eq!(
        counted(&mut turn, &first),
        Tokens::default(),
        "an older frame replayed counts nothing"
    );
    assert_eq!(counted(&mut turn, &usage(4, json!({}))), Tokens::default());
    let (failed, bytes) = frame(
        5,
        json!({"kind": "completed", "turn_id": "turn-a", "status": "failed"}),
    );
    let completion = turn.accept(&failed, bytes).unwrap().unwrap();
    assert_eq!(completion.status, RcliMasterStatus::Failed);
    assert_eq!(completion.failure, None);
    assert_eq!(turn.take_usage(), Tokens::default());

    let (ready, _) = frame(1, json!({"kind": "ready"}));
    let mut host = stream(&ready);
    host.accept(&first.0).unwrap();
    assert!(
        host.accept(&first.0).is_err(),
        "the host stream refuses a replayed frame before any capture counts it"
    );
    assert_eq!(host.last_sequence(), 2);
}

#[test]
fn rcli_usage_counts_are_bounded_and_input_never_goes_below_zero() {
    let mut turn = TurnCapture::new(RequestId::new("turn-b").unwrap());
    let flood = usage(
        2,
        json!({"inputTokens": 9_007_199_254_740_991_u64, "outputTokens": 9_007_199_254_740_991_u64}),
    );
    assert_eq!(
        counted(&mut turn, &flood),
        Tokens {
            input: LINE_TOKENS_MAX,
            output: LINE_TOKENS_MAX,
            cache_read: 0,
            cache_write: 0
        }
    );
    let odd = usage(
        3,
        json!({"inputTokens": 100, "cachedTokens": 400, "cacheWriteTokens": 50}),
    );
    assert_eq!(
        counted(&mut turn, &odd),
        Tokens {
            input: 0,
            output: 0,
            cache_read: 400,
            cache_write: 50
        }
    );

    let beyond = json!({
        "protocol_version": 1, "session_id": "session-a", "instance_id": "instance-a",
        "sequence": 2, "event": {
            "kind": "usage_observation", "scope": "turn_unattributed",
            "provenance": "sdk_reported", "accounting": "UNKNOWN",
            "usage": {"inputTokens": 9_007_199_254_740_992_u64}
        }
    });
    assert_eq!(
        serde_json::from_value::<Event>(beyond)
            .unwrap_err()
            .to_string(),
        "invalid host observation",
        "a count past the SDK's safe integers is refused before any capture counts it"
    );
}

/// PA-6 (DEC-091): a failed turn's capture keeps the provider failure the host named, so the
/// engine can move the master to the next pool account.
#[test]
fn a_failed_turn_capture_keeps_the_provider_failure() {
    let mut turn = TurnCapture::new(RequestId::new("turn-a").unwrap());
    let (failed, bytes) = frame(
        1,
        json!({"kind": "completed", "turn_id": "turn-a", "status": "failed", "failure": "quota_exhausted"}),
    );
    let completion = turn.accept(&failed, bytes).unwrap().unwrap();
    assert_eq!(completion.status, RcliMasterStatus::Failed);
    assert_eq!(
        completion.failure,
        Some(RcliProviderFailure::QuotaExhausted)
    );
    assert_eq!(completion.reply, None);
}
