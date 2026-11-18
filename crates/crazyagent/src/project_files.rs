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
/// `project_files_git_failed`. The git is setup's on Unix
/// (`research::runner::setup_git`) and on Windows the one the engine's PATH or Git
/// for Windows names ([`crate::github::git_program`]), with the engine's PATH and
/// SystemRoot, which it needs. Paths are made canonical by [`crate::sandbox::canonical`] and
/// handed to git as `git_path` gives them, so Git for Windows never gets a verbatim path. The
/// repository's own `core.excludesFile` is never read, and on Windows a `.git` that points
/// git elsewhere (`redirected`) gives `project_files_no_repository`.
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
    let root = crate::sandbox::canonical(folder)
        .ok()
        .filter(|root| root.is_dir())
        .ok_or_else(|| failure("project_files_folder_missing"))?;
    let git_dir = root.join(".git");
    if !std::fs::symlink_metadata(&git_dir).is_ok_and(|meta| meta.is_dir())
        || (cfg!(windows) && redirected(&git_dir))
    {
        return Err(failure("project_files_no_repository"));
    }
    let git_root = git_path(&root);
    let git_failed = |_| failure("project_files_git_failed");
    #[cfg(unix)]
    let git = crate::research::runner::setup_git().to_path_buf();
    #[cfg(not(unix))]
    let git = crate::github::git_program(std::env::var_os("PATH").as_deref())
        .ok_or_else(|| failure("project_files_git_failed"))?;
    let mut command = Command::new(git);
    command
        .env_clear()
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_GLOBAL", crate::github::NULL_DEVICE)
        .envs(crate::research::runner::GIT_SAFE_ENV)
        .arg("--no-optional-locks")
        .args(crate::research::runner::GIT_SAFE)
        .args(["-c", "core.untrackedCache=false", "-c"])
        .arg(format!("core.excludesFile={}", crate::github::NULL_DEVICE))
        .arg("--git-dir")
        .arg(git_root.join(".git"))
        .arg("--work-tree")
        .arg(&git_root)
        .current_dir(&git_root)
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
    #[cfg(unix)]
    command.env("PATH", "/usr/bin:/bin");
    #[cfg(not(unix))]
    for name in ["PATH", "SystemRoot"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
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
            .and_then(|parent| crate::sandbox::canonical(parent).ok())
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
/// the owner, a share among them: a `commondir` entry of any kind, or an `include` or
/// `includeIf` section in its config or worktree config, or either file that is a link, over
/// 1 MiB or unreadable (rv-sec2 #492 F1). Input: the `.git` folder. Output: the answer.
fn redirected(git_dir: &Path) -> bool {
    use std::io::Read;
    let absent = |found: std::io::Result<std::fs::Metadata>| {
        found.is_err_and(|problem| problem.kind() == std::io::ErrorKind::NotFound)
    };
    !absent(std::fs::symlink_metadata(git_dir.join("commondir")))
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

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::fs;
    use std::process::{Command, Stdio};

    /// Runs git from the runner's PATH as a test owner would, with an empty home and no
    /// system config. Inputs: the home, the folder and the arguments.
    fn git(home: &Path, dir: &Path, args: &[&str]) {
        let made = crate::spawn_gate::output(
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
        let made = crate::spawn_gate::output(
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

    #[test]
    fn a_windows_project_lists_its_tracked_and_untracked_files_but_no_ignored_one_or_one_through_a_junction()
     {
        let place = tempfile::tempdir().unwrap();
        let (repo, home, outside) = (
            place.path().join("repo"),
            place.path().join("home"),
            place.path().join("outside"),
        );
        for folder in [&repo, &home, &outside, &repo.join("src")] {
            fs::create_dir(folder).unwrap();
        }
        fs::write(outside.join("secret.txt"), "secret").unwrap();
        git(&home, &repo, &["init", "-q", "--template="]);
        for (path, text) in [
            ("README.md", "readme"),
            ("src/Lib.rs", "pub fn lib() {}"),
            ("notes.txt", "untracked"),
            ("debug.log", "ignored"),
            (".gitignore", "*.log\n"),
        ] {
            fs::write(repo.join(path), text).unwrap();
        }
        git(&home, &repo, &["add", "README.md", "src"]);
        junction(&repo.join("out"), &outside);
        let stop = Instant::now() + LISTING_TIME;
        let mut listed = list(&repo, "", FILES_MAX, stop).unwrap();
        listed.sort();
        assert_eq!(
            listed,
            [".gitignore", "README.md", "notes.txt", "src/Lib.rs"],
            "tracked and untracked files, no ignored file and nothing through the junction"
        );
        assert_eq!(
            list(&repo, "lib", FILES_MAX, stop).unwrap(),
            ["src/Lib.rs"],
            "the query matches in any case"
        );
        assert_eq!(
            list(&outside, "", FILES_MAX, stop).unwrap_err().0,
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
        let (repo, _home) = repository(place.path());
        fs::write(repo.join("README.md"), "readme").unwrap();
        let (git_dir, stop) = (repo.join(".git"), Instant::now() + LISTING_TIME);
        fs::write(git_dir.join("commondir"), r"\\127.0.0.1\x").unwrap();
        assert_eq!(
            list(&repo, "", FILES_MAX, stop).unwrap_err().0,
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
                list(&repo, "", FILES_MAX, stop).unwrap_err().0,
                "project_files_no_repository",
                "{section}"
            );
        }
        fs::write(&config, &kept).unwrap();
        assert_eq!(list(&repo, "", FILES_MAX, stop).unwrap(), ["README.md"]);
    }

    #[test]
    fn a_windows_repository_own_excludes_file_is_not_read() {
        let place = tempfile::tempdir().unwrap();
        let (repo, home) = repository(place.path());
        let excludes = place.path().join("excludes");
        fs::write(&excludes, "notes.txt\n").unwrap();
        let excludes = excludes.to_string_lossy().into_owned();
        git(&home, &repo, &["config", "core.excludesFile", &excludes]);
        fs::write(repo.join("notes.txt"), "untracked").unwrap();
        let stop = Instant::now() + LISTING_TIME;
        assert_eq!(list(&repo, "", FILES_MAX, stop).unwrap(), ["notes.txt"]);
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
        assert_eq!(list(&repo, "", FILES_MAX, stop).unwrap(), ["empty.txt"]);
    }
}
