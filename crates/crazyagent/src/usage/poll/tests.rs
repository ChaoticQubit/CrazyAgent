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
            provider: Some(super::Provider::Claude),
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
    assert_eq!(bare.provider, Some(Provider::Codex));
}

/// N-D16 r1 NDX-L1: a Codex window of a length that is neither 5 hours nor a week cannot be
/// placed, so the reply is refused as a failed read rather than read as one leaving that
/// window out.
#[test]
fn a_codex_window_of_an_unknown_length_refuses_the_reply() {
    for windows in [
        r#""primary_window":{"used_percent":10,"limit_window_seconds":86400},"secondary_window":{"used_percent":20,"limit_window_seconds":604800}"#,
        r#""primary_window":{"used_percent":10,"limit_window_seconds":18000},"secondary_window":{"used_percent":20,"limit_window_seconds":2592000}"#,
        r#""primary_window":{"used_percent":10,"limit_window_seconds":604800},"secondary_window":{"used_percent":20,"limit_window_seconds":0}"#,
    ] {
        let body = format!(r#"{{"plan_type":"pro","rate_limit":{{{windows}}}}}"#);
        assert_eq!(
            parse_codex(body.as_bytes(), NOW).unwrap_err().0,
            "usage_reply_invalid",
            "{windows}"
        );
    }
}

#[test]
fn a_reply_that_leaves_a_window_out_reads_the_other_and_names_its_provider() {
    let prolite = parse_codex(
        br#"{"plan_type":"prolite","rate_limit":{"primary_window":{"used_percent":25,"limit_window_seconds":604800,"reset_at":1793400000},"secondary_window":null}}"#,
        NOW,
    )
    .unwrap();
    assert_eq!(
        prolite,
        Observation {
            plan: Some("prolite".into()),
            five_hour: None,
            weekly: Some(LimitWindow {
                used_percent: 25.0,
                resets_at_ms: Some(1_793_400_000_000)
            }),
            provider: Some(Provider::Codex),
        }
    );
    let idle = parse_claude(
        br#"{"five_hour":null,"seven_day":{"utilization":40,"resets_at":"2026-10-30T00:00:00+00:00"}}"#,
    )
    .unwrap();
    assert_eq!(
        idle,
        Observation {
            plan: None,
            five_hour: None,
            weekly: Some(LimitWindow {
                used_percent: 40.0,
                resets_at_ms: Some(1_793_318_400_000)
            }),
            provider: Some(Provider::Claude),
        }
    );
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
    let (_held, port) = crate::spawn_gate::refusing_tcp_port().unwrap();
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

fn claude_windows() -> serde_json::Value {
    serde_json::json!({
        "five_hour": {"utilization": 37.0, "resets_at": "2026-10-27T08:00:00+00:00"},
        "seven_day": {"utilization": 81.5, "resets_at": "2026-10-30T00:00:00+00:00"},
        "extra_usage": {"is_enabled": false}
    })
}

#[test]
fn a_background_claude_poll_makes_one_request_without_the_private_query() {
    let secret = token();
    let server = Server::start(|_, _| Answer::json(200, claude_windows()));
    let (observed, extras) = poll_with_extras(
        Provider::Claude,
        local(&server, "/api/oauth/usage"),
        &credential(&secret, None, None),
        true,
        NOW,
        false,
    )
    .unwrap();
    assert_eq!(
        observed.five_hour.map(|window| window.used_percent),
        Some(37.0)
    );
    assert_eq!(
        extras,
        Extras {
            resets: Field::NotAsked,
            credits: Field::Known("Extra usage off".into()),
        }
    );
    let seen = server.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].query, "");
}

#[test]
fn a_refused_claude_detail_keeps_the_windows_and_reads_resets_absent() {
    let secret = token();
    for status in [401, 403, 429, 500] {
        let server = Server::start(move |seen, _| {
            if seen.query.is_empty() {
                Answer::json(200, claude_windows())
            } else {
                Answer::json(status, serde_json::json!({"error": "scope"}))
            }
        });
        let (observed, extras) = poll_with_extras(
            Provider::Claude,
            local(&server, "/api/oauth/usage"),
            &credential(&secret, None, None),
            true,
            NOW,
            true,
        )
        .unwrap();
        assert!(
            observed.five_hour.is_some() && observed.weekly.is_some(),
            "{status}"
        );
        assert_eq!(extras.resets, Field::Absent, "{status}");
        assert_eq!(extras.credits, Field::Known("Extra usage off".into()));
        let seen = server.seen();
        assert_eq!(seen.len(), 2, "{status}");
        assert_eq!(seen[1].path, "/api/oauth/usage");
        assert_eq!(seen[1].query, super::super::extras::CLAUDE_DETAIL_QUERY);
        assert_eq!(
            seen[1].header("authorization"),
            Some(format!("Bearer {secret}").as_str())
        );
        assert_eq!(seen[1].header("anthropic-beta"), Some("oauth-2025-04-20"));
    }
}

#[test]
fn a_good_claude_detail_reads_the_free_resets() {
    let secret = token();
    let server = Server::start(|seen, _| {
        if seen.query.is_empty() {
            Answer::json(200, claude_windows())
        } else {
            Answer::json(
                200,
                serde_json::json!({"cedar_ember": {"eligible": true, "grants": [
                    {"resets_left": 2, "paused": false, "ends_at": "2026-10-27T08:00:00+00:00"}
                ]}}),
            )
        }
    });
    let (_, extras) = poll_with_extras(
        Provider::Claude,
        local(&server, "/api/oauth/usage"),
        &credential(&secret, None, None),
        true,
        NOW,
        true,
    )
    .unwrap();
    assert_eq!(
        extras.resets,
        Field::Known(super::super::extras::Resets {
            count: 2,
            kinds: vec![],
            next_expiry_ms: Some(1_793_088_000_000),
        })
    );
}

#[test]
fn a_failed_main_poll_makes_no_detail_request() {
    let server = Server::start(|_, _| Answer::json(401, serde_json::json!({})));
    let failed = poll_with_extras(
        Provider::Claude,
        local(&server, "/api/oauth/usage"),
        &credential(&token(), None, None),
        true,
        NOW,
        true,
    )
    .unwrap_err();
    assert_eq!(failed.0, "usage_sign_in_lapsed");
    assert_eq!(server.seen().len(), 1);
}

#[test]
fn a_malformed_codex_reset_field_keeps_the_windows_from_the_one_request() {
    let server = Server::start(|_, _| {
        Answer::json(
            200,
            serde_json::json!({
                "plan_type": "plus",
                "rate_limit": {"primary_window": {"used_percent": 12, "limit_window_seconds": 18000, "reset_at": 1_793_010_000}},
                "rate_limit_reset_credits": {"available_count": "two"},
                "credits": {"has_credits": true, "unlimited": true}
            }),
        )
    });
    let (observed, extras) = poll_with_extras(
        Provider::Codex,
        local(&server, "/backend-api/wham/usage"),
        &credential(&token(), Some("acct-1"), None),
        true,
        NOW,
        true,
    )
    .unwrap();
    assert!(observed.five_hour.is_some());
    assert_eq!(
        extras,
        Extras {
            resets: Field::Absent,
            credits: Field::Known("Unlimited credits".into()),
        }
    );
    assert_eq!(server.seen().len(), 1, "Codex has no detail request");
}

#[test]
fn a_key_balance_read_sends_only_that_key_and_reads_failures_absent() {
    let key = token();
    let secret = SecretString::new(key.as_bytes().to_vec()).unwrap();
    let server = Server::start(|_, _| {
        Answer::json(
            200,
            serde_json::json!({"data": {"total_credits": 20, "total_usage": 7.5}}),
        )
    });
    assert_eq!(
        poll_balance(
            Adapter::OpenRouter,
            local(&server, "/api/v1/credits"),
            &secret,
            true
        ),
        Field::Known("12.50 USD left".into())
    );
    let seen = server.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "GET");
    assert_eq!(seen[0].path, "/api/v1/credits");
    assert_eq!(
        seen[0].header("authorization"),
        Some(format!("Bearer {key}").as_str())
    );
    assert_eq!(seen[0].header("anthropic-beta"), None);
    assert_eq!(seen[0].header("chatgpt-account-id"), None);
    for answer in [
        Answer::json(401, serde_json::json!({})),
        Answer::json(302, serde_json::json!({})),
        Answer::json(200, serde_json::json!({"data": "lots"})),
        Answer::bytes(200, b"not json".to_vec()),
    ] {
        let status = answer.status;
        let server = Server::start(move |_, _| Answer {
            status: answer.status,
            headers: answer.headers.clone(),
            body: answer.body.clone(),
        });
        assert_eq!(
            poll_balance(
                Adapter::OpenRouter,
                local(&server, "/api/v1/credits"),
                &secret,
                true
            ),
            Field::Absent,
            "{status}"
        );
    }
}

#[test]
fn a_key_balance_read_over_plain_http_is_refused_outside_the_loopback_test() {
    let key = SecretString::new(token().into_bytes()).unwrap();
    let server = Server::start(|_, _| Answer::json(200, serde_json::json!({})));
    assert_eq!(
        poll_balance(
            Adapter::OpenRouter,
            local(&server, "/api/v1/credits"),
            &key,
            false
        ),
        Field::Absent
    );
    assert!(server.seen().is_empty(), "nothing left the engine");
}

#[test]
fn a_detail_only_read_makes_one_request_with_the_private_query() {
    let secret = token();
    let server = Server::start(|_, _| {
        Answer::json(
            200,
            serde_json::json!({"cedar_ember": {"eligible": true, "grants": [
                {"resets_left": 1, "paused": false}
            ]}}),
        )
    });
    let read = claude_detail(
        local(&server, "/api/oauth/usage"),
        &credential(&secret, None, None),
        true,
        NOW,
    );
    assert_eq!(
        read,
        Field::Known(super::super::extras::Resets {
            count: 1,
            kinds: vec![],
            next_expiry_ms: None,
        })
    );
    let seen = server.seen();
    assert_eq!(seen.len(), 1, "no window poll rides along");
    assert_eq!(seen[0].query, super::super::extras::CLAUDE_DETAIL_QUERY);
    assert_eq!(
        seen[0].header("authorization"),
        Some(format!("Bearer {secret}").as_str())
    );
}
