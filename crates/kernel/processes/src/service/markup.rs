use std::path::Path;

pub(super) fn xml(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

pub(super) fn systemd_specifiers(text: &str) -> String {
    text.replace('%', "%%")
}

/// The largest definition or Info.plist read when telling installed versions apart.
const PLIST_BYTES_MAX: u64 = 64 << 10;

/// Reads a small file as UTF-8 text. Input: the path. Output: the text, or none when it is
/// missing, not UTF-8 or larger than [`PLIST_BYTES_MAX`].
pub(super) fn bounded_text(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(PLIST_BYTES_MAX + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= PLIST_BYTES_MAX).then_some(text)
}

/// The text of a one-line plist `<string>` element, with XML's five escapes undone. Input:
/// the line. Output: the text, or none for a line that holds no such element.
pub(super) fn plist_string(line: &str) -> Option<String> {
    let inner = line
        .trim()
        .strip_prefix("<string>")?
        .strip_suffix("</string>")?;
    Some(
        inner
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

/// Reads back the first word of an ExecStart= line as [`systemd_quoted`] wrote it.
/// Input: the line after `ExecStart=`. Output: the word, or none when it is not quoted so.
pub(super) fn systemd_unquoted(text: &str) -> Option<String> {
    let mut characters = text.strip_prefix('"')?.chars();
    let mut word = String::new();
    while let Some(character) = characters.next() {
        match character {
            '"' => return Some(word),
            '\\' => word.push(characters.next()?),
            '%' | '$' => {
                if characters.next()? != character {
                    return None;
                }
                word.push(character);
            }
            other => word.push(other),
        }
    }
    None
}

/// Quotes one word of an ExecStart= line, where systemd also expands `$` variables, so a
/// `$` is doubled on top of [`systemd_environment_quoted`]. Input: the word. Output: the
/// quoted text.
pub(super) fn systemd_quoted(text: &str) -> String {
    systemd_environment_quoted(&text.replace('$', "$$"))
}

/// Quotes a value for systemd: backslash and double quote are escaped and `%` (a specifier)
/// is doubled. Environment= expands no variables, so `$` is written as is. Input: the value.
/// Output: the quoted text.
pub(super) fn systemd_environment_quoted(text: &str) -> String {
    let mut quoted = String::from("\"");
    for character in text.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '%' => quoted.push_str("%%"),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

pub(super) fn windows_quoted(text: &str) -> String {
    format!("\"{text}\"")
}
