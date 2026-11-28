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

/// The encoding before the hex table: one `format!` per byte, kept here as the oracle the
/// table must match byte for byte. Input: the random bytes. Output: the token text.
fn formatted(bytes: &[u8; TOKEN_HEX / 2]) -> String {
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{TOKEN_PREFIX}{hex}")
}

#[test]
fn encoded_matches_the_formatted_token_for_every_byte_value() {
    for value in 0..=u8::MAX {
        let bytes = [value; TOKEN_HEX / 2];
        assert_eq!(encoded(&bytes), formatted(&bytes), "byte {value:#04x}");
    }
}

#[test]
fn encoded_matches_the_formatted_token_for_a_thousand_random_tokens() {
    for _ in 0..1000 {
        let mut bytes = [0_u8; TOKEN_HEX / 2];
        getrandom::fill(&mut bytes).unwrap();
        assert_eq!(encoded(&bytes), formatted(&bytes));
    }
}

#[test]
fn encoded_fills_its_first_capacity_exactly_and_is_a_relay_token() {
    let bytes = [0xa5; TOKEN_HEX / 2];
    let token = encoded(&bytes);
    assert_eq!(token.len(), TOKEN_PREFIX.len() + TOKEN_HEX);
    assert_eq!(token.capacity(), token.len());
    assert!(is_relay_token(&token));
}
