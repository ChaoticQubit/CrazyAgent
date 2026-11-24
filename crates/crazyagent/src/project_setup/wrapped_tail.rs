use super::*;

/// Whether a word at a line's start can be the wrapped tail of a token masked at the end of
/// the line before: at least `WRAPPED_TAIL_MIN` bytes of the token and base64 alphabets,
/// holding a digit. Takes the word.
pub(super) fn wrapped_tail(word: &str) -> bool {
    word.len() >= WRAPPED_TAIL_MIN
        && word.bytes().any(|b| b.is_ascii_digit())
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'-' | b'_'))
}
