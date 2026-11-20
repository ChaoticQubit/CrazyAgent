const NAME_UNITS_MAX: usize = 255;
const CHECKED_DEVICE_NAMES: [&str; 6] = ["AUX", "CON", "CONIN$", "CONOUT$", "NUL", "PRN"];
const FORBIDDEN: [char; 9] = ['/', '\\', ':', '<', '>', '"', '|', '?', '*'];

/// The device names Windows takes with one digit after them, COM and LPT, which a name
/// rule for constants also refuses. Output: the prefixes.
pub const NUMBERED_DEVICES: [&str; 2] = ["COM", "LPT"];

/// The one name rule W1 `rename` and the engine's held names share (V244-N6-W1-SEC-L1):
/// says whether a name is not one plain component on some system, so empty, over 255 UTF-16
/// units, holding a control, a separator, a colon or another character Windows refuses,
/// ending in a dot or a space (`.` and `..` among them), or a device name. Input: the name.
/// Output: true when it is refused.
pub fn refused(text: &str) -> bool {
    text.is_empty()
        || text.encode_utf16().count() > NAME_UNITS_MAX
        || text
            .chars()
            .any(|character| character.is_control() || FORBIDDEN.contains(&character))
        || text.ends_with('.')
        || text.ends_with(' ')
        || device(text)
}

/// Says whether a name is a Windows device in any case, with or without an extension and
/// with spaces before it: CON, PRN, AUX, NUL, CONIN$, CONOUT$, and COM or LPT followed by
/// one digit or a superscript one, two or three. Input: the name. Output: the answer.
fn device(text: &str) -> bool {
    let stem = text
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_uppercase();
    let numbered = NUMBERED_DEVICES.iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|tail| {
            let mut units = tail.chars();
            matches!(
                (units.next(), units.next()),
                (Some(digit), None)
                    if digit.is_ascii_digit() || matches!(digit, '\u{b9}' | '\u{b2}' | '\u{b3}')
            )
        })
    });
    CHECKED_DEVICE_NAMES.contains(&stem.as_str()) || numbered
}
