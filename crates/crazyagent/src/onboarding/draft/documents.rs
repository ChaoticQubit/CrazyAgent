use super::{Document, ENTRY_NAME_BYTES_MAX, Folder, clip, file_text, open_folder, open_in};
use crate::onboarding::FoundResearch;
use crate::project_setup::{DOCUMENT_PATH_BYTES_MAX, ResearchKind};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};
use std::path::Path;

const TEXT_EXTENSIONS: [&str; 7] = ["md", "markdown", "mdx", "txt", "rst", "adoc", "org"];
const LISTED_EXTENSIONS: [&str; 3] = ["pdf", "docx", "csv"];
const DOCUMENT_FOLDERS: [&str; 5] = ["docs", "doc", "documentation", "research", "notes"];
const UNREAD_FOLDERS: [&str; 11] = [
    "private",
    "graphify-out",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "vendor",
    "venv",
    "__pycache__",
    "Pods",
];
const DEPTH_MAX: usize = 4;
const ENTRIES_MAX: usize = 4000;
const FILE_BYTES_MAX: u64 = 1024 * 1024;
const TEXT_BYTES_MAX: usize = 6 * 1024;
const TOTAL_TEXT_BYTES_MAX: usize = 48 * 1024;
const READ_MAX: usize = 40;
const UNREAD_MAX: usize = 64;
const RESEARCH_FILES_MAX: usize = 8;

/// One file the walk found below the project folder: its path with `/` between names, its
/// size and whether its text is read or only its path and size are listed.
struct Found {
    path: String,
    size: u64,
    text: bool,
}

/// Reads the project's own documents beneath `repo`, never through a link: Markdown and
/// plain text files up to four folders deep, the docs, doc, documentation, research and notes
/// folders first, then root files, then the rest, shallower first. Hidden names, private and
/// build or vendor folders and files over 1 MiB are never opened. Each text is cut at 6 KiB,
/// all of them at 48 KiB and 40 files; files past that and PDF, Word and CSV files are only
/// listed. Takes the folder and the README name already read. Returns the documents read and
/// the paths and sizes listed.
pub(super) fn read(repo: &Path, readme: Option<&str>) -> (Vec<Document>, Vec<(String, u64)>) {
    let Some(root) = open_folder(repo) else {
        return (Vec::new(), Vec::new());
    };
    let mut found = Vec::new();
    let mut visited = 0;
    walk(&root, "", 0, &mut visited, &mut found);
    assert!(visited <= ENTRIES_MAX);
    found.retain(|file| Some(file.path.as_str()) != readme);
    found.sort_by(|left, right| rank(&left.path).cmp(&rank(&right.path)));
    let (mut documents, mut unread) = (Vec::new(), Vec::new());
    let mut budget = TOTAL_TEXT_BYTES_MAX;
    for file in found {
        if file.text && documents.len() < READ_MAX && budget > 0 {
            let max = TEXT_BYTES_MAX.min(budget);
            if let Some(text) = text_beneath(&root, &file.path, max) {
                budget -= text.len();
                documents.push(Document {
                    cut: file.size > max as u64,
                    path: file.path,
                    size: file.size,
                    text,
                });
            }
            continue;
        }
        if unread.len() < UNREAD_MAX {
            unread.push((file.path, file.size));
        }
    }
    let total: usize = documents.iter().map(|document| document.text.len()).sum();
    assert!(total <= TOTAL_TEXT_BYTES_MAX);
    assert!(documents.len() <= READ_MAX && unread.len() <= UNREAD_MAX);
    (documents, unread)
}

/// Walks `folder`, found at `at` below the project folder and `depth` folders deep, adding
/// each document file it may read or list to `found`. Entries are taken in name order, at
/// most 4,000 in all, statted without following links; a root's document folders go first.
/// Takes the open folder, its path, its depth, the entries visited so far and the list.
fn walk(folder: &Folder, at: &str, depth: usize, visited: &mut usize, found: &mut Vec<Found>) {
    let Ok(entries) = Dir::read_from(folder) else {
        return;
    };
    let mut names = Vec::new();
    for entry in entries {
        if *visited == ENTRIES_MAX {
            break;
        }
        *visited += 1;
        let Ok(entry) = entry else {
            break;
        };
        let Ok(name) = entry.file_name().to_str() else {
            continue;
        };
        if !name.starts_with('.')
            && name.len() <= ENTRY_NAME_BYTES_MAX
            && !name.chars().any(char::is_control)
        {
            names.push(name.to_owned());
        }
    }
    names.sort();
    let mut folders = Vec::new();
    for name in names {
        let Ok(stat) = rustix::fs::statat(folder, name.as_str(), AtFlags::SYMLINK_NOFOLLOW) else {
            continue;
        };
        let path = if at.is_empty() {
            name.clone()
        } else {
            format!("{at}/{name}")
        };
        match FileType::from_raw_mode(stat.st_mode) {
            FileType::Directory => {
                if depth < DEPTH_MAX && !named(&UNREAD_FOLDERS, &name) {
                    folders.push((name, path));
                }
            }
            FileType::RegularFile => {
                let size = u64::try_from(stat.st_size).unwrap_or(0);
                let text = extension_in(&name, &TEXT_EXTENSIONS);
                if (text || extension_in(&name, &LISTED_EXTENSIONS))
                    && size > 0
                    && size <= FILE_BYTES_MAX
                {
                    found.push(Found { path, size, text });
                }
            }
            _ => {}
        }
    }
    if depth == 0 {
        folders.sort_by_key(|(name, _)| !named(&DOCUMENT_FOLDERS, name));
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    for (name, path) in folders {
        if *visited == ENTRIES_MAX {
            return;
        }
        let Ok(child) = rustix::fs::openat(folder, name.as_str(), flags, Mode::empty()) else {
            continue;
        };
        walk(&child, &path, depth + 1, visited, found);
    }
}

/// Whether `name` is one of `list`, ignoring ASCII case.
fn named(list: &[&str], name: &str) -> bool {
    list.iter().any(|known| known.eq_ignore_ascii_case(name))
}

/// Whether the file `name` has one of the extensions in `list`, ignoring ASCII case.
fn extension_in(name: &str, list: &[&str]) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(stem, extension)| !stem.is_empty() && named(list, extension))
}

/// Where a document stands in the reading order: files in a top document folder first, then
/// root files, then the rest, each shallower first, then by path. Takes the path. Returns the
/// sort key.
fn rank(path: &str) -> (u8, usize, &str) {
    let group = match path.split_once('/') {
        Some((top, _)) if named(&DOCUMENT_FOLDERS, top) => 0,
        None => 1,
        Some(_) => 2,
    };
    (group, path.matches('/').count(), path)
}

/// Reads up to `max` bytes of the text file at `path` below `root`, opening each folder and
/// the file itself without following a link, so a path swapped for a link since the walk is
/// never read through. Takes the open root, the path and the bound. Returns the trimmed text,
/// or `None` for a missing, linked, special or empty file.
fn text_beneath(root: &Folder, path: &str, max: usize) -> Option<String> {
    let mut parts: Vec<&str> = path.split('/').collect();
    let name = parts.pop()?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut folder: Option<Folder> = None;
    for part in parts {
        let next = rustix::fs::openat(folder.as_ref().unwrap_or(root), part, flags, Mode::empty());
        folder = Some(next.ok()?);
    }
    let text = file_text(open_in(folder.as_ref().unwrap_or(root), name)?, max as u64)?;
    let text = clip(&text, max).trim_end().to_owned();
    assert!(text.len() <= max);
    (!text.is_empty()).then_some(text)
}

/// The Project documents part of the mapping prompt: each document read under its path and
/// size in a fence longer than any backtick run in its text, then the paths and sizes of the
/// documents whose text is not read. Takes the documents and the listed paths. Returns the
/// text, empty when there are none.
pub(super) fn section(documents: &[Document], unread: &[(String, u64)]) -> String {
    let mut text = String::new();
    if documents.is_empty() && unread.is_empty() {
        return text;
    }
    text.push_str("\n## Project documents\n\n");
    for document in documents {
        let shown = if document.cut {
            format!(", the first {} shown", document.text.len())
        } else {
            String::new()
        };
        let fence = "`".repeat(longest_run(&document.text).max(2) + 1);
        text.push_str(&format!(
            "### {} ({} bytes{shown})\n\n{fence}\n{}\n{fence}\n\n",
            document.path, document.size, document.text
        ));
    }
    if !unread.is_empty() {
        let listed: Vec<String> = unread
            .iter()
            .map(|(path, size)| format!("{path} ({size} bytes)"))
            .collect();
        text.push_str(&format!(
            "Also in the repository, text not read: {}\n",
            listed.join(", ")
        ));
    }
    text
}

/// The longest run of backticks in `text`.
fn longest_run(text: &str) -> usize {
    text.split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0)
}

/// The research an answer's `research` value names: known kinds only, each once in kind
/// order with at most eight files, still to be checked against the documents shown.
/// Takes the JSON value. Returns the kinds and their files.
pub(super) fn research(value: &serde_json::Value) -> Vec<FoundResearch> {
    let mut found: Vec<FoundResearch> = Vec::new();
    let items = value.as_array().into_iter().flatten();
    for item in items.take(ResearchKind::ALL.len() * 2) {
        let Some(kind) = item
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .and_then(|name| ResearchKind::parse(name.trim()))
        else {
            continue;
        };
        let files = paths(
            item.get("files").unwrap_or(&serde_json::Value::Null),
            RESEARCH_FILES_MAX,
        );
        match found.iter_mut().find(|known| known.kind == kind) {
            Some(known) => {
                for file in files {
                    if known.files.len() < RESEARCH_FILES_MAX && !known.files.contains(&file) {
                        known.files.push(file);
                    }
                }
            }
            None if !files.is_empty() => found.push(FoundResearch { kind, files }),
            None => {}
        }
    }
    found.sort_by_key(|known| known.kind);
    assert!(found.len() <= ResearchKind::ALL.len());
    found
}

/// The file paths an answer's JSON list names: trimmed, a leading `./` dropped, each once,
/// at most `max`, none empty, past the path bound or holding control characters. Takes the
/// JSON value and the bound. Returns the paths.
pub(super) fn paths(value: &serde_json::Value, max: usize) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    let listed = value.as_array().into_iter().flatten();
    for path in listed.take(max * 2).filter_map(serde_json::Value::as_str) {
        if paths.len() == max {
            break;
        }
        let path = path.trim();
        let path = path.strip_prefix("./").unwrap_or(path);
        if !path.is_empty()
            && path.len() <= DOCUMENT_PATH_BYTES_MAX
            && !path.chars().any(char::is_control)
            && !paths.iter().any(|known| known == path)
        {
            paths.push(path.to_owned());
        }
    }
    assert!(paths.len() <= max);
    paths
}
