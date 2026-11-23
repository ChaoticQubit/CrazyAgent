use super::*;

/// A drawn lineage token and a relay token are each told by their shape, the local
/// placeholder is a placeholder but no relay token, and text of another shape is neither.
#[test]
fn a_lineage_token_and_a_relay_token_are_told_by_their_shape() {
    let token = LineageToken::new().unwrap();
    assert!(LineageToken::valid(token.as_str()));
    assert!(!LineageToken::valid("ABC"));
    let relay = format!("{TOKEN_PREFIX}{}", "a".repeat(TOKEN_HEX));
    assert!(is_relay_token(&relay));
    assert!(is_placeholder(&relay));
    assert!(is_placeholder(LOCAL_PLACEHOLDER));
    assert!(!is_relay_token(LOCAL_PLACEHOLDER));
    assert!(!is_placeholder(LINEAGE_VARIABLE));
}
