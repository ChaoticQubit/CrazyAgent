const NAME_UNITS_MAX: usize = 255;
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
        || kernel_common::windows_device(text)
}
