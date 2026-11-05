use super::{Document, ENTRY_NAME_BYTES_MAX, Folder, clip, file_text, open_folder, open_in};
use crate::onboarding::FoundResearch;
use crate::project_setup::{DOCUMENT_PATH_BYTES_MAX, ResearchKind};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, Stat};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
const IGNORE_CHECK_TIME: Duration = Duration::from_secs(20);
const IGNORE_POLL: Duration = Duration::from_millis(5);
const IGNORE_OUTPUT_BYTES_MAX: usize = 8 * 1024 * 1024;
const IGNORES_UNKNOWN: &str =
    "No project document was read: the project's git could not tell which files it ignores.";

/// Words that mark a file or folder name as one that may hold a secret: credentials, keys
/// and tokens, and key-file extensions such as `.pem` or `.env` kept even when renamed
/// `.txt`. The mapping prompt goes to the project's model provider, and a document is
/// chosen by its name alone before any text is read, so a name with one of these words, or
/// two adjacent words joining into one, is neither read nor listed. A few documents with
/// such names, such as `key-metrics.md`, are left out with them.
const SECRET_WORDS: [&str; 19] = [
    "secret",
    "secrets",
    "credential",
    "credentials",
    "password",
    "passwords",
    "passwd",
    "token",
    "tokens",
    "apikey",
    "apikeys",
    "privatekey",
    "pem",
    "key",
    "keys",
    "p12",
    "pfx",
    "env",
    "netrc",
];

/// One file the walk found below the project folder: its path with `/` between names, its
/// size and whether its text is read or only its path and size are listed.
struct Found {
    path: String,
    size: u64,
    text: bool,
}

/// Reads the project's own documents beneath `repo`, never through a link: Markdown and
/// plain text files up to four folders deep, the docs, doc, documentation, research and notes
/// folders first, then root files, then the rest, shallower first. Hidden names, names that
/// look like secrets, private and build or vendor folders, other mounts, hard-linked files,
/// files over 1 MiB and whatever the project's git ignores are never opened or listed. Each
/// text is cut at 6 KiB, all of them at 48 KiB and 40 files; files past that and PDF, Word
/// and CSV files are only listed. Takes the folder and the README name already read. Returns
/// the documents read and the paths and sizes listed, or `None` when the project's git could
/// not say which files it ignores, so none is read.
pub(super) fn read(
    repo: &Path,
    readme: Option<&str>,
) -> Option<(Vec<Document>, Vec<(String, u64)>)> {
    let Some((root, device)) = open_folder(repo)
        .and_then(|root| rustix::fs::fstat(&root).ok().map(|device| (root, device)))
    else {
        return Some((Vec::new(), Vec::new()));
    };
    let mut found = Vec::new();
    let mut visited = 0;
    walk(&root, &device, "", 0, &mut visited, &mut found);
    assert!(visited <= ENTRIES_MAX);
    found.retain(|file| Some(file.path.as_str()) != readme);
    let paths: Vec<&str> = found.iter().map(|file| file.path.as_str()).collect();
    let ignored = ignored(repo, &paths)?;
    assert_eq!(ignored.len(), found.len());
    let mut found: Vec<Found> = found
        .into_iter()
        .zip(ignored)
        .filter_map(|(file, ignored)| (!ignored).then_some(file))
        .collect();
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
    Some((documents, unread))
}

/// Walks `folder`, found at `at` below the project folder and `depth` folders deep, adding
/// each document file it may read or list to `found`. Entries are taken in name order, at
/// most 4,000 in all, statted without following links; a root's document folders go first.
/// A file with more than one link is skipped, since its other name may be outside the
/// project, and so is a folder on another device than the root, such as a mount.
/// Takes the open folder, the root's status, its path, its depth, the entries visited so far
/// and the list.
fn walk(
    folder: &Folder,
    root: &Stat,
    at: &str,
    depth: usize,
    visited: &mut usize,
    found: &mut Vec<Found>,
) {
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
            && !secret_like(name)
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
                    && stat.st_nlink == 1
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
        if rustix::fs::fstat(&child).is_ok_and(|stat| stat.st_dev == root.st_dev) {
            walk(&child, root, &path, depth + 1, visited, found);
        }
    }
}

/// Whether `name` is one of `list`, ignoring ASCII case.
fn named(list: &[&str], name: &str) -> bool {
    list.iter().any(|known| known.eq_ignore_ascii_case(name))
}

/// Whether the file or folder `name` looks like it holds a secret: one of its words, split
/// at every character that is not an ASCII letter or digit, or two adjacent words joined,
/// is in [`SECRET_WORDS`], ignoring case. Takes the full name, extensions included.
fn secret_like(name: &str) -> bool {
    let words: Vec<&str> = name
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    words.iter().any(|word| named(&SECRET_WORDS, word))
        || words
            .windows(2)
            .any(|pair| named(&SECRET_WORDS, &format!("{}{}", pair[0], pair[1])))
}

/// Asks the project's own git, in one `check-ignore` call, which of `paths` it ignores by
/// the repository's ignore files, info/exclude and configured excludes file: the real git
/// with no system or global config, hooks, fsmonitor or pager, given 20 s and 8 MiB of
/// output. Each name goes in behind `./`, so one starting with `:` is never pathspec magic.
/// Git must name the ignored paths exactly as sent and in order; any other answer counts as
/// none, so the mapping never guesses which files git keeps out. Takes the project folder and
/// the paths below it. Returns whether each path is ignored, or `None` when git is missing,
/// fails, runs over or answers out of shape.
fn ignored(repo: &Path, paths: &[&str]) -> Option<Vec<bool>> {
    if paths.is_empty() {
        return Some(Vec::new());
    }
    let sent: Vec<String> = paths.iter().map(|path| format!("./{path}")).collect();
    let input: Vec<u8> = sent
        .iter()
        .flat_map(|path| path.bytes().chain([0]))
        .collect();
    let mut command = Command::new(crate::research::runner::setup_git());
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("--no-optional-locks")
        .args(crate::research::runner::GIT_SAFE)
        .args(["check-ignore", "-z", "--stdin", "--no-index"])
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let (code, output) = answer(&mut command, input)?;
    let names: Vec<&[u8]> = match output.split_last() {
        None => Vec::new(),
        Some((0, body)) => body.split(|byte| *byte == 0).collect(),
        Some(_) => return None,
    };
    if !matches!((code, names.is_empty()), (0, false) | (1, true)) {
        return None;
    }
    let mut ignored = vec![false; paths.len()];
    let mut next = 0;
    for name in names {
        let at = (next..sent.len()).find(|&index| sent[index].as_bytes() == name)?;
        ignored[at] = true;
        next = at + 1;
    }
    Some(ignored)
}

/// Runs `command` within [`IGNORE_CHECK_TIME`], writing `input` to it while keeping at most
/// [`IGNORE_OUTPUT_BYTES_MAX`] bytes of its output and draining the rest, so it never blocks
/// on a full pipe. Takes the command and its input. Returns the exit code and the output, or
/// `None` when it cannot start, runs past the time, is stopped by a signal, does not take
/// all its input or prints more.
fn answer(command: &mut Command, input: Vec<u8>) -> Option<(i32, Vec<u8>)> {
    let mut child = crate::spawn_gate::spawn(command).ok()?;
    let writer = child
        .stdin
        .take()
        .map(|mut stdin| std::thread::spawn(move || stdin.write_all(&input)));
    let reader = child.stdout.take().map(|stdout| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut source = stdout.take(IGNORE_OUTPUT_BYTES_MAX as u64 + 1);
            let read = source.read_to_end(&mut bytes);
            let _ = std::io::copy(&mut source.into_inner(), &mut std::io::sink());
            read.map(|_| bytes)
        })
    });
    let deadline = Instant::now() + IGNORE_CHECK_TIME;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(IGNORE_POLL),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let wrote = writer.is_some_and(|writer| writer.join().is_ok_and(|written| written.is_ok()));
    let output = reader.and_then(|reader| reader.join().ok()?.ok());
    let (code, output) = (status?.code()?, output?);
    (wrote && output.len() <= IGNORE_OUTPUT_BYTES_MAX).then_some((code, output))
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
/// never read through, nor a file given a second link since. Takes the open root, the path
/// and the bound. Returns the trimmed text, or `None` for a missing, linked, special or empty
/// file.
fn text_beneath(root: &Folder, path: &str, max: usize) -> Option<String> {
    let mut parts: Vec<&str> = path.split('/').collect();
    let name = parts.pop()?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut folder: Option<Folder> = None;
    for part in parts {
        let next = rustix::fs::openat(folder.as_ref().unwrap_or(root), part, flags, Mode::empty());
        folder = Some(next.ok()?);
    }
    let file = open_in(folder.as_ref().unwrap_or(root), name)?;
    if file.metadata().ok()?.nlink() != 1 {
        return None;
    }
    let text = file_text(file, max as u64)?;
    let text = clip(&text, max).trim_end().to_owned();
    assert!(text.len() <= max);
    (!text.is_empty()).then_some(text)
}

/// The Project documents part of the mapping prompt: each document read under its path and
/// size in a fence longer than any backtick run in its text, then the paths and sizes of the
/// documents whose text is not read, or why none was read when the project's git could not
/// say what it ignores. Takes the documents, the listed paths and whether git said nothing.
/// Returns the text, empty when there are none.
pub(super) fn section(
    documents: &[Document],
    unread: &[(String, u64)],
    ignores_unknown: bool,
) -> String {
    if ignores_unknown {
        assert!(documents.is_empty() && unread.is_empty());
        return format!("\n## Project documents\n\n{IGNORES_UNKNOWN}\n");
    }
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
