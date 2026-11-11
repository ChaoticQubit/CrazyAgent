use super::names::{paths_in, secret_like, source_file};
use super::{
    Document, ENTRY_NAME_BYTES_MAX, Evidence, Folder, FoundKind, SKIPPED_ENTRIES, clip, file_text,
    open_folder, open_in,
};
use crate::onboarding::FoundResearch;
use crate::project_setup::{DOCUMENT_PATH_BYTES_MAX, ResearchKind};
use crate::projects_home::CRAZYAGENT_DIRECTORY;
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, Stat};
use std::collections::BTreeSet;
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
/// Hidden tool and dependency folders, and Python's package folder, whose Markdown belongs to
/// other projects: walked, their changelogs and readme files would rank as this project's
/// roadmap or research.
const DEPENDENCY_FOLDERS: [&str; 13] = [
    ".venv",
    ".tox",
    ".nox",
    ".cargo",
    ".terraform",
    ".yarn",
    ".pnpm-store",
    ".gradle",
    ".direnv",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    "site-packages",
];
/// The file that marks a Python virtual environment under any folder name.
const PYTHON_ENVIRONMENT_MARK: &str = "pyvenv.cfg";
const DEPTH_MAX: usize = 8;
const ENTRIES_MAX: usize = 6_000;
const FILE_BYTES_MAX: u64 = 1024 * 1024;
const TEXT_BYTES_MAX: usize = 6 * 1024;
const TOTAL_TEXT_BYTES_MAX: usize = 48 * 1024;
const READ_MAX: usize = 40;
const UNREAD_MAX: usize = 64;
const RESEARCH_FILES_MAX: usize = 8;
const IGNORE_CHECK_TIME: Duration = Duration::from_secs(20);
const IGNORE_POLL: Duration = Duration::from_millis(5);
const IGNORE_OUTPUT_BYTES_MAX: usize = 8 * 1024 * 1024;
const WALKED_ENTRIES_MAX: usize = 100_000;
const WALKED_DEPTH_MAX: usize = 64;
const GITLINK_MODE: &str = "160000 ";
/// Entries inside hidden folders count here too, so a large `.venv` or `.agent` never
/// starves the visible documents (DEC-132 Q3).
const HIDDEN_ENTRIES_MAX: usize = 2_000;
const _: () = assert!(HIDDEN_ENTRIES_MAX < ENTRIES_MAX);
/// The only git-ignored files the walk keeps: Markdown, as tracked (DEC-132 addendum).
const MARKDOWN_EXTENSIONS: [&str; 3] = ["md", "markdown", "mdx"];
const HEADING_SCAN_BYTES_MAX: usize = 256 * 1024;
const HEADING_SCAN_TOTAL_BYTES_MAX: usize = 32 * 1024 * 1024;
const MILESTONES_COUNTED_MAX: usize = 16;
const ROADMAP_SCORE_MIN: u32 = 12;
const RESEARCH_SCORE_MIN: u32 = 6;
const SCORE_MAX: u32 = 256;
const MARKS_MAX: usize = 12;
const MARK_BYTES_MAX: usize = 120;
const MILESTONE_POINTS: u32 = 6;
const HEADING_POINTS: u32 = 3;
const LINKS_PER_POINT: u32 = 8;
const LINK_POINTS_MAX: u32 = 16;
const ROADMAP_WORDS: [&str; 6] = [
    "roadmap",
    "milestone",
    "sequencing",
    "delivery plan",
    "release plan",
    "phase",
];
const RESEARCH_WORDS: [&str; 9] = [
    "competitor",
    "market",
    "comparison",
    "research",
    "references",
    "sources",
    "interview",
    "persona",
    "survey",
];
const MILESTONE_WORDS: [&str; 3] = ["Phase", "Milestone", "Release"];
const CHECKBOXES: [&str; 6] = ["- [ ]", "- [x]", "- [X]", "* [ ]", "* [x]", "* [X]"];
const IGNORES_UNKNOWN: &str =
    "No project document was read: the project's git could not tell which files it ignores.";

/// What the local heading scan says of one document: its kind, the kind's score and, for a
/// roadmap, its marks.
type Heading = (FoundKind, u32, Vec<String>);

/// What one document walk gives the prompt: the documents read and the paths and sizes of
/// those only listed.
type Reading = (Vec<Document>, Vec<(String, u64)>);

/// One file the walk found below the project folder: its path with `/` between names, its
/// size and whether its text is read or only its path and size are listed.
struct Found {
    path: String,
    size: u64,
    text: bool,
}

/// What one document walk gathers: the entries visited in all, those visited inside hidden
/// folders, and the files found.
#[derive(Default)]
struct Walk {
    visited: usize,
    hidden_visited: usize,
    found: Vec<Found>,
}

/// Reads the project's own documents beneath `repo`, never through a link: Markdown and plain
/// text files up to eight folders deep, hidden folders included but walked after the visible
/// ones and within their own entry bound. A local heading scan of each text first puts
/// roadmaps, then research, then the rest; within each, the docs, doc, documentation, research
/// and notes folders first, then root files, then the rest, shallower first. Hidden files,
/// `.git`, the engine's own `.crazyagent` folder, names that look like secrets, private, build,
/// vendor and dependency folders, Python environments, other mounts, hard-linked files, files
/// over 1 MiB and whatever the project's git ignores, except Markdown, are never opened or
/// listed. Each text is cut at 6 KiB, all of them at 48 KiB and 40 files; files past that and
/// PDF, Word and CSV files are only listed. Takes the folder and the README name already read.
/// Returns the documents read and the paths and sizes listed, or `None` when the project's git
/// could not say which files it ignores, so none is read. Files in the `hidden` folders, given
/// below the project folder, are never read or listed.
pub(super) fn read(repo: &Path, readme: Option<&str>, hidden: &[String]) -> Option<Reading> {
    let Some((root, device)) = open_folder(repo)
        .and_then(|root| rustix::fs::fstat(&root).ok().map(|device| (root, device)))
    else {
        return Some((Vec::new(), Vec::new()));
    };
    let mut gathered = Walk::default();
    walk(&root, &device, "", 0, false, &mut gathered);
    assert!(gathered.visited <= ENTRIES_MAX && gathered.hidden_visited <= HIDDEN_ENTRIES_MAX);
    let mut found = gathered.found;
    found.retain(|file| Some(file.path.as_str()) != readme && !under(&file.path, hidden));
    let paths: Vec<&str> = found.iter().map(|file| file.path.as_str()).collect();
    let ignored = ignored(repo, &paths)?;
    assert_eq!(ignored.len(), found.len());
    let mut found: Vec<Found> = found
        .into_iter()
        .zip(ignored)
        .filter_map(|(file, ignored)| (!ignored || kept_ignored(&file.path)).then_some(file))
        .collect();
    found.sort_by(|left, right| rank(&left.path).cmp(&rank(&right.path)));
    let mut scan_budget = HEADING_SCAN_TOTAL_BYTES_MAX;
    let mut scanned: Vec<(Found, Heading)> = found
        .into_iter()
        .map(|file| {
            let max = HEADING_SCAN_BYTES_MAX.min(scan_budget);
            let text = (file.text && max > 0)
                .then(|| text_beneath(&root, &file.path, max))
                .flatten();
            let kind = text.map_or((FoundKind::Other, 0, Vec::new()), |text| {
                scan_budget -= text.len();
                heading_kind(&text)
            });
            (file, kind)
        })
        .collect();
    scanned.sort_by_key(|(_, (kind, score, _))| (*kind, std::cmp::Reverse(*score)));
    let (mut documents, mut unread) = (Vec::new(), Vec::new());
    let mut budget = TOTAL_TEXT_BYTES_MAX;
    for (file, (kind, _, marks)) in scanned {
        if file.text && documents.len() < READ_MAX && budget > 0 {
            let max = TEXT_BYTES_MAX.min(budget);
            if let Some(text) = text_beneath(&root, &file.path, max) {
                budget -= text.len();
                documents.push(Document {
                    cut: file.size > max as u64,
                    path: file.path,
                    size: file.size,
                    text,
                    kind,
                    marks,
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

/// Drops each file name that looks like it holds a secret or that git ignores from the
/// mapping evidence (MAP-2): such top-level entries and each README, manifest, overview and
/// document line or heading scan mark naming one, as a mark may come from past the text
/// shown, and a code graph line also on any secret word, as its nodes may be bare names.
/// One `check-ignore` call answers for all; a name git does not clear, or every
/// name when git could not answer, counts as ignored, and so does a name in one of the
/// `hidden` folders, given below the project folder, written relative or absolute. Takes
/// the folder, the evidence and the hidden folders.
pub(super) fn screen(repo: &Path, evidence: &mut Evidence, hidden: &[String]) {
    let mut names: BTreeSet<String> = evidence.entries.iter().cloned().collect();
    let texts = evidence
        .readme
        .iter()
        .map(|(_, text)| text)
        .chain(evidence.manifests.iter().map(|(_, text)| text))
        .chain(&evidence.graph)
        .chain(&evidence.overview)
        .chain(evidence.documents.iter().map(|document| &document.text))
        .chain(
            evidence
                .documents
                .iter()
                .flat_map(|document| document.marks.iter()),
        );
    for text in texts {
        names.extend(paths_in(text).filter(|path| below(path)).map(str::to_owned));
    }
    let asked: Vec<&str> = names.iter().map(String::as_str).collect();
    let checked = (!evidence.ignores_unknown)
        .then(|| ignored(repo, &asked))
        .flatten();
    let cleared: BTreeSet<&str> = checked.map_or_else(BTreeSet::new, |flags| {
        asked
            .iter()
            .zip(flags)
            .filter_map(|(path, ignored)| {
                ((!ignored || kept_ignored(path)) && !under(path, hidden)).then_some(*path)
            })
            .collect()
    });
    let root = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let hidden_absolute = |path: &str| {
        [root.as_path(), repo].iter().any(|base| {
            Path::new(path)
                .strip_prefix(base)
                .ok()
                .and_then(Path::to_str)
                .is_some_and(|below| under(below, hidden))
        })
    };
    let shown = |line: &str| {
        paths_in(line).all(|path| {
            !secret_like(path)
                && ((path.starts_with('/') && !hidden_absolute(path))
                    || path.starts_with('~')
                    || cleared.contains(path))
        })
    };
    let keep = |text: &mut String, strict: bool| {
        *text = text
            .split_inclusive('\n')
            .filter(|line| !(strict && secret_like(line)) && shown(line))
            .collect();
    };
    evidence
        .entries
        .retain(|entry| !secret_like(entry) && cleared.contains(entry.as_str()));
    if let Some((_, text)) = &mut evidence.readme {
        keep(text, false);
    }
    for (_, text) in &mut evidence.manifests {
        keep(text, false);
    }
    if let Some(text) = &mut evidence.overview {
        keep(text, false);
    }
    for document in &mut evidence.documents {
        keep(&mut document.text, false);
        document.marks.retain(|mark| shown(mark));
    }
    if let Some(text) = &mut evidence.graph {
        keep(text, true);
    }
}

/// The project's `hidden` folders as paths below `repo`, with `/` between names, for the
/// mapping to leave out. Takes the folder and the hidden folders. Returns the paths, or
/// `None` when one is not below the folder or not UTF-8, so nothing is cleared.
pub(super) fn below_root(repo: &Path, hidden: &[std::path::PathBuf]) -> Option<Vec<String>> {
    let root = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    hidden
        .iter()
        .map(|folder| {
            let below = folder
                .strip_prefix(&root)
                .or_else(|_| folder.strip_prefix(repo))
                .ok()?;
            let text = below.to_str()?;
            (!text.is_empty()).then(|| text.to_owned())
        })
        .collect()
}

/// Whether `path`, below the project folder and with an optional closing `/`, is one of the
/// `hidden` folders or lies inside one. Takes the path and the folders. Returns the answer.
fn under(path: &str, hidden: &[String]) -> bool {
    let path = path.trim_end_matches('/');
    hidden.iter().any(|folder| {
        path.strip_prefix(folder.as_str())
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// Whether `path` may name a file below the project folder for git to check: not from the
/// root or a home folder, within the path bound, without control characters, and with no
/// empty, `.` or `..` name before an optional closing `/`. Takes the path. Returns the answer.
fn below(path: &str) -> bool {
    !path.starts_with('/')
        && !path.starts_with('~')
        && path.len() <= DOCUMENT_PATH_BYTES_MAX
        && !path.chars().any(char::is_control)
        && path
            .strip_suffix('/')
            .unwrap_or(path)
            .split('/')
            .all(|name| !matches!(name, "" | "." | ".."))
}

/// Walks `folder`, found at `at` below the project folder and `depth` folders deep, adding
/// each document file it may read or list to `walk_so_far`. Entries are taken in name order, at
/// most 6,000 in all and 2,000 inside hidden folders, statted without following links;
/// visible folders go before hidden ones, and a root's document folders first. A hidden
/// file is skipped, and so are `.git`, the engine's `.crazyagent` folder, which the
/// mapping reads its overview from on its own, dependency folders, a hidden folder once the
/// hidden bound is spent, a file with more than one link, since its other name may be
/// outside the project, a folder on another device than the root, such as a mount, a
/// folder holding a `.git` entry, a nested repository or submodule whose own ignores the
/// project's git does not apply, and a folder holding `pyvenv.cfg`, a Python environment.
/// Takes the open folder, the root's status, its path, its depth, whether it is a hidden
/// folder or inside one, and what the walk gathered so far.
fn walk(
    folder: &Folder,
    root: &Stat,
    at: &str,
    depth: usize,
    in_hidden: bool,
    walk_so_far: &mut Walk,
) {
    let Ok(entries) = Dir::read_from(folder) else {
        return;
    };
    let mut names = Vec::new();
    for entry in entries {
        if walk_so_far.visited == ENTRIES_MAX
            || (in_hidden && walk_so_far.hidden_visited == HIDDEN_ENTRIES_MAX)
        {
            break;
        }
        walk_so_far.visited += 1;
        if in_hidden {
            walk_so_far.hidden_visited += 1;
        }
        let Ok(entry) = entry else {
            break;
        };
        let Ok(name) = entry.file_name().to_str() else {
            continue;
        };
        if name != "."
            && name != ".."
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
                if depth < DEPTH_MAX
                    && !named(&UNREAD_FOLDERS, &name)
                    && !named(&DEPENDENCY_FOLDERS, &name)
                    && !name.eq_ignore_ascii_case(".git")
                    && !name.eq_ignore_ascii_case(CRAZYAGENT_DIRECTORY)
                {
                    folders.push((name, path));
                }
            }
            FileType::RegularFile if !name.starts_with('.') => {
                let size = u64::try_from(stat.st_size).unwrap_or(0);
                let text = extension_in(&name, &TEXT_EXTENSIONS);
                if (text || extension_in(&name, &LISTED_EXTENSIONS))
                    && size > 0
                    && size <= FILE_BYTES_MAX
                    && stat.st_nlink == 1
                {
                    walk_so_far.found.push(Found { path, size, text });
                }
            }
            _ => {}
        }
    }
    folders.sort_by_key(|(name, _)| {
        (
            name.starts_with('.'),
            depth == 0 && !named(&DOCUMENT_FOLDERS, name),
        )
    });
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    for (name, path) in folders {
        if walk_so_far.visited == ENTRIES_MAX {
            return;
        }
        let hidden = in_hidden || name.starts_with('.');
        if hidden && walk_so_far.hidden_visited == HIDDEN_ENTRIES_MAX {
            continue;
        }
        let Ok(child) = rustix::fs::openat(folder, name.as_str(), flags, Mode::empty()) else {
            continue;
        };
        let absent = |name: &str| {
            matches!(
                rustix::fs::statat(&child, name, AtFlags::SYMLINK_NOFOLLOW),
                Err(rustix::io::Errno::NOENT)
            )
        };
        if rustix::fs::fstat(&child).is_ok_and(|stat| stat.st_dev == root.st_dev)
            && absent(".git")
            && absent(PYTHON_ENVIRONMENT_MARK)
        {
            walk(&child, root, &path, depth + 1, hidden, walk_so_far);
        }
    }
}

/// Whether a file git ignores is still read, as tracked (DEC-132 addendum): its last name
/// has a Markdown extension and no name on its path looks like it holds a secret. Takes the
/// path below the project folder. Returns the answer.
fn kept_ignored(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    extension_in(name, &MARKDOWN_EXTENSIONS) && !path.split('/').any(secret_like)
}

/// Whether `name` is one of `list`, ignoring ASCII case.
fn named(list: &[&str], name: &str) -> bool {
    list.iter().any(|known| known.eq_ignore_ascii_case(name))
}

/// Asks the project's own git, in one `check-ignore` call, which of `paths` it ignores by
/// the repository's ignore files, info/exclude and configured excludes file: the real git
/// with no system or global config, hooks, fsmonitor or pager, given 20 s and 8 MiB of
/// output. Each name goes in behind `./`, so one starting with `:` is never pathspec magic.
/// Git must name the ignored paths exactly as sent and in order; any other answer counts as
/// none, so the mapping never guesses which files git keeps out. A folder outside git
/// ignores nothing, as a repository without ignore files. Takes the project folder and the
/// paths below it. Returns whether each path is ignored, or `None` when git is missing,
/// fails, runs over or answers out of shape.
fn ignored(repo: &Path, paths: &[&str]) -> Option<Vec<bool>> {
    if paths.is_empty() {
        return Some(Vec::new());
    }
    if outside_git(repo) {
        return Some(vec![false; paths.len()]);
    }
    let sent: Vec<String> = paths.iter().map(|path| format!("./{path}")).collect();
    let input: Vec<u8> = sent
        .iter()
        .flat_map(|path| path.bytes().chain([0]))
        .collect();
    let mut command = git(repo, &["check-ignore", "-z", "--stdin", "--no-index"]);
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

/// The project's own git run in `repo` with `args`: the real git with no system or global
/// config, hooks, fsmonitor or pager, no prompt and the C locale, its input and output piped
/// and its errors dropped. Takes the folder and the arguments. Returns the command.
fn git(repo: &Path, args: &[&str]) -> Command {
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
        .args(args)
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

/// Whether `repo` is outside every git repository: neither it nor any folder above it holds
/// a `.git` entry, so no ignore rule applies and it ignores nothing, as a repository without
/// ignore files. A `.git` that cannot be checked counts as there. Takes the folder. Returns
/// the answer.
pub(crate) fn outside_git(repo: &Path) -> bool {
    repo.is_absolute()
        && repo.ancestors().all(|folder| {
            std::fs::symlink_metadata(folder.join(".git"))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        })
}

/// The paths below `repo` the code graph must never read (MAP-2), by the rules documents
/// follow: whatever the project's git ignores, tracked or not, an ignored folder once with
/// a closing `/`, each submodule and nested repository whole with a closing `/`, as their
/// own ignores are not the project's, and the outermost file or folder of each other path
/// whose name is secret-like, but for a programming source file, which a word in its name
/// alone does not keep out. Asks git three times, bounded as [`ignored`]; a folder outside
/// git is walked instead. Takes the folder. Returns the paths, or `None` when git could not
/// answer, the walk could not finish or a name is not UTF-8, so no graph is built.
pub(crate) fn kept_out(repo: &Path) -> Option<Vec<String>> {
    if outside_git(repo) {
        return walked(repo);
    }
    let ignored = listed(
        repo,
        &["-c", "-o", "-i", "--exclude-standard", "--directory"],
    )?;
    let mut kept: BTreeSet<String> = ignored.into_iter().collect();
    let mut visible = listed(repo, &["-o", "--exclude-standard"])?;
    for record in listed(repo, &["-s"])? {
        let (mode, path) = record.split_once('\t')?;
        if mode.starts_with(GITLINK_MODE) {
            kept.insert(format!("{path}/"));
        } else {
            visible.push(path.to_owned());
        }
    }
    for path in visible {
        if path.ends_with('/') {
            kept.insert(path);
            continue;
        }
        let mut end = 0;
        for name in path.split('/') {
            end += name.len();
            let folder = end < path.len();
            if secret_like(name) && (folder || !source_file(name)) {
                kept.insert(format!("{}{}", &path[..end], if folder { "/" } else { "" }));
                break;
            }
            end += 1;
        }
    }
    Some(kept.into_iter().collect())
}

/// The paths below `repo`, a folder outside git, the code graph must never read (MAP-2): the
/// outermost file or folder of each path whose name is secret-like, but for a programming
/// source file, and each folder holding a `.git` entry, whole, each folder with a closing
/// `/`. The walk follows no link and skips
/// the root's engine and graph folders. Takes the folder. Returns the paths, or `None` past
/// 100,000 entries or 64 folders deep, or when a folder cannot be read or a name is not UTF-8.
fn walked(repo: &Path) -> Option<Vec<String>> {
    let mut kept = BTreeSet::new();
    let mut folders = vec![(repo.to_path_buf(), String::new(), 0)];
    let mut visited = 0;
    while let Some((folder, at, depth)) = folders.pop() {
        for entry in std::fs::read_dir(&folder).ok()? {
            visited += 1;
            if visited > WALKED_ENTRIES_MAX {
                return None;
            }
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            if depth == 0 && SKIPPED_ENTRIES.contains(&name.as_str()) {
                continue;
            }
            let path = if at.is_empty() {
                name.clone()
            } else {
                format!("{at}/{name}")
            };
            if !entry.file_type().ok()?.is_dir() {
                if secret_like(&name) && !source_file(&name) {
                    kept.insert(path);
                }
                continue;
            }
            let nested = !std::fs::symlink_metadata(entry.path().join(".git"))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound);
            if secret_like(&name) || nested {
                kept.insert(format!("{path}/"));
            } else if depth + 1 < WALKED_DEPTH_MAX {
                folders.push((entry.path(), path, depth + 1));
            } else {
                return None;
            }
        }
    }
    Some(kept.into_iter().collect())
}

/// The paths `git ls-files -z` with `args` names in `repo`, within the bounds of
/// [`answer`]. Takes the folder and the arguments. Returns the paths, or `None` when git
/// fails, runs over, answers out of shape or names a path that is not UTF-8.
fn listed(repo: &Path, args: &[&str]) -> Option<Vec<String>> {
    let mut command = git(repo, &["ls-files", "-z"]);
    command.args(args);
    let (code, output) = answer(&mut command, Vec::new())?;
    let body = match output.split_last() {
        _ if code != 0 => return None,
        None => return Some(Vec::new()),
        Some((0, body)) => body,
        Some(_) => return None,
    };
    body.split(|byte| *byte == 0)
        .map(|name| String::from_utf8(name.to_vec()).ok())
        .collect()
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
/// size in a fence longer than any backtick run in its text, the local heading scan's
/// roadmap or research line with its marks first inside the fence, as the marks are file
/// text, then the paths and sizes of the
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
        let scan = match document.kind {
            FoundKind::Roadmap if document.marks.is_empty() => "Local scan: roadmap\n".to_owned(),
            FoundKind::Roadmap => format!("Local scan: roadmap ({})\n", document.marks.join("; ")),
            FoundKind::Research => "Local scan: research\n".to_owned(),
            FoundKind::Other => String::new(),
        };
        let fence = "`".repeat(longest_run(&format!("{scan}{}", document.text)).max(2) + 1);
        text.push_str(&format!(
            "### {} ({} bytes{shown})\n\n{fence}\n{scan}{}\n{fence}\n\n",
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
/// at most `max`, none empty, past the path bound, holding control characters or looking
/// like it holds a secret, so a guessed secret name is never kept. Takes the JSON value and
/// the bound. Returns the paths.
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
            && !secret_like(path)
            && !paths.iter().any(|known| known == path)
        {
            paths.push(path.to_owned());
        }
    }
    assert!(paths.len() <= max);
    paths
}

/// What a document's headings, list items, table rows and bold-led lines outside code
/// fences say it is. Each distinct milestone label (`M0`, `V2`, `Phase 1`, `Milestone 3`,
/// `Release 4`), at most 16, adds 6 to roadmap and becomes a mark; a heading naming a
/// roadmap word adds 3 to roadmap and one naming a research word 3 to research; every 8
/// links add 1 to research, at most 16; every 2 checkboxes take 1 from roadmap, as a ticket
/// plan is not a roadmap. Roadmap at 12 or more and no less than research, else research at
/// 6 or more, else other. Takes the text. Returns the kind, its score up to 256, and for a
/// roadmap at most 12 marks of at most 120 bytes, the most used label family first.
pub(super) fn heading_kind(text: &str) -> Heading {
    let (mut roadmap, mut research, mut links, mut checkboxes) = (0u32, 0u32, 0u32, 0u32);
    let mut labels: Vec<String> = Vec::new();
    let mut marks: Vec<(&'static str, String)> = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        let line = line.trim_start();
        if line.starts_with("```") || line.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        links =
            links.saturating_add(u32::try_from(line.matches("](http").count()).unwrap_or(u32::MAX));
        if CHECKBOXES.iter().any(|checkbox| line.starts_with(checkbox)) {
            checkboxes += 1;
        }
        let heading = line.starts_with('#');
        if !(heading || line.starts_with(['-', '*', '+', '|']) || ordered_item(line)) {
            continue;
        }
        let body = without_number(line.trim_start_matches(['#', '-', '*', '+', '|', ' ']));
        if heading {
            let lower = body.to_lowercase();
            if ROADMAP_WORDS.iter().any(|word| lower.contains(word)) {
                roadmap = roadmap.saturating_add(HEADING_POINTS);
            }
            if RESEARCH_WORDS.iter().any(|word| lower.contains(word)) {
                research = research.saturating_add(HEADING_POINTS);
            }
        }
        if let Some((family, label)) = milestone(body)
            && labels.len() < MILESTONES_COUNTED_MAX
            && !labels
                .iter()
                .any(|known| known.eq_ignore_ascii_case(&label))
        {
            labels.push(label);
            roadmap = roadmap.saturating_add(MILESTONE_POINTS);
            let mark = clip(body.trim_end_matches(['*', '|', '#', ' ']), MARK_BYTES_MAX);
            marks.push((family, mark.trim_end().to_owned()));
        }
    }
    research = research.saturating_add((links / LINKS_PER_POINT).min(LINK_POINTS_MAX));
    let roadmap = roadmap.saturating_sub(checkboxes / 2).min(SCORE_MAX);
    let research = research.min(SCORE_MAX);
    let families: Vec<&str> = marks.iter().map(|(family, _)| *family).collect();
    marks.sort_by_key(|(family, _)| {
        std::cmp::Reverse(families.iter().filter(|known| **known == *family).count())
    });
    let found = if roadmap >= ROADMAP_SCORE_MIN && roadmap >= research {
        let marks: Vec<String> = marks
            .into_iter()
            .take(MARKS_MAX)
            .map(|(_, mark)| mark)
            .collect();
        (FoundKind::Roadmap, roadmap, marks)
    } else if research >= RESEARCH_SCORE_MIN {
        (FoundKind::Research, research, Vec::new())
    } else {
        (FoundKind::Other, 0, Vec::new())
    };
    assert!(found.1 <= SCORE_MAX && found.2.len() <= MARKS_MAX);
    found
}

/// Whether `line` starts an ordered list item: digits, then `.` or `)`, then a space.
/// Takes the line. Returns the answer.
fn ordered_item(line: &str) -> bool {
    let rest = line.trim_start_matches(|character: char| character.is_ascii_digit());
    rest.len() < line.len() && (rest.starts_with(". ") || rest.starts_with(") "))
}

/// `line` without a leading section number such as `7.` or `0.0` and the spaces after it.
/// Takes the line. Returns the rest, or the line itself when it has no such number.
fn without_number(line: &str) -> &str {
    let rest =
        line.trim_start_matches(|character: char| character.is_ascii_digit() || character == '.');
    let numbered = rest.len() < line.len()
        && line.starts_with(|character: char| character.is_ascii_digit())
        && rest.starts_with([' ', ')']);
    if numbered {
        rest.trim_start_matches([' ', ')'])
    } else {
        line
    }
}

/// The milestone label `line` starts with, and its family: `M`, `V` or `v` and digits, or
/// `Phase`, `Milestone` or `Release` (any case), a space and digits, each followed by the
/// end, a space, `:` or `.`. A `V` or `Release` number followed by `.` and a digit is a
/// version number, such as a changelog's `v3.0.0`, not a milestone. Takes the line. Returns
/// the family and the label, or none.
fn milestone(line: &str) -> Option<(&'static str, String)> {
    let numbered = |rest: &str, versioned: bool| {
        let digits = rest.len()
            - rest
                .trim_start_matches(|character: char| character.is_ascii_digit())
                .len();
        let after = &rest[digits..];
        let version = versioned
            && after
                .strip_prefix('.')
                .is_some_and(|tail| tail.starts_with(|character: char| character.is_ascii_digit()));
        (digits > 0 && !version && (after.is_empty() || after.starts_with([' ', ':', '.'])))
            .then_some(digits)
    };
    let letter = match line.chars().next() {
        Some('M') => Some("M"),
        Some('V' | 'v') => Some("V"),
        _ => None,
    };
    if let Some(family) = letter
        && let Some(digits) = numbered(&line[1..], family == "V")
    {
        return Some((family, format!("{family}{}", &line[1..1 + digits])));
    }
    MILESTONE_WORDS.into_iter().find_map(|word| {
        let head = line.get(..word.len())?;
        let rest = line[word.len()..].strip_prefix(' ')?;
        head.eq_ignore_ascii_case(word)
            .then(|| numbered(rest, word == "Release"))
            .flatten()
            .map(|digits| (word, format!("{word} {}", &rest[..digits])))
    })
}

#[cfg(test)]
mod tests;
