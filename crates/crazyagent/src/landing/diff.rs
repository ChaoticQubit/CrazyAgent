use super::git::{LISTING_BYTES_MAX, OUTPUT_BYTES_MAX, Repo, failure, is_object_id};
use super::{ChangeDiff, ChangedFile, FileKind, FileMark, FileStatus};
use crate::engine::EngineError;
use crate::ide::agent_config;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

const DIFF_TEXT_BYTES_MAX: usize = 4 * 1024 * 1024;
const PAGE_BUDGET: usize = 24 * 1024;
const RECENT_BYTES_MAX: usize = 16 * 1024 * 1024;
pub(super) const RECENT_MAX: usize = 32;
const LISTS_BYTES_MAX: usize = 4 * 1024 * 1024;
const LISTS_MAX: usize = 64;
const FED_SOURCES_MAX: usize = 16;

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

/// A change's repository with its base and head commits, or a file's repository, commit
/// and path.
pub(super) type Key = (PathBuf, String, String);

/// Values kept with their keys, least recently read first.
pub(super) type Recent<T> = VecDeque<(Key, Arc<T>)>;

/// The diffs computed for recent pages, at most `RECENT_MAX` and `RECENT_BYTES_MAX` apart
/// from the newest.
static RECENT: LazyLock<Mutex<Recent<Computed>>> = LazyLock::new(Default::default);

/// The file lists of recent changes, at most `LISTS_MAX` and `LISTS_BYTES_MAX` of paths
/// apart from the newest. They are kept apart from the diffs, so a list the IDE polls
/// stays while larger diffs push its own diff out.
static LISTS: LazyLock<Mutex<Recent<[ChangedFile]>>> = LazyLock::new(Default::default);

/// Takes a kept value and makes it the newest. Inputs: the cache and the key. Output: the
/// value, or None when it is not kept.
pub(super) fn recall<T: ?Sized>(cache: &Mutex<Recent<T>>, key: &Key) -> Option<Arc<T>> {
    let mut recent = cache.lock().unwrap_or_else(PoisonError::into_inner);
    let entry = recent
        .iter()
        .position(|(seen, _)| seen == key)
        .and_then(|at| recent.remove(at))?;
    let found = Arc::clone(&entry.1);
    recent.push_back(entry);
    Some(found)
}

/// Keeps a value as the newest, then drops the oldest ones while more than `most` are kept
/// or their `size` passes `bytes`, never the newest. Inputs: the cache, the key, the value,
/// the two bounds and the size of one value. Output: none.
pub(super) fn keep<T: ?Sized>(
    cache: &Mutex<Recent<T>>,
    key: Key,
    value: &Arc<T>,
    most: usize,
    bytes: usize,
    size: impl Fn(&T) -> usize,
) {
    assert!(most > 0);
    let mut recent = cache.lock().unwrap_or_else(PoisonError::into_inner);
    recent.retain(|(seen, _)| *seen != key);
    recent.push_back((key, Arc::clone(value)));
    while recent.len() > 1
        && (recent.len() > most
            || recent.iter().map(|(_, kept)| size(&**kept)).sum::<usize>() > bytes)
    {
        recent.pop_front();
    }
    assert!(recent.len() <= most);
    assert!(
        recent
            .back()
            .is_some_and(|(_, kept)| Arc::ptr_eq(kept, value))
    );
}

/// Line counts of a change: how many paths changed and how many lines were added and
/// removed across them, binary files counting no lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Stats {
    pub(super) files: u32,
    pub(super) added: u32,
    pub(super) removed: u32,
}

pub(super) use super::plain;

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

/// A path as a change leaves it: its bytes, its new mode and its new object id, which is
/// all zeros for a deletion.
pub(super) type PathState = (Vec<u8>, String, String);

/// Reads git's `--raw -z` records into the states they leave. Input: the output, whose
/// records may have blank separators between commits. Output: the states.
fn path_states(printed: &[u8]) -> Result<BTreeSet<PathState>, EngineError> {
    let mut states = BTreeSet::new();
    let mut fields = printed.split(|byte| *byte == 0);
    while let Some(field) = fields.next() {
        let record = field.trim_ascii_start();
        if !record.starts_with(b":") {
            continue;
        }
        let parts: Vec<&str> = text(record)?.split(' ').collect();
        let [_, mode, _, object, _] = parts.as_slice() else {
            return Err(failure("changes_git_failed"));
        };
        let path = fields.next().ok_or_else(|| failure("changes_git_failed"))?;
        assert!(is_object_id(object));
        states.insert((path.to_vec(), (*mode).to_owned(), (*object).to_owned()));
    }
    Ok(states)
}

/// The states a change leaves on the paths it changes, each side of a rename apart.
/// Inputs: the repository and the two tree-ish ids. Output: the states.
pub(super) fn changed_states(
    repo: &Repo,
    from: &str,
    to: &str,
) -> Result<BTreeSet<PathState>, EngineError> {
    path_states(&repo.run(&[
        "diff-tree",
        "-r",
        "-z",
        "--raw",
        "--no-renames",
        "--no-abbrev",
        from,
        to,
    ])?)
}

/// Every state the newest `commits_max` commits after `from` up to `tip` left, each side
/// of a rename apart, merges adding none of their own. Inputs: the repository, both ends
/// and the bound. Output: the states, None when they run past the output bound.
pub(super) fn states_since(
    repo: &Repo,
    from: &str,
    tip: &str,
    commits_max: usize,
) -> Result<Option<BTreeSet<PathState>>, EngineError> {
    assert!(commits_max > 0);
    let range = format!("{from}..{tip}");
    let count = format!("--max-count={commits_max}");
    let output = repo.exec(
        &[
            "log",
            "--format=",
            "--raw",
            "-z",
            "--no-renames",
            "--no-abbrev",
            "--diff-merges=off",
            "--no-show-signature",
            &count,
            &range,
        ],
        None,
        &[],
        OUTPUT_BYTES_MAX,
    )?;
    if output.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    if output.overflow {
        return Ok(None);
    }
    path_states(&output.stdout).map(Some)
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

/// Whether a path's file name opens the same file as `name` on a case-insensitive APFS
/// volume, as git and the IDE open it there. Inputs: the path and a lower-case ASCII name.
/// Output: true for that file.
fn named(path: &str, name: &str) -> bool {
    agent_config::same_name(path.rsplit('/').next().unwrap_or(path), name)
}

/// One entry of a git tree listing: its mode, object id and path in the project.
struct Entry {
    mode: String,
    object: String,
    path: String,
}

/// Lists one folder of a commit's tree without going into its subfolders, as far as
/// `LISTING_BYTES_MAX` of listing holds; entries past the bound are left out. Inputs: the
/// repository, the commit and the folder, None for the root. Output: the entries.
fn entries(repo: &Repo, commit: &str, folder: Option<&str>) -> Result<Vec<Entry>, EngineError> {
    let inside = folder.map(|folder| format!("{folder}/"));
    let mut args = vec![
        "ls-tree",
        "-z",
        "--full-tree",
        "--format=%(objectmode) %(objectname)%x09%(path)",
        commit,
    ];
    if let Some(inside) = &inside {
        args.extend(["--", inside.as_str()]);
    }
    let output = repo.exec(&args, None, &[], LISTING_BYTES_MAX)?;
    if output.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    let mut listed: Vec<&[u8]> = output.stdout.split(|byte| *byte == 0).collect();
    listed.pop();
    Ok(listed
        .into_iter()
        .filter_map(|entry| std::str::from_utf8(entry).ok())
        .filter_map(|entry| {
            let (fields, path) = entry.split_once('\t')?;
            let (mode, object) = fields.split_once(' ')?;
            Some(Entry {
                mode: mode.to_owned(),
                object: object.to_owned(),
                path: path.to_owned(),
            })
        })
        .collect())
}

/// Reads a blob's text up to the instruction file bound, with invalid UTF-8 replaced.
/// Inputs: the repository and the blob id. Output: the text.
fn blob_text(repo: &Repo, object: &str) -> Result<String, EngineError> {
    if !is_object_id(object) {
        return Err(failure("changes_git_failed"));
    }
    let output = repo.exec(
        &["cat-file", "blob", object],
        None,
        &[],
        crate::instruction_files::FILE_BYTES_MAX,
    )?;
    if output.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The project paths whose text an instruction file of one commit feeds to agents besides
/// its own: what a root instruction file, or one in a root `.claude` or `.codex` folder,
/// imports with `@` or links to, and the folder a root `.claude` or `.codex` link points at,
/// one level deep. A source already read at the same path and object is read once.
/// Inputs: the repository, the commit and the sources read so far. Output: each path with
/// whether it is a folder.
fn fed_paths(
    repo: &Repo,
    commit: &str,
    read: &mut Vec<(String, String)>,
) -> Result<Vec<(String, bool)>, EngineError> {
    use crate::instruction_files::{harness_folder, imports, instruction_name, resolved};
    let mut sources = Vec::new();
    let mut feeds = Vec::new();
    for entry in entries(repo, commit, None)? {
        if harness_folder(&entry.path) {
            match entry.mode.as_str() {
                "040000" => sources.extend(
                    entries(repo, commit, Some(entry.path.as_str()))?
                        .into_iter()
                        .filter(|child| {
                            instruction_name(child.path.rsplit('/').next().unwrap_or_default())
                        }),
                ),
                "120000" => feeds.extend(
                    resolved(&entry.path, &blob_text(repo, &entry.object)?)
                        .map(|folder| (folder, true)),
                ),
                _ => {}
            }
        } else if instruction_name(&entry.path) {
            sources.push(entry);
        }
    }
    for source in sources.into_iter().take(FED_SOURCES_MAX) {
        let key = (source.path.clone(), source.object.clone());
        if read.contains(&key) {
            continue;
        }
        read.push(key);
        match source.mode.as_str() {
            "120000" => feeds.extend(
                resolved(&source.path, &blob_text(repo, &source.object)?).map(|file| (file, false)),
            ),
            "100644" | "100755" => feeds.extend(
                imports(&source.path, &blob_text(repo, &source.object)?)
                    .into_iter()
                    .map(|file| (file, false)),
            ),
            _ => {}
        }
    }
    Ok(feeds)
}

/// Lists the files a change touches with their status, kind and the marks the owner
/// should look at. A file is marked as an instruction file by its own name or folder, or
/// when an instruction file of either commit imports it or links to it.
/// Inputs: the repository and the two commits. Output: the files.
pub(super) fn files(repo: &Repo, base: &str, head: &str) -> Result<Vec<ChangedFile>, EngineError> {
    let counts = numstat(repo, base, head)?;
    let mut read = Vec::new();
    let mut feeds = fed_paths(repo, base, &mut read)?;
    feeds.extend(fed_paths(repo, head, &mut read)?);
    let instruction = |path: &str| {
        crate::instruction_files::instruction_file(path)
            || crate::instruction_files::fed(&feeds, path)
    };
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
        if instruction(path) || old_path.is_some_and(instruction) {
            marks.push(FileMark::Instructions);
        }
        if old_mode != "000000" && new_mode != "000000" && old_mode != new_mode {
            marks.push(FileMark::Mode);
        }
        if agent_config::is_ide_config(path) || old_path.is_some_and(agent_config::is_ide_config) {
            marks.push(FileMark::IdeConfig);
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
    if lasting && let Some(found) = recall(&RECENT, &key) {
        return Ok(found);
    }
    let fresh = Arc::new(compute(&super::flow::shared(repository)?, base, head)?);
    if lasting {
        keep(
            &RECENT,
            key,
            &fresh,
            RECENT_MAX,
            RECENT_BYTES_MAX,
            Computed::bytes,
        );
    }
    Ok(fresh)
}

/// Lists every file a change touches, from the recent lists, or from its diff once and
/// kept when both sides are commit ids, whose files never change. Inputs: the repository
/// and the two commits. Output: the files in path order.
pub(super) fn listed(
    repository: &Path,
    base: &str,
    head: &str,
) -> Result<Arc<[ChangedFile]>, EngineError> {
    let key = (repository.to_path_buf(), base.to_owned(), head.to_owned());
    let lasting = is_object_id(base) && is_object_id(head);
    if lasting && let Some(found) = recall(&LISTS, &key) {
        return Ok(found);
    }
    let fresh: Arc<[ChangedFile]> = computed(repository, base, head)?.files.clone().into();
    if lasting {
        keep(&LISTS, key, &fresh, LISTS_MAX, LISTS_BYTES_MAX, path_bytes);
    }
    Ok(fresh)
}

/// The bytes the paths of a file list hold. Input: the files. Output: the bytes.
fn path_bytes(files: &[ChangedFile]) -> usize {
    files
        .iter()
        .map(|file| file.path.len() + file.old_path.as_ref().map_or(0, String::len))
        .sum()
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
