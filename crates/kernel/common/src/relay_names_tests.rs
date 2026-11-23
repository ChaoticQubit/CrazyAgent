use crate::relay_names::{
    LOCAL_PLACEHOLDER, TOKEN_HEX, TOKEN_PREFIX, is_placeholder, is_relay_token,
};

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
