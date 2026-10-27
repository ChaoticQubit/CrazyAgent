use super::git::{OUTPUT_BYTES_MAX, Repo, failure, is_object_id};
use super::{ChangeDiff, ChangedFile, FileKind, FileMark, FileStatus};
use crate::engine::EngineError;
use std::collections::{BTreeMap, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

const DIFF_TEXT_BYTES_MAX: usize = 4 * 1024 * 1024;
const PAGE_BUDGET: usize = 24 * 1024;
const RECENT_BYTES_MAX: usize = 16 * 1024 * 1024;
const RECENT_MAX: usize = 32;

/// A change's diff computed once: its files and plain patch text, each cut into pages,
/// and whether git's output stopped at the byte bound.
struct Computed {
    files: Vec<ChangedFile>,
    lists: Vec<Range<usize>>,
    patch: String,
    texts: Vec<Range<usize>>,
    overflow: bool,
}

impl Computed {
    fn bytes(&self) -> usize {
        self.patch.len()
            + self
                .files
                .iter()
                .map(|file| file.path.len() + file.old_path.as_ref().map_or(0, String::len))
                .sum::<usize>()
    }
}

/// A change's repository with its base and head commits.
type Key = (PathBuf, String, String);

/// Computed diffs with their keys, least recently read first.
type Recent = VecDeque<(Key, Arc<Computed>)>;

/// The diffs computed for recent pages, at most `RECENT_MAX` and `RECENT_BYTES_MAX` apart
/// from the newest.
static RECENT: LazyLock<Mutex<Recent>> = LazyLock::new(Default::default);

/// Line counts of a change: how many paths changed and how many lines were added and
/// removed across them, binary files counting no lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Stats {
    pub(super) files: u32,
    pub(super) added: u32,
    pub(super) removed: u32,
}

/// Replaces every control character but newline and tab, and every bidirectional
/// override or isolate, with U+FFFD, so text shown to the owner cannot move a terminal
/// cursor or reorder what it shows. Input: the text. Output: the plain text.
pub(super) fn plain(text: &str) -> String {
    text.chars()
        .map(|c| {
            let bidi = matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}');
            if (c.is_control() && c != '\n' && c != '\t') || bidi {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}

fn text(bytes: &[u8]) -> Result<&str, EngineError> {
    std::str::from_utf8(bytes).map_err(|_| failure("changes_git_unsupported"))
}

/// Lists the paths that differ between two trees, each side of a rename apart.
/// Inputs: the repository and the two tree-ish ids. Output: the paths.
pub(super) fn changed_paths(repo: &Repo, from: &str, to: &str) -> Result<Vec<String>, EngineError> {
    let printed = repo.run(&[
        "diff-tree",
        "-r",
        "-z",
        "--name-only",
        "--no-renames",
        from,
        to,
    ])?;
    printed
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| text(path).map(str::to_owned))
        .collect()
}

/// Counts the lines a change adds and removes, with renames found.
/// Inputs: the repository and the two commits. Output: the counts.
pub(super) fn stats(repo: &Repo, base: &str, head: &str) -> Result<Stats, EngineError> {
    let mut stats = Stats::default();
    for (_, (added, removed)) in numstat(repo, base, head)? {
        stats.files = stats.files.saturating_add(1);
        stats.added = stats.added.saturating_add(added.unwrap_or(0));
        stats.removed = stats.removed.saturating_add(removed.unwrap_or(0));
    }
    Ok(stats)
}

/// Lines added and removed per changed path, keyed by the new path; None for a binary
/// file's counts.
type LineCounts = BTreeMap<String, (Option<u32>, Option<u32>)>;

/// Reads git's line counts per changed path, keyed by the new path; a binary file
/// counts None. Inputs: the repository and the two commits. Output: the counts.
fn numstat(repo: &Repo, base: &str, head: &str) -> Result<LineCounts, EngineError> {
    let printed = repo.run(&[
        "diff-tree",
        "-r",
        "-z",
        "-M",
        "--numstat",
        "--no-ext-diff",
        base,
        head,
    ])?;
    let mut fields = printed.split(|byte| *byte == 0);
    let mut counts = BTreeMap::new();
    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        let record = text(record)?;
        let mut parts = record.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(failure("changes_git_failed"));
        };
        let path = if path.is_empty() {
            fields.next().ok_or_else(|| failure("changes_git_failed"))?;
            text(fields.next().ok_or_else(|| failure("changes_git_failed"))?)?
        } else {
            path
        };
        counts.insert(path.to_owned(), (added.parse().ok(), removed.parse().ok()));
    }
    Ok(counts)
}

fn kind_of(mode: &str, binary: bool) -> FileKind {
    match mode {
        "120000" => FileKind::Symlink,
        "160000" => FileKind::Gitlink,
        _ if binary => FileKind::Binary,
        _ => FileKind::Text,
    }
}

fn named(path: &str, name: &str) -> bool {
    path.rsplit('/').next() == Some(name)
}

/// Lists the files a change touches with their status, kind and the marks the owner
/// should look at. Inputs: the repository and the two commits. Output: the files.
pub(super) fn files(repo: &Repo, base: &str, head: &str) -> Result<Vec<ChangedFile>, EngineError> {
    let counts = numstat(repo, base, head)?;
    let printed = repo.run(&[
        "diff-tree",
        "-r",
        "-z",
        "-M",
        "--raw",
        "--no-abbrev",
        base,
        head,
    ])?;
    let mut fields = printed.split(|byte| *byte == 0);
    let mut files = Vec::new();
    while let Some(header) = fields.next() {
        if header.is_empty() {
            continue;
        }
        let header = text(header)?;
        let parts: Vec<&str> = header.trim_start_matches(':').split(' ').collect();
        let [old_mode, new_mode, _, _, status] = parts[..] else {
            return Err(failure("changes_git_failed"));
        };
        let first = text(fields.next().ok_or_else(|| failure("changes_git_failed"))?)?;
        let renamed = status.starts_with('R') || status.starts_with('C');
        let (path, old_path) = if renamed {
            let second = text(fields.next().ok_or_else(|| failure("changes_git_failed"))?)?;
            (second, Some(first))
        } else {
            (first, None)
        };
        let status = match status.as_bytes().first() {
            Some(b'A') => FileStatus::Added,
            Some(b'D') => FileStatus::Deleted,
            Some(b'R' | b'C') => FileStatus::Renamed,
            _ => FileStatus::Modified,
        };
        let shown_mode = if status == FileStatus::Deleted {
            old_mode
        } else {
            new_mode
        };
        let binary = counts.get(path).is_some_and(|(added, _)| added.is_none());
        let mut marks = Vec::new();
        if new_mode == "100755" {
            marks.push(FileMark::Executable);
        }
        if old_mode == "120000" || new_mode == "120000" {
            marks.push(FileMark::Symlink);
        }
        if named(path, ".gitattributes") || old_path.is_some_and(|old| named(old, ".gitattributes"))
        {
            marks.push(FileMark::Attributes);
        }
        if named(path, ".gitmodules") || old_path.is_some_and(|old| named(old, ".gitmodules")) {
            marks.push(FileMark::Modules);
        }
        if old_mode != "000000" && new_mode != "000000" && old_mode != new_mode {
            marks.push(FileMark::Mode);
        }
        files.push(ChangedFile {
            path: plain(path),
            old_path: old_path.map(plain),
            status,
            kind: kind_of(shown_mode, binary),
            marks,
        });
    }
    Ok(files)
}

/// The bytes one character of plain text takes inside a JSON reply.
/// Input: the character. Output: the bytes.
fn escaped(c: char) -> usize {
    match c {
        '"' | '\\' | '\n' | '\t' => 2,
        _ => c.len_utf8(),
    }
}

/// Cuts plain text into pages that each fit `PAGE_BUDGET` once escaped, at a line end
/// where one falls inside the page. Input: the text. Output: the pages as byte ranges,
/// in order.
fn text_pages(text: &str) -> Vec<Range<usize>> {
    let mut pages = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let rest = &text[start..];
        let mut size = 0;
        let mut end = rest.len();
        let mut line_end = None;
        for (at, c) in rest.char_indices() {
            size += escaped(c);
            if size > PAGE_BUDGET {
                end = line_end.unwrap_or(at);
                break;
            }
            if c == '\n' {
                line_end = Some(at + 1);
            }
        }
        assert!(end > 0 && rest.is_char_boundary(end));
        pages.push(start..start + end);
        start += end;
    }
    pages
}

/// Cuts the file list into pages that each fit `PAGE_BUDGET` once encoded.
/// Input: the files. Output: the pages as index ranges, in order.
fn file_pages(files: &[ChangedFile]) -> Result<Vec<std::ops::Range<usize>>, EngineError> {
    let mut pages = Vec::new();
    let mut start = 0;
    let mut size = 0;
    for (at, file) in files.iter().enumerate() {
        let bytes = serde_json::to_vec(file)
            .map_err(|_| failure("changes_reply_bounds"))?
            .len()
            + 1;
        if at > start && size + bytes > PAGE_BUDGET {
            pages.push(start..at);
            start = at;
            size = 0;
        }
        size += bytes;
    }
    if start < files.len() {
        pages.push(start..files.len());
    }
    Ok(pages)
}

/// Computes a change's diff: its files and patch text with no external diff or text
/// conversion, bounded to `DIFF_TEXT_BYTES_MAX`, made plain and cut into pages.
/// Inputs: the repository and the two commits. Output: the computed diff.
fn compute(repo: &Repo, base: &str, head: &str) -> Result<Computed, EngineError> {
    let files = files(repo, base, head)?;
    let output = repo.exec(
        &[
            "diff-tree",
            "-r",
            "-p",
            "-M",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            base,
            head,
        ],
        None,
        &[],
        DIFF_TEXT_BYTES_MAX.min(OUTPUT_BYTES_MAX),
    )?;
    if output.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    let patch = plain(&String::from_utf8_lossy(&output.stdout));
    let texts = text_pages(&patch);
    let lists = file_pages(&files)?;
    Ok(Computed {
        files,
        lists,
        patch,
        texts,
        overflow: output.overflow,
    })
}

/// Reads a change's diff from the recent ones, or computes it and keeps it when both
/// sides are commit ids, whose diff never changes. Inputs: the repository and the two
/// commits. Output: the computed diff.
fn computed(repository: &Path, base: &str, head: &str) -> Result<Arc<Computed>, EngineError> {
    let key = (repository.to_path_buf(), base.to_owned(), head.to_owned());
    let lasting = is_object_id(base) && is_object_id(head);
    if lasting {
        let mut recent = RECENT.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = recent
            .iter()
            .position(|(seen, _)| *seen == key)
            .and_then(|at| recent.remove(at))
        {
            let found = Arc::clone(&entry.1);
            recent.push_back(entry);
            return Ok(found);
        }
    }
    let fresh = Arc::new(compute(&super::flow::shared(repository)?, base, head)?);
    if lasting {
        let mut recent = RECENT.lock().unwrap_or_else(PoisonError::into_inner);
        recent.retain(|(seen, _)| *seen != key);
        recent.push_back((key, Arc::clone(&fresh)));
        while recent.len() > 1
            && (recent.len() > RECENT_MAX
                || recent.iter().map(|(_, kept)| kept.bytes()).sum::<usize>() > RECENT_BYTES_MAX)
        {
            recent.pop_front();
        }
        assert!(recent.len() <= RECENT_MAX);
        assert!(
            recent
                .back()
                .is_some_and(|(_, kept)| Arc::ptr_eq(kept, &fresh))
        );
    }
    Ok(fresh)
}

/// Builds one page of a change's diff from the change's diff computed once, so paging
/// does not rerun git or rescan the patch. Inputs: the repository, the two commits and
/// the page. Output: the page.
pub(super) fn page(
    repository: &Path,
    base: &str,
    head: &str,
    page: u16,
) -> Result<ChangeDiff, EngineError> {
    let computed = computed(repository, base, head)?;
    let count = computed.texts.len().max(computed.lists.len()).max(1);
    let index = usize::from(page);
    let truncated = computed.overflow || count > usize::from(u16::MAX);
    let next = (index + 1 < count)
        .then(|| u16::try_from(index + 1).ok())
        .flatten();
    let shown = computed
        .lists
        .get(index)
        .map_or(&[][..], |range| &computed.files[range.clone()]);
    assert!(next.is_none_or(|next| next > page));
    Ok(ChangeDiff {
        files: shown.to_vec(),
        text: computed.texts.get(index).map_or_else(String::new, |range| {
            computed.patch[range.clone()].to_owned()
        }),
        next,
        truncated,
    })
}
