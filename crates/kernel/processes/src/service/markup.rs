use kernel_common::Platform;
use std::ffi::OsStr;
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

/// Reads a definition file in the encoding its manager reads: UTF-16LE after a byte order
/// mark for a Windows task ([`super::write_definition`]), UTF-8 elsewhere ([`bounded_text`]).
/// Inputs: the path and the platform. Output: the text, or none when it is missing, larger
/// than [`PLIST_BYTES_MAX`] or not in that encoding.
pub(super) fn bounded_definition(path: &Path, platform: Platform) -> Option<String> {
    use std::io::Read;
    if platform != Platform::Windows {
        return bounded_text(path);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(PLIST_BYTES_MAX + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let (pairs, rest) = bytes.strip_prefix(&[0xff, 0xfe])?.as_chunks::<2>();
    if bytes.len() as u64 > PLIST_BYTES_MAX || !rest.is_empty() {
        return None;
    }
    let units: Vec<u16> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
    String::from_utf16(&units).ok()
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

/// The characters of cmd's line a Windows logon task leaves unused, so a small difference
/// in how cmd counts its line never makes the task's PATH carry it past 8191.
pub(super) const TASK_LINE_MARGIN: usize = 64;

/// The PATH a Windows logon task sets before it runs the engine: the service PATH's folders
/// in order, leaving out empty entries and any folder holding `%`, which cmd expands even
/// inside quotes, or `"`, and every folder from the first one past `budget` UTF-16 units
/// on. Inputs: the service PATH and the units of cmd's line left for it. Output: the folders
/// joined by `;`, or none when no folder is left.
pub(super) fn windows_task_path(path: &OsStr, budget: usize) -> Option<String> {
    let text = path.to_string_lossy();
    let mut kept = String::new();
    let mut units = 0;
    for folder in text.split(';') {
        if folder.is_empty() || folder.contains(['%', '"', char::REPLACEMENT_CHARACTER]) {
            continue;
        }
        let joined = units + usize::from(!kept.is_empty()) + folder.encode_utf16().count();
        if joined > budget {
            break;
        }
        if !kept.is_empty() {
            kept.push(';');
        }
        kept.push_str(folder);
        units = joined;
        assert_eq!(kept.encode_utf16().count(), units);
    }
    assert!(units <= budget);
    (!kept.is_empty()).then_some(kept)
}
