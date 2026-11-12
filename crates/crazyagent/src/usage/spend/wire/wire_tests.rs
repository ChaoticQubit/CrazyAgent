use super::super::{Outcome, attempt_key};
use super::*;
use crate::credentials::SecretString;
use crate::github::test_server::{Answer, Server};

fn secret() -> String {
    let mut bytes = [0_u8; 12];
    getrandom::fill(&mut bytes).unwrap();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("synthetic-{hex}")
}

fn credential(token: &str) -> PollCredential {
    PollCredential {
        token: SecretString::new(token.as_bytes().to_vec()).unwrap(),
        account_id: Some("acct-synthetic".into()),
        plan: None,
    }
}

fn consume_at(server: &Server) -> url::Url {
    let usage = url::Url::parse(&format!(
        "http://127.0.0.1:{}/backend-api/wham/usage",
        server.port
    ))
    .unwrap();
    codex_consume_url(&usage).unwrap()
}

#[test]
fn the_consume_call_sits_beside_the_usage_call() {
    let usage = url::Url::parse(crate::usage::poll::CODEX_USAGE_URL).unwrap();
    assert_eq!(
        codex_consume_url(&usage).unwrap().as_str(),
        "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits/consume"
    );
}

#[test]
fn a_codex_spend_posts_the_key_and_token_and_names_no_reset() {
    let token = secret();
    let key = attempt_key().unwrap();
    let server = Server::start(|_, _| Answer::json(200, serde_json::json!({"outcome": "reset"})));
    let outcome = codex_consume(consume_at(&server), &credential(&token), &key, true).unwrap();
    assert_eq!(outcome, Outcome::Reset);
    let seen = server.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "POST");
    assert_eq!(
        seen[0].path,
        "/backend-api/wham/rate-limit-reset-credits/consume"
    );
    assert_eq!(seen[0].header("idempotency-key"), Some(key.as_str()));
    assert_eq!(
        seen[0].header("authorization"),
        Some(format!("Bearer {token}").as_str())
    );
    assert_eq!(seen[0].header("chatgpt-account-id"), Some("acct-synthetic"));
    assert_eq!(seen[0].header("content-type"), Some("application/json"));
    assert_eq!(seen[0].body, b"{}");
}

#[test]
fn the_curl_command_holds_no_token_key_or_url() {
    let token = secret();
    let key = attempt_key().unwrap();
    let url =
        url::Url::parse("http://127.0.0.1:9/backend-api/wham/rate-limit-reset-credits/consume")
            .unwrap();
    let (command, config) = codex_request(&url, &credential(&token), &key, true)
        .unwrap()
        .json_command("{}")
        .unwrap();
    let shown = format!(
        "{:?} {:?}",
        command.get_args().collect::<Vec<_>>(),
        command.get_envs().collect::<Vec<_>>()
    );
    assert!(!shown.contains(&token));
    assert!(!shown.contains(&key));
    assert!(!shown.contains("rate-limit-reset-credits"));
    let config = String::from_utf8(config).unwrap();
    assert!(config.contains(&key));
    assert!(config.contains("data-raw = \"{}\""));
}

#[test]
fn codex_spend_replies_are_read_by_their_outcome_word_in_either_case() {
    let cases = [
        (
            serde_json::json!({"outcome": "nothingToReset"}),
            Outcome::NothingToReset,
        ),
        (
            serde_json::json!({"outcome": "nothing_to_reset"}),
            Outcome::NothingToReset,
        ),
        (
            serde_json::json!({"outcome": "noCredit"}),
            Outcome::NoCredit,
        ),
        (
            serde_json::json!({"result": "already-redeemed"}),
            Outcome::AlreadyRedeemed,
        ),
        (
            serde_json::json!({"status": "RESET", "extra": 1}),
            Outcome::Reset,
        ),
        (
            serde_json::json!({"outcome": "somethingNew"}),
            Outcome::Unknown,
        ),
        (serde_json::json!({"outcome": 3}), Outcome::Unknown),
        (serde_json::json!([]), Outcome::Unknown),
    ];
    for (body, expected) in cases {
        let answer = body.clone();
        let server = Server::start(move |_, _| Answer::json(200, answer.clone()));
        let key = attempt_key().unwrap();
        let outcome =
            codex_consume(consume_at(&server), &credential(&secret()), &key, true).unwrap();
        assert_eq!(outcome, expected, "{body}");
    }
    let server = Server::start(|_, _| Answer::bytes(200, b"not json".to_vec()));
    let key = attempt_key().unwrap();
    assert_eq!(
        codex_consume(consume_at(&server), &credential(&secret()), &key, true).unwrap(),
        Outcome::Unknown
    );
}

#[test]
fn codex_spend_failures_become_plain_codes() {
    for (status, code) in [
        (401, "usage_sign_in_lapsed"),
        (403, "usage_sign_in_lapsed"),
        (429, "usage_rate_limited"),
        (404, "usage_spend_failed"),
        (500, "usage_spend_failed"),
    ] {
        let server = Server::start(move |_, _| Answer::json(status, serde_json::json!({})));
        let key = attempt_key().unwrap();
        let problem =
            codex_consume(consume_at(&server), &credential(&secret()), &key, true).unwrap_err();
        assert_eq!(problem.0, code, "status {status}");
    }
    let closed = crate::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let gone = url::Url::parse(&format!(
        "http://127.0.0.1:{port}/backend-api/wham/rate-limit-reset-credits/consume"
    ))
    .unwrap();
    let key = attempt_key().unwrap();
    assert_eq!(
        codex_consume(gone, &credential(&secret()), &key, true)
            .unwrap_err()
            .0,
        "usage_unreachable"
    );
}

#[test]
fn a_malformed_key_is_never_sent() {
    let server = Server::start(|_, _| Answer::json(200, serde_json::json!({"outcome": "reset"})));
    let problem = codex_consume(
        consume_at(&server),
        &credential(&secret()),
        "not-a-key",
        true,
    )
    .unwrap_err();
    assert_eq!(problem.0, "usage_request_invalid");
    assert!(server.seen().is_empty());
}

#[test]
fn a_plain_http_consume_call_is_refused_unless_it_is_a_loopback_test() {
    let key = attempt_key().unwrap();
    let url =
        url::Url::parse("http://chatgpt.com/backend-api/wham/rate-limit-reset-credits/consume")
            .unwrap();
    assert_eq!(
        codex_consume(url, &credential(&secret()), &key, false)
            .unwrap_err()
            .0,
        "usage_request_invalid"
    );
}
