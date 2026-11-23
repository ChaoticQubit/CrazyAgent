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

/// True when the text is exactly one plain path component that names one child, never
/// itself, its parent or another root: not empty, `.`, `..`, a separator, a root or a
/// Windows drive like `c:`, no trailing dot or space, which Windows strips, and no Windows
/// device name. A colon stays allowed, for identities and as a Unix file name; on Windows
/// `name:x` is a stream of `name`, so a Windows folder name needs none. Input: the text.
/// Output: whether it names one child.
pub fn single_component(text: &str) -> bool {
    let mut parts = std::path::Path::new(text).components();
    matches!(
        (parts.next(), parts.next()),
        (Some(std::path::Component::Normal(part)), None) if part == std::ffi::OsStr::new(text)
    ) && !text.ends_with(['.', ' '])
        && !crate::windows_device(text)
}

/// The refusal [`validate_identity`] gives, which the engine's protocol error carries.
pub const IDENTITY_REFUSED: &str = "identity must be 1..128 safe ASCII bytes";

/// Checks one engine identity: 1 to 128 ASCII letters, digits, `-`, `_`, `.` or `:`, no
/// `..`, not starting with one letter and `:` (a drive on Windows, refused on every OS so
/// an identity is valid everywhere or nowhere), and one plain path component
/// ([`single_component`]). Input: the text. Output: Ok, or [`IDENTITY_REFUSED`].
pub fn validate_identity(value: &str) -> Result<(), &'static str> {
    let drive = matches!(value.as_bytes(), [letter, b':', ..] if letter.is_ascii_alphabetic());
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
        || value.contains("..")
        || drive
        || !single_component(value)
    {
        return Err(IDENTITY_REFUSED);
    }
    assert!(!value.is_empty());
    assert!(value.len() <= 128);
    Ok(())
}
