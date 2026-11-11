use super::library::{SCOPE_ITEMS_MAX, TEXT_BYTES_MAX};
use super::{error, hidden_text};
use crate::engine::EngineError;
use serde::Serialize;
use std::io::Read;
use std::path::Path;

pub const ENTRIES_MAX: usize = 1000;
pub const FILE_BYTES_MAX: u64 = 64 * 1024;
pub const FILE_NAME_BYTES_MAX: usize = 255;
const INDEX_FILE: &str = "MEMORY.md";

/// One markdown file read for import: its name inside the folder and its text
/// without frontmatter, trimmed, within the saved-memory bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportFile {
    pub file: String,
    pub text: String,
}

/// What a folder holds for import: the readable markdown files in name order and
/// how many markdown files were refused for size, type, encoding or content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportScan {
    pub files: Vec<ImportFile>,
    pub skipped: u32,
}

/// How an import ended: memories saved and markdown files not saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct ImportReport {
    pub imported: u32,
    pub skipped: u32,
}

/// Checks the name an imported memory keeps as its source.
/// Input: the untrusted file name. Output: true for one plain path component
/// ending in .md, up to 255 bytes, without control characters.
pub fn file_name_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= FILE_NAME_BYTES_MAX
        && name.ends_with(".md")
        && !name.starts_with('.')
        && !name.contains(['/', '\\'])
        && !name.chars().any(char::is_control)
}

/// Checks the text of one imported memory.
/// Input: the untrusted text. Output: true when it is trimmed, not blank, within
/// the saved-memory size and free of hidden or control characters.
pub fn text_valid(text: &str) -> bool {
    !text.is_empty() && text.trim() == text && text.len() <= TEXT_BYTES_MAX && !hidden_text(text)
}

/// Reads the markdown memory files of a folder the owner chose, one level deep.
/// Input: an absolute path to a real folder, not a link. Output: up to 200 files
/// in name order; MEMORY.md and hidden files are left out, links and anything that
/// is not a regular file count as skipped, and no link inside a file is followed.
pub fn read_folder(folder: &Path) -> Result<ImportScan, EngineError> {
    if !folder.is_absolute() {
        return Err(error("memory_import_folder_invalid"));
    }
    let meta =
        std::fs::symlink_metadata(folder).map_err(|_| error("memory_import_folder_invalid"))?;
    if !meta.is_dir() {
        return Err(error("memory_import_folder_invalid"));
    }
    let entries = std::fs::read_dir(folder).map_err(|_| error("memory_import_folder_invalid"))?;
    let mut names = Vec::new();
    let mut skipped = 0u32;
    for entry in entries.take(ENTRIES_MAX) {
        let Ok(entry) = entry else {
            skipped += 1;
            continue;
        };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            skipped += 1;
            continue;
        };
        if name == INDEX_FILE || name.starts_with('.') || !name.ends_with(".md") {
            continue;
        }
        if file_name_valid(&name) {
            names.push(name);
        } else {
            skipped += 1;
        }
    }
    names.sort();
    let mut files = Vec::new();
    for name in names {
        if files.len() >= SCOPE_ITEMS_MAX {
            skipped += 1;
            continue;
        }
        match read_file(&folder.join(&name)) {
            Some(text) => files.push(ImportFile { file: name, text }),
            None => skipped += 1,
        }
    }
    assert!(files.len() <= SCOPE_ITEMS_MAX);
    assert!(
        files
            .iter()
            .all(|f| file_name_valid(&f.file) && text_valid(&f.text))
    );
    Ok(ImportScan { files, skipped })
}

fn read_file(path: &Path) -> Option<String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
        );
    }
    #[cfg(not(unix))]
    {
        if !std::fs::symlink_metadata(path).ok()?.is_file() {
            return None;
        }
    }
    let file = options.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > FILE_BYTES_MAX {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(FILE_BYTES_MAX + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > FILE_BYTES_MAX {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?.replace("\r\n", "\n");
    let body = without_frontmatter(&text).trim().to_owned();
    text_valid(&body).then_some(body)
}

/// Drops a leading YAML frontmatter block fenced by `---` lines.
/// Input: the file text with LF line ends. Output: the text after the closing
/// fence, or the whole text when it has no complete frontmatter block.
pub fn without_frontmatter(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("---\n") else {
        return text;
    };
    if let Some(body) = rest.strip_prefix("---\n") {
        return body;
    }
    match rest.find("\n---\n") {
        Some(at) => &rest[at + 5..],
        None if rest.ends_with("\n---") => "",
        None => text,
    }
}

#[cfg(all(test, unix))]
mod tests;
