/// Whether a word is a snake name: a lower-case letter, then lower-case letters, digits and `_`.
/// Input: the word. Output: true for a snake name.
pub fn is_snake(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes.next().is_some_and(|first| first.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

/// Whether a word is a dotted name: two or more snake names joined by `.`.
/// Input: the word. Output: true for a dotted name.
pub fn is_dotted(text: &str) -> bool {
    text.split('.').count() >= 2 && text.split('.').all(is_snake)
}
