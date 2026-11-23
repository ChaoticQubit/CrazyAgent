use super::{Query, target};
use crate::error::StoreError;

/// The refusal code every rule of `target` answers.
fn refused() -> Result<(), StoreError> {
    Err(StoreError::code("url_target_invalid"))
}

/// Runs `target` and keeps only whether it passed. Inputs: the text and the query rule.
/// Output: `Ok(())` or the refusal.
fn checked(text: &str, query: Query) -> Result<(), StoreError> {
    target(text, query).map(|_| ())
}

#[test]
fn https_to_any_host_is_a_target() {
    let found = target("https://api.openai.com/v1", Query::Refused).unwrap();
    assert_eq!(found.url.as_str(), "https://api.openai.com/v1");
    assert!(!found.loopback);
}

#[test]
fn plain_http_to_a_loopback_v4_host_is_a_target() {
    let found = target("http://127.0.0.1:8080/v1", Query::Refused).unwrap();
    assert!(found.loopback);
    assert!(target("http://127.0.0.2:8080", Query::Refused).unwrap().loopback);
}

#[test]
fn plain_http_to_a_loopback_v6_host_is_a_target() {
    assert!(target("http://[::1]:11434/v1", Query::Refused).unwrap().loopback);
}

#[test]
fn plain_http_to_localhost_in_any_case_is_a_target() {
    assert!(target("http://localhost:8080", Query::Refused).unwrap().loopback);
    assert!(target("http://LOCALHOST", Query::Refused).unwrap().loopback);
    assert!(target("https://LOCALHOST/v1", Query::Refused).unwrap().loopback);
}

#[test]
fn plain_http_to_a_non_loopback_host_is_refused() {
    for text in [
        "http://api.openai.com/v1",
        "http://192.168.1.10:8080",
        "http://10.0.0.1:11434/v1",
        "http://0.0.0.0:8080/v1",
        "http://localhost.attacker.invalid/v1",
    ] {
        assert_eq!(checked(text, Query::Allowed), refused(), "{text}");
    }
}

#[test]
fn a_user_is_refused() {
    assert_eq!(checked("https://user@api.openai.com/v1", Query::Allowed), refused());
    assert_eq!(checked("http://user@127.0.0.1/v1", Query::Allowed), refused());
}

#[test]
fn a_password_is_refused() {
    assert_eq!(checked("https://:pass@api.openai.com/v1", Query::Allowed), refused());
    assert_eq!(checked("https://user:pass@api.openai.com/v1", Query::Allowed), refused());
}

#[test]
fn a_fragment_is_refused() {
    assert_eq!(checked("https://api.openai.com/v1#x", Query::Allowed), refused());
    assert_eq!(checked("https://api.openai.com/v1#", Query::Allowed), refused());
}

#[test]
fn another_scheme_or_no_host_is_refused() {
    for text in [
        "ftp://127.0.0.1/v1",
        "file:///etc/passwd",
        "ws://127.0.0.1/v1",
        "not a url",
        "",
    ] {
        assert_eq!(checked(text, Query::Allowed), refused(), "{text:?}");
    }
}

#[test]
fn a_query_is_refused_for_the_relay_and_allowed_for_net() {
    let text = "https://api.example.test/v1/search?q=a";
    assert_eq!(checked(text, Query::Refused), refused());
    assert_eq!(
        target(text, Query::Allowed).unwrap().url.query(),
        Some("q=a")
    );
    assert_eq!(checked("https://api.example.test/v1?", Query::Refused), refused());
}
