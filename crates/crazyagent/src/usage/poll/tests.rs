use super::*;
use crate::github::test_server::{Answer, Server};

const NOW: i64 = 1_793_000_000_000;

fn token() -> String {
    let mut bytes = [0_u8; 12];
    getrandom::fill(&mut bytes).unwrap();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("synthetic-{hex}")
}

fn credential(text: &str, account_id: Option<&str>, plan: Option<&str>) -> PollCredential {
    PollCredential {
        token: SecretString::new(text.as_bytes().to_vec()).unwrap(),
        account_id: account_id.map(str::to_owned),
        plan: plan.map(str::to_owned),
    }
}

fn local(server: &Server, path: &str) -> url::Url {
    url::Url::parse(&format!("http://127.0.0.1:{}{path}", server.port)).unwrap()
}

#[test]
fn rfc3339_times_read_to_epoch_milliseconds() {
    assert_eq!(
        rfc3339_ms("2026-10-27T08:00:00+00:00"),
        Some(1_793_088_000_000)
    );
    assert_eq!(
        rfc3339_ms("2026-10-27T08:00:00.123456+00:00"),
        Some(1_793_088_000_123)
    );
    assert_eq!(
        rfc3339_ms("2026-10-27T10:30:00.5+02:30"),
        Some(1_793_088_000_500)
    );
    assert_eq!(rfc3339_ms("2024-02-29T23:59:59Z"), Some(1_709_251_199_000));
    assert_eq!(rfc3339_ms("1970-01-01T00:00:01Z"), Some(1_000));
    for bad in [
        "2026-13-01T00:00:00Z",
        "2026-10-27 08:00",
        "2026-10-27T08:00:00",
        "2026-10-27T08:00:00+0000",
        "2026-10-27T25:00:00Z",
        "2026-10-27T08:00:00.Z",
        "１９７０-01-01T00:00:01Z",
        "1970-01-01T00:00:00Z",
    ] {
        assert_eq!(rfc3339_ms(bad), None, "{bad}");
    }
}

#[test]
fn claude_poll_sends_the_token_and_beta_header_and_reads_both_windows() {
    let secret = token();
    let server = Server::start(|_, _| {
        Answer::json(
            200,
            serde_json::json!({
                "five_hour": {"utilization": 37.0, "resets_at": "2026-10-27T08:00:00.123456+00:00"},
                "seven_day": {"utilization": 81.5, "resets_at": "2026-10-30T00:00:00+00:00"},
                "seven_day_opus": null,
                "extra_usage": {"is_enabled": false}
            }),
        )
    });
    let observed = poll(
        Provider::Claude,
        local(&server, "/api/oauth/usage"),
        &credential(&secret, None, Some("Max")),
        true,
        NOW,
    )
    .unwrap();
    assert_eq!(
        observed,
        Observation {
            plan: Some("max".into()),
            five_hour: Some(LimitWindow {
                used_percent: 37.0,
                resets_at_ms: Some(1_793_088_000_123)
            }),
            weekly: Some(LimitWindow {
                used_percent: 81.5,
                resets_at_ms: Some(1_793_318_400_000)
            }),
        }
    );
    let seen = server.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "GET");
    assert_eq!(seen[0].path, "/api/oauth/usage");
    assert_eq!(
        seen[0].header("authorization"),
        Some(format!("Bearer {secret}").as_str())
    );
    assert_eq!(seen[0].header("anthropic-beta"), Some("oauth-2025-04-20"));
    assert_eq!(seen[0].header("chatgpt-account-id"), None);
}

#[test]
fn codex_poll_sends_the_account_id_and_places_windows_by_length() {
    let secret = token();
    let server = Server::start(|_, _| {
        Answer::json(
            200,
            serde_json::json!({
                "plan_type": "pro",
                "rate_limit": {
                    "allowed": true,
                    "limit_reached": false,
                    "primary_window": {"used_percent": 604800, "limit_window_seconds": 604800, "reset_at": 1_793_400_000},
                    "secondary_window": {"used_percent": 12, "limit_window_seconds": 18000, "reset_after_seconds": 600}
                }
            }),
        )
    });
    let observed = poll(
        Provider::Codex,
        local(&server, "/backend-api/wham/usage"),
        &credential(&secret, Some("acct-123_x"), Some("plus")),
        true,
        NOW,
    )
    .unwrap();
    assert_eq!(observed.plan.as_deref(), Some("pro"));
    assert_eq!(
        observed.weekly,
        Some(LimitWindow {
            used_percent: 100.0,
            resets_at_ms: Some(1_793_400_000_000)
        })
    );
    assert_eq!(
        observed.five_hour,
        Some(LimitWindow {
            used_percent: 12.0,
            resets_at_ms: Some(NOW + 600_000)
        })
    );
    let seen = server.seen();
    assert_eq!(seen[0].header("chatgpt-account-id"), Some("acct-123_x"));
    assert_eq!(
        seen[0].header("authorization"),
        Some(format!("Bearer {secret}").as_str())
    );
    assert_eq!(seen[0].header("anthropic-beta"), None);
    let bare = parse_codex(
        br#"{"rate_limit":{"primary_window":{"used_percent":5},"secondary_window":{"used_percent":6}}}"#,
        NOW,
    )
    .unwrap();
    assert_eq!(bare.five_hour.unwrap().used_percent, 5.0);
    assert_eq!(bare.weekly.unwrap().used_percent, 6.0);
    assert_eq!(bare.plan, None);
}

#[test]
fn poll_failures_map_to_plain_codes_and_odd_replies_are_refused() {
    let server = Server::start(|seen, _| match seen.path.as_str() {
        "/401" => Answer::json(401, serde_json::json!({"error": "expired"})),
        "/403" => Answer::json(403, serde_json::json!({})),
        "/429" => Answer::json(429, serde_json::json!({"retry_after": 60})),
        "/500" => Answer::json(500, serde_json::json!({})),
        "/text" => Answer::bytes(200, b"<html>".to_vec()),
        "/empty" => Answer::json(200, serde_json::json!({"five_hour": null})),
        _ => Answer::json(
            200,
            serde_json::json!({"five_hour": {"utilization": "a lot"}}),
        ),
    });
    let secret = token();
    let owner = credential(&secret, None, None);
    for (path, code) in [
        ("/401", "usage_sign_in_lapsed"),
        ("/403", "usage_sign_in_lapsed"),
        ("/429", "usage_rate_limited"),
        ("/500", "usage_poll_failed"),
        ("/text", "usage_reply_invalid"),
        ("/empty", "usage_reply_invalid"),
        ("/odd", "usage_reply_invalid"),
    ] {
        let refused = poll(Provider::Claude, local(&server, path), &owner, true, NOW);
        assert_eq!(refused.unwrap_err().0, code, "{path}");
    }
    let refused = poll(Provider::Codex, local(&server, "/429"), &owner, true, NOW);
    assert_eq!(refused.unwrap_err().0, "usage_rate_limited");
    let plain = poll(Provider::Claude, local(&server, "/401"), &owner, false, NOW);
    assert_eq!(plain.unwrap_err().0, "usage_request_invalid");
    let closed = crate::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let gone = url::Url::parse(&format!("http://127.0.0.1:{port}/api/oauth/usage")).unwrap();
    let refused = poll(Provider::Claude, gone, &owner, true, NOW);
    assert_eq!(refused.unwrap_err().0, "usage_unreachable");
    assert_eq!(
        parse_claude(br#"{"five_hour":{"utilization":10,"resets_at":"soon"}}"#)
            .unwrap_err()
            .0,
        "usage_reply_invalid"
    );
    assert_eq!(
        parse_claude(br#"{"seven_day":{"utilization":10,"resets_at":1793088000}}"#)
            .unwrap()
            .weekly,
        Some(LimitWindow {
            used_percent: 10.0,
            resets_at_ms: Some(1_793_088_000_000)
        })
    );
}
