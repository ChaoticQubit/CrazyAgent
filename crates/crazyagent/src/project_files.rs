use crate::engine::EngineError;
use std::path::Path;
use std::time::{Duration, Instant};

/// The most paths one file-list read returns.
pub const FILES_MAX: u16 = 200;
/// The longest file-list query, in bytes.
pub const QUERY_BYTES_MAX: usize = 256;
/// The longest a file-list read lets git run before it answers with what it has.
pub const LISTING_TIME: Duration = Duration::from_secs(5);
#[cfg(unix)]
const PATH_BYTES_MAX: usize = 512;
#[cfg(unix)]
const REPLY_BYTES_MAX: usize = 48 * 1024;
#[cfg(unix)]
const LISTING_BYTES_MAX: u64 = 32 * 1024 * 1024;
#[cfg(unix)]
const POLL: Duration = Duration::from_millis(2);

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

/// Lists a project's files for `@` tags: the paths git tracks and the untracked paths it
/// does not ignore, relative to the project folder, whose path contains the query in any
/// case. Never lists `.git`, a link, or a file reached through a link out of the folder.
/// Runs the real git with no outside config, hooks or fsmonitor, and stops at the limit,
/// at a 48 KiB reply or at `stop`, answering with the paths found by then. Inputs: the
/// absolute project folder, the query, the limit and the time to stop. Output: the paths
/// in git's order, or `project_files_folder_missing`, `project_files_no_repository` or
/// `project_files_git_failed`. Git is given the `.git` folder as its common folder, so a
/// `commondir` planted there at any time redirects nothing.
#[cfg(unix)]
pub(crate) fn list(
    folder: &Path,
    query: &str,
    limit: u16,
    stop: Instant,
) -> Result<Vec<String>, EngineError> {
    use std::process::{Command, Stdio};
    validate(query, limit)?;
    if !folder.is_absolute() {
        return Err(failure("project_files_folder_missing"));
    }
    let root = folder
        .canonicalize()
        .ok()
        .filter(|root| root.is_dir())
        .ok_or_else(|| failure("project_files_folder_missing"))?;
    let git_dir = root.join(".git");
    if !std::fs::symlink_metadata(&git_dir).is_ok_and(|meta| meta.is_dir()) {
        return Err(failure("project_files_no_repository"));
    }
    #[cfg(test)]
    AFTER_CHECK.with(|step| {
        if let Some(step) = step.borrow().as_ref() {
            step(&git_dir);
        }
    });
    let git_failed = |_| failure("project_files_git_failed");
    let mut command = Command::new(crate::research::runner::setup_git());
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_COMMON_DIR", &git_dir)
        .envs(crate::research::runner::GIT_SAFE_ENV)
        .arg("--no-optional-locks")
        .args(crate::research::runner::GIT_SAFE)
        .args(["-c", "core.untrackedCache=false"])
        .arg("--git-dir")
        .arg(&git_dir)
        .arg("--work-tree")
        .arg(&root)
        .current_dir(&root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = crate::spawn_gate::spawn(&mut command).map_err(git_failed)?;
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

/// Refuses a file list where the engine's git listing does not exist. Takes the folder,
/// query, limit and stop, unused. Returns `project_files_unavailable`.
#[cfg(not(unix))]
pub(crate) fn list(
    _folder: &Path,
    _query: &str,
    _limit: u16,
    _stop: Instant,
) -> Result<Vec<String>, EngineError> {
    Err(failure("project_files_unavailable"))
}

/// Reads git's NUL-separated listing and keeps each distinct path a tag may name that
/// contains the query. Inputs: git's output, the canonical project folder, the lowercased
/// query and the limit. Output: the kept paths and whether the limit or the reply bound
/// ended the reading before git's output did.
#[cfg(unix)]
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
/// in a folder that resolves inside the project. Inputs: the canonical folder and the path.
#[cfg(unix)]
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
            .and_then(|parent| parent.canonicalize().ok())
            .is_some_and(|parent| parent.starts_with(root))
}

/// A test step `list` runs with the `.git` folder it checked. Input: that folder. Output:
/// none.
#[cfg(all(test, unix))]
type AfterCheck = Box<dyn Fn(&Path)>;

#[cfg(all(test, unix))]
thread_local! {
    /// A step the tests run once `list` has checked the `.git` folder and before git starts,
    /// with that folder, standing in for an agent that changes it meanwhile. None runs nothing.
    static AFTER_CHECK: std::cell::RefCell<Option<AfterCheck>> =
        const { std::cell::RefCell::new(None) };
}

/// Lists a repository while a step plants a `commondir` after `list` checked the `.git`
/// folder, naming a common folder whose `info/exclude` hides `notes.txt`. Inputs: a
/// repository with an untracked `notes.txt` and a new folder for the common one. Output:
/// what `list` answered.
#[cfg(all(test, unix))]
fn listed_with_commondir_planted_after_check(
    repo: &Path,
    store: &Path,
) -> Result<Vec<String>, EngineError> {
    for name in ["objects", "refs", "info"] {
        std::fs::create_dir_all(store.join(name)).unwrap();
    }
    std::fs::write(
        store.join("config"),
        "[core]\n\trepositoryformatversion = 0\n",
    )
    .unwrap();
    std::fs::write(store.join("info").join("exclude"), "notes.txt\n").unwrap();
    let common = format!("{}\n", store.display());
    AFTER_CHECK.with(|step| {
        *step.borrow_mut() = Some(Box::new(move |git_dir: &Path| {
            std::fs::write(git_dir.join("commondir"), &common).unwrap();
        }));
    });
    let listed = list(repo, "", FILES_MAX, Instant::now() + LISTING_TIME);
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
        let status = crate::spawn_gate::output(
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
            sorted(list(&repo, "", FILES_MAX, later()).unwrap()),
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
            sorted(list(&repo, "SRC/", FILES_MAX, later()).unwrap()),
            ["src/Main.rs", "src/lib.rs"]
        );
        assert_eq!(
            list(&repo, "main", FILES_MAX, later()).unwrap(),
            ["src/Main.rs"]
        );
        assert_eq!(
            list(&repo, "secret", FILES_MAX, later()).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(list(&repo, "", 2, later()).unwrap().len(), 2);
        assert_eq!(list(&repo, "", 1, later()).unwrap().len(), 1);
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
        let code = |folder: &Path| list(folder, "", FILES_MAX, later()).unwrap_err().0;
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
