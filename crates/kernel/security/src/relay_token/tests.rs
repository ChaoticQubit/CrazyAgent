use super::*;

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

#[test]
fn the_local_placeholder_is_a_placeholder_and_not_a_token() {
    assert!(is_placeholder(LOCAL_PLACEHOLDER));
    assert!(!is_relay_token(LOCAL_PLACEHOLDER));
    assert!(!is_placeholder("sk-ant-real-key"));
    assert!(!is_placeholder(""));
}

#[test]
fn a_token_with_upper_hex_is_refused() {
    let lower = format!("{TOKEN_PREFIX}{}", "a".repeat(TOKEN_HEX));
    let upper = format!("{TOKEN_PREFIX}{}", "A".repeat(TOKEN_HEX));
    assert!(is_relay_token(&lower));
    assert!(!is_relay_token(&upper));
    assert!(!is_placeholder(&upper));
    assert!(!is_relay_token(&format!(
        "{TOKEN_PREFIX}{}",
        "a".repeat(TOKEN_HEX - 1)
    )));
    assert!(!is_relay_token(&format!(
        "{TOKEN_PREFIX}{}",
        "a".repeat(TOKEN_HEX + 1)
    )));
    assert!(!is_relay_token(&format!(
        "{TOKEN_PREFIX}{}g",
        "a".repeat(TOKEN_HEX - 1)
    )));
}
