use crate::engine::EngineError;
use std::path::Path;
use std::time::{Duration, Instant};

/// The most paths one file-list read returns.
pub const FILES_MAX: u16 = 200;
/// The longest file-list query, in bytes.
pub const QUERY_BYTES_MAX: usize = 256;
/// The longest a file-list read lets git run before it answers with what it has.
pub const LISTING_TIME: Duration = Duration::from_secs(5);
const PATH_BYTES_MAX: usize = 512;
const REPLY_BYTES_MAX: usize = 48 * 1024;
const LISTING_BYTES_MAX: u64 = 32 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(2);
const CONFIG_BYTES_MAX: u64 = 1024 * 1024;
/// The most entries [`redirected`] looks through in one `.git` folder before it refuses it.
const GIT_ENTRIES_MAX: usize = 100_000;
/// What [`list`] asks git in owner git's container for, and git on Unix: tracked and
/// unignored untracked files.
const LISTED_ALL: &[&str] = &[
    "ls-files",
    "-z",
    "--cached",
    "--others",
    "--exclude-standard",
];
/// What [`list`] asks git as the owner for: [`LISTED_ALL`] on Unix, tracked files only on
/// Windows (the declined path), so owner git walks no folder an agent made there.
const LISTED: &[&str] = if cfg!(windows) {
    &["ls-files", "-z", "--cached"]
} else {
    LISTED_ALL
};

fn failure(code: &str) -> EngineError {
    EngineError(code.into())
}

/// Checks one file-list request before the engine runs it. Inputs: the query and the
/// limit. Output: unit, `project_files_query_invalid` for a query over 256 bytes or with a
/// control character, or `project_files_limit_invalid` for a limit outside 1 to 200.
pub(crate) fn validate(query: &str, limit: u16) -> Result<(), EngineError> {
    if query.len() > QUERY_BYTES_MAX || query.chars().any(char::is_control) {
        return Err(failure("project_files_query_invalid"));
    }
    if limit == 0 || limit > FILES_MAX {
        return Err(failure("project_files_limit_invalid"));
    }
    Ok(())
}

/// Lists, for `@` tags through [`owner_git`], files whose path holds the query in any case:
/// [`LISTED_ALL`] on Unix and in owner git's container, [`LISTED`] for Windows owner git (the
/// declined path); never `.git`, a link or a file through a link out of the folder. Inputs:
/// folder, query, limit, stop, owner git's container or none. Output: paths by the limit, 48 KiB
/// or `stop` (the `.git` walk first is bounded by entries, not time), or a `project_files_*` code.
pub(crate) fn list(
    folder: &Path,
    query: &str,
    limit: u16,
    stop: Instant,
    contained: Option<&kernel_sandbox::windows::owner_git::OwnerGit>,
) -> Result<Vec<String>, EngineError> {
    use std::process::Stdio;
    validate(query, limit)?;
    if !folder.is_absolute() {
        return Err(failure("project_files_folder_missing"));
    }
    let root = kernel_sandbox::paths::canonical(folder)
        .ok()
        .filter(|root| root.is_dir())
        .ok_or_else(|| failure("project_files_folder_missing"))?;
    let git = match contained {
        Some(owner) => Some(owner.git().to_path_buf()),
        None => engine_git(),
    };
    let mut command = owner_git(&root, git).map_err(|missing| match missing {
        NoOwnerGit::Unavailable => failure("project_files_git_failed"),
        NoOwnerGit::Absent | NoOwnerGit::Refused => failure("project_files_no_repository"),
    })?;
    command.args(if contained.is_some() {
        LISTED_ALL
    } else {
        LISTED
    });
    let git_failed = |_| failure("project_files_git_failed");
    let mut wrapped;
    let command = match contained {
        None => &mut command,
        Some(owner) => {
            wrapped = owner
                .wrap(&command)
                .map_err(|_| failure("project_files_git_failed"))?;
            &mut wrapped
        }
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = kernel_processes::spawn_gate::spawn(command).map_err(git_failed)?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| failure("project_files_git_failed"))?;
    let wanted = query.to_lowercase();
    let limit = usize::from(limit);
    let reader = std::thread::spawn(move || collect(output, &root, &wanted, limit));
    let exited = loop {
        if let Some(status) = child.try_wait().map_err(git_failed)? {
            break Some(status);
        }
        if reader.is_finished() || Instant::now() >= stop {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(POLL);
    };
    let (paths, full) = reader
        .join()
        .map_err(|_| failure("project_files_git_failed"))?;
    assert!(paths.len() <= limit);
    match exited {
        Some(status) if !status.success() && !full => Err(failure("project_files_git_failed")),
        _ => Ok(paths),
    }
}

/// Why [`owner_git`] gives no git: `Absent` with no `.git`, `Refused` when `.git` is not a
/// real folder, cannot be read or, on Windows, points git elsewhere (`redirected`), and
/// `Unavailable` with no git program to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoOwnerGit {
    Absent,
    Refused,
    Unavailable,
}

/// The engine's own git program, the one [`owner_git`] runs for a project git outside owner
/// git's container: setup's git on Unix, the engine's PATH or Git for Windows on Windows.
/// Output: the program, or none when Windows finds no git.
pub(crate) fn engine_git() -> Option<std::path::PathBuf> {
    #[cfg(unix)]
    {
        Some(crate::research::runner::setup_git().to_path_buf())
    }
    #[cfg(not(unix))]
    {
        crate::github::git_program(std::env::var_os("PATH").as_deref())
    }
}

/// A project git for a canonical project folder: `git` ([`engine_git`] or owner git's) with
/// PATH and SystemRoot kept on Windows, under no outside config, hooks, fsmonitor or excludes
/// file, `.git` as repository and common folder so a planted `commondir` redirects nothing.
/// Inputs: the folder and the program or none. Output: the command, to which the caller adds
/// the subcommand and stdout, or [`NoOwnerGit`].
pub(crate) fn owner_git(
    root: &Path,
    git: Option<std::path::PathBuf>,
) -> Result<std::process::Command, NoOwnerGit> {
    use std::process::{Command, Stdio};
    let git_dir = root.join(".git");
    match std::fs::symlink_metadata(&git_dir) {
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => {
            return Err(NoOwnerGit::Absent);
        }
        Ok(meta) if meta.is_dir() && !(cfg!(windows) && redirected(&git_dir)) => {}
        _ => return Err(NoOwnerGit::Refused),
    }
    #[cfg(test)]
    AFTER_CHECK.with(|step| {
        if let Some(step) = step.borrow().as_ref() {
            step(&git_dir);
        }
    });
    let Some(git) = git else {
        return Err(NoOwnerGit::Unavailable);
    };
    let git_root = git_path(root);
    let mut command = Command::new(git);
    command
        .env_clear()
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_GLOBAL", kernel_common::names::NULL_DEVICE)
        .env("GIT_COMMON_DIR", git_root.join(".git"))
        .envs(crate::research::runner::GIT_SAFE_ENV)
        .arg("--no-optional-locks")
        .args(crate::research::runner::git_safe())
        .args(["-c", "core.untrackedCache=false", "-c"])
        .arg(format!(
            "core.excludesFile={}",
            kernel_common::names::NULL_DEVICE
        ))
        .arg("--git-dir")
        .arg(git_root.join(".git"))
        .arg("--work-tree")
        .arg(&git_root)
        .current_dir(&git_root)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    command.env("PATH", "/usr/bin:/bin");
    #[cfg(not(unix))]
    for name in ["PATH", "SystemRoot"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    Ok(command)
}

/// Reads git's NUL-separated listing and keeps each distinct path a tag may name that
/// contains the query. Inputs: git's output, the canonical project folder, the lowercased
/// query and the limit. Output: the kept paths and whether the limit or the reply bound
/// ended the reading before git's output did.
fn collect(
    output: impl std::io::Read,
    root: &Path,
    wanted: &str,
    limit: usize,
) -> (Vec<String>, bool) {
    use std::io::BufRead;
    let mut listing = std::io::BufReader::new(output.take(LISTING_BYTES_MAX));
    let mut paths: Vec<String> = Vec::new();
    let mut bytes = 0;
    let mut entry = Vec::new();
    while paths.len() < limit {
        entry.clear();
        if !matches!(listing.read_until(0, &mut entry), Ok(read) if read > 0) {
            return (paths, false);
        }
        if entry.last() == Some(&0) {
            entry.pop();
        }
        let Ok(path) = std::str::from_utf8(&entry) else {
            continue;
        };
        if paths.last().is_some_and(|last| last == path)
            || !path.to_lowercase().contains(wanted)
            || !inside(root, path)
        {
            continue;
        }
        let cost = serde_json::to_string(path).map_or(REPLY_BYTES_MAX, |text| text.len() + 1);
        if bytes + cost > REPLY_BYTES_MAX {
            return (paths, true);
        }
        bytes += cost;
        paths.push(path.to_owned());
    }
    assert!(bytes <= REPLY_BYTES_MAX);
    (paths, true)
}

/// True when a listed path names a regular file in the project: a plain relative path of
/// at most 512 bytes with no part a case-insensitive volume opens as `.git`
/// ([`crate::instruction_files::names_git`]) and no control character, not itself a link,
/// in a folder that resolves inside the project with no `.git` part, so a name NTFS opens
/// as `.git` (`.git.`, `git~1`) is refused too. Inputs: the canonical folder and the path.
fn inside(root: &Path, path: &str) -> bool {
    let relative = Path::new(path);
    let plain = relative.components().all(|part| match part {
        std::path::Component::Normal(name) => {
            !crate::instruction_files::names_git(&name.to_string_lossy())
        }
        _ => false,
    });
    if !plain || path.len() > PATH_BYTES_MAX || path.chars().any(char::is_control) {
        return false;
    }
    let full = root.join(relative);
    std::fs::symlink_metadata(&full).is_ok_and(|meta| meta.is_file())
        && full
            .parent()
            .and_then(|parent| kernel_sandbox::paths::canonical(parent).ok())
            .is_some_and(|parent| {
                parent.strip_prefix(root).is_ok_and(|below| {
                    below.components().all(|part| {
                        !crate::instruction_files::names_git(&part.as_os_str().to_string_lossy())
                    })
                })
            })
}

/// A canonical folder as git is given it: a share's verbatim `\\?\UNC\server\share` form
/// as `\\server\share`, any other path unchanged. Input: the folder. Output: the path.
fn git_path(folder: &Path) -> std::path::PathBuf {
    match folder
        .to_str()
        .and_then(|text| text.strip_prefix(r"\\?\UNC\"))
    {
        Some(share) => format!(r"\\{share}").into(),
        None => folder.to_path_buf(),
    }
}

/// True when a `.git` folder points git at another place, which Git for Windows would open as
/// the owner, a share among them: first a link at any depth or more than [`GIT_ENTRIES_MAX`]
/// entries ([`linked_within`]), so no later check reads through a link, then a `commondir` or
/// `objects\info` alternates entry of any kind, or an include in its config or worktree config,
/// or either file over 1 MiB or unreadable. Input: the `.git` folder. Output: the answer.
pub(crate) fn redirected(git_dir: &Path) -> bool {
    use std::io::Read;
    if linked_within(git_dir, GIT_ENTRIES_MAX) {
        return true;
    }
    #[cfg(test)]
    BEFORE_POINTERS.with(|step| {
        if let Some(step) = step.borrow().as_ref() {
            step(git_dir);
        }
    });
    let absent = |found: std::io::Result<std::fs::Metadata>| {
        found.is_err_and(|problem| problem.kind() == std::io::ErrorKind::NotFound)
    };
    let info = git_dir.join("objects").join("info");
    !absent(std::fs::symlink_metadata(git_dir.join("commondir")))
        || ["alternates", "http-alternates"]
            .iter()
            .any(|name| !absent(std::fs::symlink_metadata(info.join(name))))
        || ["config", "config.worktree"].iter().any(|name| {
            let file = match crate::platform::open_plain(&git_dir.join(name)) {
                Ok(file) => file,
                Err(problem) => return problem.kind() != std::io::ErrorKind::NotFound,
            };
            let mut text = Vec::new();
            !file.metadata().is_ok_and(|meta| meta.is_file())
                || file
                    .take(CONFIG_BYTES_MAX + 1)
                    .read_to_end(&mut text)
                    .is_err()
                || text.len() as u64 > CONFIG_BYTES_MAX
                || text
                    .to_ascii_lowercase()
                    .windows(b"[include".len())
                    .any(|part| part == b"[include")
        })
}

/// True when a folder holds a link, a junction among them, at any depth, an entry that cannot
/// be read, or more than `entries_max` entries; no link is followed, and each entry's kind comes
/// from the listing itself. Inputs: the folder and the bound. Output: the answer.
fn linked_within(folder: &Path, entries_max: usize) -> bool {
    let mut folders = vec![folder.to_path_buf()];
    let mut seen = 0_usize;
    while let Some(next) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            return true;
        };
        for entry in entries {
            seen += 1;
            let Ok((kind, path)) = entry.and_then(|entry| Ok((entry.file_type()?, entry.path())))
            else {
                return true;
            };
            if seen > entries_max || kind.is_symlink() {
                return true;
            }
            if kind.is_dir() {
                folders.push(path);
            }
        }
    }
    false
}

/// A step a test runs inside `owner_git` or [`redirected`], given the `.git` folder.
#[cfg(test)]
type Step = Box<dyn Fn(&Path)>;

#[cfg(test)]
thread_local! {
    /// A step the tests run once `owner_git` has checked the `.git` folder and before git
    /// starts, with that folder, standing in for an agent that changes it meanwhile. None runs
    /// nothing.
    static AFTER_CHECK: std::cell::RefCell<Option<Step>> =
        const { std::cell::RefCell::new(None) };
    /// A step the tests run when [`redirected`] starts its checks that read through `.git`'s
    /// folders, with that folder, so a row sees whether they ran. None runs nothing.
    static BEFORE_POINTERS: std::cell::RefCell<Option<Step>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs `work` and counts the gits `owner_git` gave meanwhile on this thread: one each time it
/// checked a `.git` folder and went on to build the command. Input: the work. Output: the
/// count.
#[cfg(test)]
pub(crate) fn owner_git_runs(work: impl FnOnce()) -> usize {
    let runs = std::rc::Rc::new(std::cell::Cell::new(0_usize));
    let counted = std::rc::Rc::clone(&runs);
    AFTER_CHECK.with(|step| {
        *step.borrow_mut() = Some(Box::new(move |_: &Path| counted.set(counted.get() + 1)));
    });
    work();
    AFTER_CHECK.with(|step| *step.borrow_mut() = None);
    runs.get()
}

/// Lists a repository while a step plants a `commondir` after `owner_git` checked the `.git`
/// folder, naming a common folder whose `info/exclude` hides `notes.txt` and whose config
/// names a repository format git refuses, so a followed `commondir` fails the listing.
/// Inputs: a repository with an untracked `notes.txt` and a new folder for the common one.
/// Output: what `list` answered.
#[cfg(test)]
fn listed_with_commondir_planted_after_check(
    repo: &Path,
    store: &Path,
) -> Result<Vec<String>, EngineError> {
    for name in ["objects", "refs", "info"] {
        std::fs::create_dir_all(store.join(name)).unwrap();
    }
    std::fs::write(
        store.join("config"),
        "[core]\n\trepositoryformatversion = 99\n",
    )
    .unwrap();
    std::fs::write(store.join("info").join("exclude"), "notes.txt\n").unwrap();
    let common = format!("{}\n", store.display());
    AFTER_CHECK.with(|step| {
        *step.borrow_mut() = Some(Box::new(move |git_dir: &Path| {
            std::fs::write(git_dir.join("commondir"), &common).unwrap();
        }));
    });
    let listed = list(repo, "", FILES_MAX, Instant::now() + LISTING_TIME, None);
    AFTER_CHECK.with(|step| *step.borrow_mut() = None);
    listed
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    /// Runs git as a test owner would, with an empty home and no system config.
    /// Inputs: the home, the folder and the arguments.
    fn git(home: &Path, dir: &Path, args: &[&str]) {
        let status = kernel_processes::spawn_gate::output(
            std::process::Command::new("/usr/bin/git")
                .args(args)
                .current_dir(dir)
                .env_clear()
                .env("HOME", home)
                .env("PATH", "/usr/bin:/bin")
                .env("GIT_CONFIG_NOSYSTEM", "1"),
        )
        .unwrap()
        .status;
        assert!(status.success(), "git {args:?} failed");
    }

    /// Builds a project repository with tracked, untracked and ignored files, a tracked
    /// link and an untracked link out of it, and a tracked file whose folder became a link
    /// out. Input: the scratch folder. Output: the project folder.
    fn project(place: &Path) -> std::path::PathBuf {
        let (repo, home, outside) = (
            place.join("repo"),
            place.join("home"),
            place.join("outside"),
        );
        for folder in [
            &repo,
            &home,
            &outside,
            &repo.join("src"),
            &repo.join("moved"),
        ] {
            fs::create_dir(folder).unwrap();
        }
        fs::write(outside.join("secret.txt"), "secret").unwrap();
        fs::write(outside.join("a.txt"), "outside").unwrap();
        git(&home, &repo, &["init", "-q", "--template="]);
        for (path, text) in [
            ("README.md", "readme"),
            ("src/lib.rs", "pub fn lib() {}"),
            ("src/Main.rs", "fn main() {}"),
            ("moved/a.txt", "inside"),
            (".gitignore", "target/\n*.log\n"),
        ] {
            fs::write(repo.join(path), text).unwrap();
        }
        symlink(outside.join("secret.txt"), repo.join("tracked-link.txt")).unwrap();
        git(
            &home,
            &repo,
            &["add", "README.md", "src", "moved", "tracked-link.txt"],
        );
        fs::remove_dir_all(repo.join("moved")).unwrap();
        symlink(&outside, repo.join("moved")).unwrap();
        fs::write(repo.join("notes.txt"), "untracked").unwrap();
        fs::create_dir(repo.join("target")).unwrap();
        fs::write(repo.join("target/out.bin"), "ignored").unwrap();
        fs::write(repo.join("debug.log"), "ignored").unwrap();
        symlink(&outside, repo.join("outside-link")).unwrap();
        repo
    }

    #[test]
    fn a_commondir_planted_after_the_check_redirects_nothing() {
        let place = tempfile::tempdir().unwrap();
        let repo = project(place.path());
        let listed =
            listed_with_commondir_planted_after_check(&repo, &place.path().join("store")).unwrap();
        assert!(listed.iter().any(|path| path == "notes.txt"), "{listed:?}");
    }

    fn sorted(mut paths: Vec<String>) -> Vec<String> {
        paths.sort();
        paths
    }

    fn later() -> Instant {
        Instant::now() + LISTING_TIME
    }

    #[test]
    fn lists_tracked_and_untracked_files_inside_the_project_and_nothing_ignored_or_linked() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = project(&scratch.path().canonicalize().unwrap());
        assert_eq!(
            sorted(list(&repo, "", FILES_MAX, later(), None).unwrap()),
            [
                ".gitignore",
                "README.md",
                "notes.txt",
                "src/Main.rs",
                "src/lib.rs"
            ]
        );
    }

    #[test]
    fn the_query_matches_any_case_and_the_limit_bounds_the_list() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = project(&scratch.path().canonicalize().unwrap());
        assert_eq!(
            sorted(list(&repo, "SRC/", FILES_MAX, later(), None).unwrap()),
            ["src/Main.rs", "src/lib.rs"]
        );
        assert_eq!(
            list(&repo, "main", FILES_MAX, later(), None).unwrap(),
            ["src/Main.rs"]
        );
        assert_eq!(
            list(&repo, "secret", FILES_MAX, later(), None).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(list(&repo, "", 2, later(), None).unwrap().len(), 2);
        assert_eq!(list(&repo, "", 1, later(), None).unwrap().len(), 1);
    }

    /// A listed path with a part a case-insensitive HFS+ volume opens as `.git`, such as
    /// `.g\u{200c}it` or `\u{feff}.git`, is never inside the project, as `.git` is not
    /// (RV-DS241-148).
    #[test]
    fn a_path_part_hfs_opens_as_git_is_never_inside() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        for part in [".g\u{200c}it", "\u{feff}.git"] {
            fs::create_dir(root.join(part)).unwrap();
            fs::write(root.join(part).join("config"), "[core]\n").unwrap();
            assert!(!inside(&root, &format!("{part}/config")), "{part:?}");
        }
    }

    #[test]
    fn a_folder_without_its_own_repository_is_refused_with_a_plain_code() {
        let scratch = tempfile::tempdir().unwrap();
        let place = scratch.path().canonicalize().unwrap();
        let plain = place.join("plain");
        fs::create_dir(&plain).unwrap();
        fs::write(plain.join("a.txt"), "a").unwrap();
        let code = |folder: &Path| list(folder, "", FILES_MAX, later(), None).unwrap_err().0;
        assert_eq!(code(&plain), "project_files_no_repository");
        fs::write(plain.join(".git"), "gitdir: /elsewhere\n").unwrap();
        assert_eq!(
            code(&plain),
            "project_files_no_repository",
            "a .git file is not followed"
        );
        assert_eq!(code(&place.join("absent")), "project_files_folder_missing");
        assert_eq!(
            code(Path::new("relative/folder")),
            "project_files_folder_missing"
        );
    }

    #[test]
    fn a_query_or_limit_out_of_bounds_is_refused_before_git_runs() {
        assert_eq!(
            validate("", 0).unwrap_err().0,
            "project_files_limit_invalid"
        );
        assert_eq!(
            validate("", FILES_MAX + 1).unwrap_err().0,
            "project_files_limit_invalid"
        );
        assert!(validate("", FILES_MAX).is_ok());
        assert!(validate(&"a".repeat(QUERY_BYTES_MAX), 1).is_ok());
        assert_eq!(
            validate(&"a".repeat(QUERY_BYTES_MAX + 1), 1).unwrap_err().0,
            "project_files_query_invalid"
        );
        assert_eq!(
            validate("a\nb", 1).unwrap_err().0,
            "project_files_query_invalid"
        );
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::fs;
    use std::process::{Command, Stdio};

    /// Runs git from the runner's PATH as a test owner would, with an empty home and no
    /// system config. Inputs: the home, the folder and the arguments.
    fn git(home: &Path, dir: &Path, args: &[&str]) {
        let made = kernel_processes::spawn_gate::output(
            Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("HOME", home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .stdin(Stdio::null()),
        )
        .unwrap();
        assert!(
            made.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&made.stderr)
        );
    }

    /// Makes a junction, which needs no privilege, from a folder in the project to one
    /// outside it. Inputs: the junction and its target.
    fn junction(link: &Path, target: &Path) {
        let made = kernel_processes::spawn_gate::output(
            Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .stdin(Stdio::null()),
        )
        .unwrap();
        assert!(
            made.status.success(),
            "{}",
            String::from_utf8_lossy(&made.stderr)
        );
    }

    /// Makes a repository with one commit of `README.md` and `.gitignore`, an untracked
    /// `notes.txt` and an ignored `debug.log`, owned by the runner user, and installs owner
    /// git for it. Input: the temp folder. Output: the repository folder and its owner git.
    fn contained_repository(place: &Path) -> (std::path::PathBuf, kernel_sandbox::windows::owner_git::OwnerGit) {
        use crate::sandbox::windows_farm;
        let (repo, home) = (place.join("repo"), place.join("home"));
        for folder in [&repo, &home] {
            fs::create_dir(folder).unwrap();
        }
        git(&home, &repo, &["init", "-q", "--template="]);
        for (path, text) in [
            ("README.md", "readme"),
            ("notes.txt", "untracked"),
            ("debug.log", "ignored"),
            (".gitignore", "*.log\n"),
        ] {
            fs::write(repo.join(path), text).unwrap();
        }
        git(&home, &repo, &["add", "README.md", ".gitignore"]);
        git(
            &home,
            &repo,
            &[
                "-c",
                "user.name=Owner",
                "-c",
                "user.email=owner@example.invalid",
                "commit",
                "-q",
                "--no-gpg-sign",
                "-m",
                "start",
            ],
        );
        farm::own(&repo);
        let program = crate::github::git_program(std::env::var_os("PATH").as_deref()).unwrap();
        let owner = farm::installed(place, &repo, &program);
        (repo, owner)
    }

    #[test]
    #[ignore = "needs unc-share-setup.ps1's shares and device entries on an elevated runner (test farm)"]
    fn a_contained_windows_list_opens_no_share_a_git_folder_names_after_the_check() {
        use crate::sandbox::windows_farm;
        let sessions = farm::hold();
        let mut reached = Vec::new();
        for (shape, _) in farm::SHAPES {
            let place = tempfile::tempdir().unwrap();
            let (repo, owner) = contained_repository(place.path());
            let before = list(
                &repo,
                "",
                FILES_MAX,
                Instant::now() + LISTING_TIME,
                Some(&owner),
            );
            assert!(
                before
                    .as_ref()
                    .is_ok_and(|listed| listed.iter().any(|path| path == "README.md")),
                "{shape}: a contained list before the plant: {before:?}"
            );
            farm::reset_sessions(&sessions);
            if shape == "nested" {
                farm::plant(&repo, shape);
            } else {
                let planted = repo.clone();
                AFTER_CHECK.with(|step| {
                    *step.borrow_mut() =
                        Some(Box::new(move |_: &Path| farm::plant(&planted, shape)));
                });
            }
            let stop = Instant::now() + LISTING_TIME;
            let listed = list(&repo, "", FILES_MAX, stop, Some(&owner));
            AFTER_CHECK.with(|step| *step.borrow_mut() = None);
            if farm::server_session(&sessions) {
                reached.push(format!("{shape} list: {listed:?}"));
            }
        }
        assert!(
            reached.is_empty(),
            "a server session after a contained list: {}",
            reached.join(", ")
        );
    }

    #[test]
    #[ignore = "needs unc-share-setup.ps1's device entries on an elevated runner (test farm)"]
    fn a_contained_windows_list_names_new_files_and_leaves_out_ignored_ones() {
        let place = tempfile::tempdir().unwrap();
        let (repo, owner) = contained_repository(place.path());
        let stop = Instant::now() + LISTING_TIME;
        let mut listed = list(&repo, "", FILES_MAX, stop, Some(&owner)).unwrap();
        listed.sort();
        assert_eq!(
            listed,
            [".gitignore", "README.md", "notes.txt"],
            "tracked and unignored new files, as the Unix list names them"
        );
        let mut declined = list(&repo, "", FILES_MAX, stop, None).unwrap();
        declined.sort();
        assert_eq!(
            declined,
            [".gitignore", "README.md"],
            "the declined list stays tracked-only"
        );
    }

    #[test]
    fn the_declined_windows_list_names_its_tracked_files_only_and_none_through_a_junction() {
        let place = tempfile::tempdir().unwrap();
        let (repo, home, outside) = (
            place.path().join("repo"),
            place.path().join("home"),
            place.path().join("outside"),
        );
        for folder in [&repo, &home, &outside, &repo.join("src"), &repo.join("out")] {
            fs::create_dir(folder).unwrap();
        }
        fs::write(outside.join("secret.txt"), "secret").unwrap();
        git(&home, &repo, &["init", "-q", "--template="]);
        for (path, text) in [
            ("README.md", "readme"),
            ("src/Lib.rs", "pub fn lib() {}"),
            ("out/secret.txt", "tracked"),
            ("notes.txt", "untracked"),
            ("debug.log", "ignored"),
            (".gitignore", "*.log\n"),
        ] {
            fs::write(repo.join(path), text).unwrap();
        }
        git(&home, &repo, &["add", "README.md", "src", "out"]);
        fs::remove_dir_all(repo.join("out")).unwrap();
        junction(&repo.join("out"), &outside);
        let stop = Instant::now() + LISTING_TIME;
        let mut listed = list(&repo, "", FILES_MAX, stop, None).unwrap();
        listed.sort();
        assert_eq!(
            listed,
            ["README.md", "src/Lib.rs"],
            "tracked files only, and no tracked path whose folder became a junction"
        );
        assert_eq!(
            list(&repo, "lib", FILES_MAX, stop, None).unwrap(),
            ["src/Lib.rs"],
            "the query matches in any case"
        );
        assert_eq!(
            list(&outside, "", FILES_MAX, stop, None).unwrap_err().0,
            "project_files_no_repository"
        );
    }

    /// Makes an empty repository with its own home beside it. Input: the temp folder.
    /// Output: the repository folder and the home.
    fn repository(place: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let (repo, home) = (place.join("repo"), place.join("home"));
        for folder in [&repo, &home] {
            fs::create_dir(folder).unwrap();
        }
        git(&home, &repo, &["init", "-q", "--template="]);
        (repo, home)
    }

    #[test]
    fn a_windows_repository_whose_git_folder_points_elsewhere_is_not_listed() {
        let place = tempfile::tempdir().unwrap();
        let (repo, home) = repository(place.path());
        fs::write(repo.join("README.md"), "readme").unwrap();
        git(&home, &repo, &["add", "README.md"]);
        let (git_dir, stop) = (repo.join(".git"), Instant::now() + LISTING_TIME);
        fs::write(git_dir.join("commondir"), r"\\127.0.0.1\x").unwrap();
        assert_eq!(
            list(&repo, "", FILES_MAX, stop, None).unwrap_err().0,
            "project_files_no_repository",
            "a commondir file is refused before git runs"
        );
        fs::remove_file(git_dir.join("commondir")).unwrap();
        let config = git_dir.join("config");
        let kept = fs::read_to_string(&config).unwrap();
        for section in [
            "[include]\n\tpath = //127.0.0.1/x\n",
            "[IncludeIf \"gitdir:C:/\"]\n\tpath = //127.0.0.1/x\n",
        ] {
            fs::write(&config, format!("{kept}{section}")).unwrap();
            assert_eq!(
                list(&repo, "", FILES_MAX, stop, None).unwrap_err().0,
                "project_files_no_repository",
                "{section}"
            );
        }
        fs::write(&config, &kept).unwrap();
        assert_eq!(
            list(&repo, "", FILES_MAX, stop, None).unwrap(),
            ["README.md"]
        );
    }

    #[test]
    fn a_windows_commondir_planted_after_the_check_redirects_nothing() {
        let place = tempfile::tempdir().unwrap();
        let (repo, home) = repository(place.path());
        fs::write(repo.join("README.md"), "readme").unwrap();
        git(&home, &repo, &["add", "README.md"]);
        fs::write(repo.join("notes.txt"), "untracked").unwrap();
        let listed =
            listed_with_commondir_planted_after_check(&repo, &place.path().join("store")).unwrap();
        assert_eq!(listed, ["README.md"]);
    }

    #[test]
    fn a_verbatim_share_folder_reaches_git_in_its_plain_form() {
        assert_eq!(
            git_path(Path::new(r"\\?\UNC\server\share\repo")),
            Path::new(r"\\server\share\repo")
        );
        assert_eq!(git_path(Path::new(r"C:\repo")), Path::new(r"C:\repo"));
    }

    #[test]
    fn a_windows_index_entry_that_ntfs_opens_as_git_is_not_listed() {
        let place = tempfile::tempdir().unwrap();
        let (repo, home) = repository(place.path());
        fs::write(repo.join("empty.txt"), "").unwrap();
        git(&home, &repo, &["add", "empty.txt"]);
        for name in [".git./config", ".git /config", "git~1/config"] {
            let entry = format!("100644,e69de29bb2d1d6434b8b29ae775ad8c2e48c5391,{name}");
            git(
                &home,
                &repo,
                &[
                    "-c",
                    "core.protectNTFS=false",
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &entry,
                ],
            );
        }
        let stop = Instant::now() + LISTING_TIME;
        assert_eq!(
            list(&repo, "", FILES_MAX, stop, None).unwrap(),
            ["empty.txt"]
        );
    }

    /// Makes a repository with one commit, packed refs and an index, the `.git` an owner's
    /// project holds, with an untracked `notes.txt` beside the tracked `README.md`. Input:
    /// the temp folder. Output: the repository folder.
    fn committed(place: &Path) -> std::path::PathBuf {
        let (repo, home) = repository(place);
        fs::write(repo.join("README.md"), "readme").unwrap();
        git(&home, &repo, &["add", "README.md"]);
        git(
            &home,
            &repo,
            &[
                "-c",
                "user.name=Owner",
                "-c",
                "user.email=owner@example.invalid",
                "commit",
                "-q",
                "--no-gpg-sign",
                "-m",
                "start",
            ],
        );
        git(&home, &repo, &["pack-refs", "--all"]);
        fs::create_dir_all(repo.join(".git").join("info")).unwrap();
        fs::write(repo.join("notes.txt"), "untracked").unwrap();
        repo
    }

    /// Replaces one entry of a `.git` folder with a link to a new place outside it: a
    /// junction for a folder, which needs no privilege, a file link for anything else,
    /// which the farm's elevated runner may make. Inputs: the `.git` folder, the entry and
    /// the outside folder. The entry's `/` become `\`, since `mklink` reads a `/` in a path as
    /// a switch.
    fn linked(git_dir: &Path, entry: &str, outside: &Path) {
        let inside = entry
            .split('/')
            .fold(git_dir.to_path_buf(), |at, part| at.join(part));
        let target = outside.join(entry.replace('/', "-"));
        if inside.is_dir() {
            fs::create_dir_all(&target).unwrap();
            fs::remove_dir_all(&inside).unwrap();
            junction(&inside, &target);
        } else {
            fs::write(&target, "").unwrap();
            if fs::symlink_metadata(&inside).is_ok() {
                fs::remove_file(&inside).unwrap();
            }
            std::os::windows::fs::symlink_file(&target, &inside)
                .unwrap_or_else(|problem| panic!("file link {entry}: {problem}"));
        }
    }

    /// Asserts `answer` is the refusal a judged `.git` gets, `project_files_no_repository`.
    /// Inputs: the answer and the planted shape, which names the failure.
    /// Output: none; any other answer fails the test.
    fn judged(answer: Result<Vec<String>, EngineError>, shape: &str) {
        assert!(
            matches!(&answer, Err(EngineError(code)) if code == "project_files_no_repository"),
            "{shape}: {answer:?}"
        );
    }

    #[test]
    fn a_windows_git_folder_with_an_alternates_file_of_any_kind_is_not_listed() {
        for (name, text) in [
            ("alternates", "\\\\127.0.0.1\\x\\objects\n"),
            ("alternates", "C:\\elsewhere\\objects\n"),
            ("alternates", ""),
            ("http-alternates", "http://127.0.0.1/x\n"),
            ("http-alternates", ""),
        ] {
            let place = tempfile::tempdir().unwrap();
            let repo = committed(place.path());
            let info = repo.join(".git").join("objects").join("info");
            fs::create_dir_all(&info).unwrap();
            fs::write(info.join(name), text).unwrap();
            let stop = Instant::now() + LISTING_TIME;
            judged(
                list(&repo, "", FILES_MAX, stop, None),
                &format!("{name} {text:?}"),
            );
        }
    }

    #[test]
    fn a_windows_git_folder_whose_fixed_entry_is_a_link_is_not_listed() {
        for entry in [
            "HEAD",
            "index",
            "config",
            "packed-refs",
            "shallow",
            "info/exclude",
            "info/attributes",
            "info",
            "objects",
            "objects/info",
            "refs",
        ] {
            let place = tempfile::tempdir().unwrap();
            let repo = committed(place.path());
            let outside = place.path().join("outside");
            fs::create_dir(&outside).unwrap();
            linked(&repo.join(".git"), entry, &outside);
            let stop = Instant::now() + LISTING_TIME;
            judged(list(&repo, "", FILES_MAX, stop, None), entry);
        }
    }

    #[test]
    fn a_windows_link_deeper_in_the_git_folder_is_not_listed() {
        for entry in ["refs/heads/side", "objects/pack/pack-0.pack"] {
            let place = tempfile::tempdir().unwrap();
            let repo = committed(place.path());
            let git_dir = repo.join(".git");
            fs::create_dir_all(git_dir.join(entry).parent().unwrap()).unwrap();
            let outside = place.path().join("outside");
            fs::create_dir(&outside).unwrap();
            linked(&git_dir, entry, &outside);
            let stop = Instant::now() + LISTING_TIME;
            judged(list(&repo, "", FILES_MAX, stop, None), entry);
        }
    }

    #[test]
    fn a_plain_windows_git_folder_with_history_passes_the_judge() {
        let place = tempfile::tempdir().unwrap();
        let repo = committed(place.path());
        let stop = Instant::now() + LISTING_TIME;
        assert_eq!(
            list(&repo, "", FILES_MAX, stop, None).unwrap(),
            ["README.md"]
        );
        assert!(!redirected(&repo.join(".git")));
    }

    #[test]
    fn the_declined_windows_list_names_tracked_files_only_and_never_opens_a_nested_git_file() {
        let place = tempfile::tempdir().unwrap();
        let repo = committed(place.path());
        let nested = repo.join("sub");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("x.txt"), "untracked").unwrap();
        fs::write(nested.join(".git"), "gitdir: \\\\127.0.0.1\\x\\nested\n").unwrap();
        let stop = Instant::now() + LISTING_TIME;
        assert_eq!(
            list(&repo, "", FILES_MAX, stop, None).unwrap(),
            ["README.md"],
            "tracked only: no notes.txt, nothing under sub"
        );
        assert_eq!(
            list(&repo, "read", FILES_MAX, stop, None).unwrap(),
            ["README.md"]
        );
    }

    #[test]
    fn the_git_folder_walk_runs_before_any_check_that_reads_through_its_folders() {
        let place = tempfile::tempdir().unwrap();
        let repo = committed(place.path());
        let git_dir = repo.join(".git");
        let reached = std::rc::Rc::new(std::cell::Cell::new(0_u32));
        let seen = std::rc::Rc::clone(&reached);
        BEFORE_POINTERS.with(|step| {
            *step.borrow_mut() = Some(Box::new(move |_: &Path| seen.set(seen.get() + 1)));
        });
        let plain = redirected(&git_dir);
        let outside = place.path().join("outside");
        fs::create_dir(&outside).unwrap();
        linked(&git_dir, "objects", &outside);
        let linked_objects = redirected(&git_dir);
        BEFORE_POINTERS.with(|step| *step.borrow_mut() = None);
        assert!(!plain, "a plain .git passes");
        assert!(linked_objects, "a linked objects folder is refused");
        assert_eq!(
            reached.get(),
            1,
            "the pointer checks ran for the plain .git only, never through the linked objects"
        );
    }

    #[test]
    fn the_git_folder_walk_passes_at_its_bound_and_refuses_past_it() {
        let place = tempfile::tempdir().unwrap();
        let folder = place.path().join("git");
        fs::create_dir_all(folder.join("refs").join("heads")).unwrap();
        fs::write(folder.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert!(!linked_within(&folder, 3), "three entries, at the bound");
        assert!(linked_within(&folder, 2), "three entries, past the bound");
    }
}
