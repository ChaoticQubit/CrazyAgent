use super::*;
use kernel_common::relay_names::{KEY_BYTES_MAX, LOCAL_PLACEHOLDER, is_placeholder};

#[test]
fn a_minted_token_is_a_relay_token() {
    let token = RelayToken::mint().unwrap();
    assert!(is_relay_token(token.as_str()));
    assert!(is_placeholder(token.as_str()));
    assert_eq!(token.as_str().len(), KEY_BYTES_MAX);
    assert!(token.as_str().starts_with(TOKEN_PREFIX));
}

#[test]
fn two_mints_differ() {
    let first = RelayToken::mint().unwrap();
    let second = RelayToken::mint().unwrap();
    assert_ne!(first.as_str(), second.as_str());
}

#[test]
fn matches_is_true_only_for_the_same_token() {
    let token = RelayToken::mint().unwrap();
    let other = RelayToken::mint().unwrap();
    assert!(token.matches(token.as_str()));
    assert!(!token.matches(other.as_str()));
    assert!(!token.matches(""));
    assert!(!token.matches(&token.as_str()[..KEY_BYTES_MAX - 1]));
    assert!(!token.matches(&format!("{}0", token.as_str())));
    assert!(!token.matches(LOCAL_PLACEHOLDER));
}
