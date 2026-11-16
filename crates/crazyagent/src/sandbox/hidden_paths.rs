use super::canonical;
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};

/// The most folders one project may keep its agents out of.
pub const HIDDEN_PATHS_MAX: usize = 8;

const PATH_TEXT_MAX: usize = 1024;

const TABLE: &str = "CREATE TABLE IF NOT EXISTS project_hidden_paths (
    project TEXT NOT NULL,
    position INTEGER NOT NULL,
    path TEXT NOT NULL,
    PRIMARY KEY (project, position)
)";

/// Checks one folder the owner wants this project's agents kept out of: an absolute existing
/// folder, taken in its canonical form, that sits strictly inside the project root. A folder
/// outside the project, the project root itself, or a path that is not a folder is refused.
/// Inputs: the typed path and the project root. Output: the canonical folder, or a plain
/// refusal code.
pub fn checked(text: &str, project_root: &Path) -> Result<PathBuf, &'static str> {
    let typed = Path::new(text);
    if text.len() > PATH_TEXT_MAX || !typed.is_absolute() {
        return Err("hidden_path_not_absolute");
    }
    if text
        .chars()
        .any(|c| c.is_control() || c == '"' || c == '\\' || c == '\'')
    {
        return Err("hidden_path_not_plain");
    }
    let real = canonical(typed).map_err(|_| "hidden_path_missing")?;
    if !real.is_dir() {
        return Err("hidden_path_not_folder");
    }
    let root = canonical(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    if !real.starts_with(&root) || real == root {
        return Err("hidden_path_outside_project");
    }
    Ok(real)
}

/// Checks a project's whole list of hidden folders as [`checked`] does, at most
/// [`HIDDEN_PATHS_MAX`], dropping repeats. Inputs: the typed paths and the project root.
/// Output: the canonical folders in order, or the first refusal code.
pub fn checked_all(texts: &[String], project_root: &Path) -> Result<Vec<PathBuf>, &'static str> {
    if texts.len() > HIDDEN_PATHS_MAX {
        return Err("hidden_paths_too_many");
    }
    let mut paths: Vec<PathBuf> = Vec::with_capacity(texts.len());
    for text in texts {
        let path = checked(text, project_root)?;
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    assert!(paths.len() <= HIDDEN_PATHS_MAX);
    Ok(paths)
}

/// Replaces a project's hidden folders. Inputs: the open connection or transaction, the
/// project id and the checked folders. Output: done, or the database error.
pub fn save(connection: &Connection, project: &str, paths: &[PathBuf]) -> rusqlite::Result<()> {
    assert!(paths.len() <= HIDDEN_PATHS_MAX);
    connection.execute(TABLE, [])?;
    connection.execute(
        "DELETE FROM project_hidden_paths WHERE project=?1",
        [project],
    )?;
    for (position, path) in paths.iter().enumerate() {
        let text = path
            .to_str()
            .ok_or_else(|| rusqlite::Error::InvalidPath(path.clone()))?;
        connection.execute(
            "INSERT INTO project_hidden_paths (project, position, path) VALUES (?1, ?2, ?3)",
            params![project, position as i64, text],
        )?;
    }
    Ok(())
}

/// Reads a project's hidden folders in the order the owner saved them; none when nothing was
/// ever saved. Inputs: the open connection or transaction and the project id. Output: the
/// folders, or the database error.
pub fn load(connection: &Connection, project: &str) -> rusqlite::Result<Vec<PathBuf>> {
    let present: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='project_hidden_paths')",
        [],
        |row| row.get(0),
    )?;
    if !present {
        return Ok(Vec::new());
    }
    let mut statement = connection.prepare(
        "SELECT path FROM project_hidden_paths WHERE project=?1 ORDER BY position LIMIT ?2",
    )?;
    let paths = statement
        .query_map(params![project, HIDDEN_PATHS_MAX as i64], |row| {
            row.get::<_, String>(0).map(PathBuf::from)
        })?
        .collect::<rusqlite::Result<Vec<PathBuf>>>()?;
    assert!(paths.len() <= HIDDEN_PATHS_MAX);
    Ok(paths)
}

/// The longest the tracked-files check lets git run before it is killed and the folder is
/// refused as unchecked.
#[cfg(unix)]
const CHECK_TIME: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(unix)]
const CHECK_POLL: std::time::Duration = std::time::Duration::from_millis(2);
/// The most of git's answer the check reads; a tracked folder shows in the first bytes.
#[cfg(any(unix, test))]
const CHECK_BYTES_MAX: u64 = 64 * 1024;

/// Whether git tracks any file inside this folder, asked in the project's own checkout with
/// the real git under no outside config, hooks, fsmonitor or lazy fetch, so a hostile
/// imported repository cannot run a command as the owner. A hidden folder covers only files
/// git does not track; a tracked file is copied into each agent's worktree and lives in
/// history, so hiding one would be a false promise. Run outside any sandbox with a deadline
/// that kills git. Inputs: the project root and the canonical folder. Output: `Ok(true)` when
/// git names any path (whatever its exit status, so a listing past the read cap still counts
/// as tracked), `Ok(false)` when the folder is outside the root or the root has no `.git` at
/// all or git lists nothing and exits clean, and `Err("hidden_path_unchecked")` when `.git`
/// exists but is not a real folder (a linked worktree or submodule gitlink), or on any git
/// error, non-zero exit with no output, timeout or unreadable answer, so the caller fails
/// closed.
#[cfg(unix)]
pub fn git_tracks(project_root: &Path, folder: &Path) -> Result<bool, &'static str> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let unchecked = "hidden_path_unchecked";
    let root = canonical(project_root).map_err(|_| unchecked)?;
    let real = canonical(folder).map_err(|_| unchecked)?;
    let Ok(relative) = real.strip_prefix(&root) else {
        return Ok(false);
    };
    let git_dir = root.join(".git");
    match std::fs::symlink_metadata(&git_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(unchecked),
        Ok(meta) if !meta.is_dir() => return Err(unchecked),
        Ok(_) => {}
    }
    let mut command = Command::new(crate::research::runner::setup_git());
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .envs(crate::research::runner::GIT_SAFE_ENV)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .arg("--no-optional-locks")
        .args(crate::research::runner::GIT_SAFE)
        .arg("--git-dir")
        .arg(&git_dir)
        .arg("--work-tree")
        .arg(&root)
        .current_dir(&root)
        .args(["ls-files", "-z", "--"])
        .arg(relative)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = crate::spawn_gate::spawn(&mut command).map_err(|_| unchecked)?;
    let mut stdout = child.stdout.take().ok_or(unchecked)?;
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = (&mut stdout).take(CHECK_BYTES_MAX).read_to_end(&mut buffer);
        buffer
    });
    let deadline = std::time::Instant::now() + CHECK_TIME;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(unchecked);
            }
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(unchecked);
        }
        std::thread::sleep(CHECK_POLL);
    };
    let bytes = reader.join().map_err(|_| unchecked)?;
    if !bytes.is_empty() {
        Ok(true)
    } else if status.success() {
        Ok(false)
    } else {
        Err(unchecked)
    }
}

/// Refuses the tracked-files check where no confined git runs. Inputs unused. Output:
/// `Err("hidden_path_unchecked")`, so the caller fails closed.
#[cfg(not(unix))]
pub fn git_tracks(_project_root: &Path, _folder: &Path) -> Result<bool, &'static str> {
    Err("hidden_path_unchecked")
}

/// Refuses a list that holds a folder git tracks, and refuses the whole list when the check
/// cannot answer for any folder. Inputs: the project root and the checked folders. Output:
/// ok, `hidden_path_tracked` for the first tracked folder, or `hidden_path_unchecked`.
pub fn none_tracked(project_root: &Path, folders: &[PathBuf]) -> Result<(), &'static str> {
    for folder in folders {
        if git_tracks(project_root, folder)? {
            return Err("hidden_path_tracked");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn project() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("ca-hidden-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap()
    }

    #[test]
    fn a_folder_inside_the_project_is_accepted_and_one_outside_is_refused() {
        let root = project();
        let inside = root.path().join("notes");
        std::fs::create_dir(&inside).unwrap();
        let got = checked(inside.to_str().unwrap(), root.path()).unwrap();
        assert_eq!(got, std::fs::canonicalize(&inside).unwrap());

        let outside = project();
        assert_eq!(
            checked(outside.path().to_str().unwrap(), root.path()),
            Err("hidden_path_outside_project")
        );
        assert_eq!(
            checked(root.path().to_str().unwrap(), root.path()),
            Err("hidden_path_outside_project")
        );
        assert_eq!(
            checked("relative/notes", root.path()),
            Err("hidden_path_not_absolute")
        );
    }

    #[test]
    fn at_most_eight_folders_are_checked() {
        let root = project();
        let mut texts = Vec::new();
        for n in 0..9 {
            let folder = root.path().join(format!("f{n}"));
            std::fs::create_dir(&folder).unwrap();
            texts.push(folder.to_str().unwrap().to_owned());
        }
        assert_eq!(
            checked_all(&texts, root.path()),
            Err("hidden_paths_too_many")
        );
        assert_eq!(
            checked_all(&texts[..HIDDEN_PATHS_MAX], root.path())
                .unwrap()
                .len(),
            HIDDEN_PATHS_MAX
        );
    }

    #[test]
    fn a_folder_whose_name_holds_a_quote_or_backslash_is_refused() {
        let root = project();
        for bad in ["/tmp/a\"b", "/tmp/a\\b", "/tmp/a'b", "/tmp/a\nb"] {
            assert_eq!(checked(bad, root.path()), Err("hidden_path_not_plain"));
        }
    }

    #[test]
    fn a_folder_git_tracks_is_refused_and_an_untracked_one_is_accepted() {
        let root = project();
        let path = root.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            let mut command = std::process::Command::new("/usr/bin/git");
            command
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .current_dir(&path)
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            let status = crate::spawn_gate::status(&mut command).unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        let tracked = path.join("secret");
        let untracked = path.join("scratch");
        std::fs::create_dir(&tracked).unwrap();
        std::fs::create_dir(&untracked).unwrap();
        std::fs::write(tracked.join("key.txt"), "k\n").unwrap();
        std::fs::write(untracked.join("tmp.txt"), "t\n").unwrap();
        git(&["add", "secret/key.txt"]);
        assert_eq!(git_tracks(&path, &tracked), Ok(true));
        assert_eq!(git_tracks(&path, &untracked), Ok(false));
        assert_eq!(
            none_tracked(&path, &[tracked.clone(), untracked.clone()]),
            Err("hidden_path_tracked")
        );
        assert_eq!(none_tracked(&path, &[untracked]), Ok(()));
    }

    #[test]
    fn a_folder_with_no_repository_is_not_tracked() {
        let root = project();
        let path = root.path().canonicalize().unwrap();
        let folder = path.join("plain");
        std::fs::create_dir(&folder).unwrap();
        assert_eq!(git_tracks(&path, &folder), Ok(false));
        assert_eq!(none_tracked(&path, &[folder]), Ok(()));
    }

    #[test]
    fn a_hostile_fsmonitor_command_never_runs_during_the_check() {
        let root = project();
        let path = root.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            let mut command = std::process::Command::new("/usr/bin/git");
            command
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .current_dir(&path)
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            assert!(crate::spawn_gate::status(&mut command).unwrap().success());
        };
        git(&["init", "-q"]);
        let marker = path.join("pwned");
        let folder = path.join("secret");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("key.txt"), "k\n").unwrap();
        git(&["add", "secret/key.txt"]);
        let hostile = format!("touch {}", marker.display());
        std::fs::write(
            path.join(".git").join("config"),
            format!(
                "[core]\n\trepositoryformatversion = 0\n\tfsmonitor = {hostile}\n\tfsmonitorHookVersion = 1\n"
            ),
        )
        .unwrap();
        let _ = git_tracks(&path, &folder);
        assert!(!marker.exists(), "fsmonitor command ran: marker written");
    }

    #[test]
    fn a_corrupt_index_fails_the_check_closed() {
        let root = project();
        let path = root.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            let mut command = std::process::Command::new("/usr/bin/git");
            command
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .current_dir(&path)
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            assert!(crate::spawn_gate::status(&mut command).unwrap().success());
        };
        git(&["init", "-q"]);
        let folder = path.join("secret");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("key.txt"), "k\n").unwrap();
        git(&["add", "secret/key.txt"]);
        std::fs::write(path.join(".git").join("index"), b"not a git index\n").unwrap();
        assert_eq!(git_tracks(&path, &folder), Err("hidden_path_unchecked"));
        assert_eq!(none_tracked(&path, &[folder]), Err("hidden_path_unchecked"));
    }

    #[test]
    fn a_dot_git_file_fails_the_check_closed() {
        let root = project();
        let path = root.path().canonicalize().unwrap();
        let folder = path.join("secret");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("key.txt"), "k\n").unwrap();
        std::fs::write(path.join(".git"), b"gitdir: /elsewhere/.git/worktrees/x\n").unwrap();
        assert_eq!(git_tracks(&path, &folder), Err("hidden_path_unchecked"));
        assert_eq!(none_tracked(&path, &[folder]), Err("hidden_path_unchecked"));
    }

    #[test]
    fn a_folder_with_more_than_64_kib_of_tracked_names_is_still_tracked() {
        let root = project();
        let path = root.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            let mut command = std::process::Command::new("/usr/bin/git");
            command
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .current_dir(&path)
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            assert!(crate::spawn_gate::status(&mut command).unwrap().success());
        };
        git(&["init", "-q"]);
        let folder = path.join("bulk");
        std::fs::create_dir(&folder).unwrap();
        let mut total = 0usize;
        let mut index = 0usize;
        while total <= CHECK_BYTES_MAX as usize {
            let name = format!("{index:06}-{}.txt", "x".repeat(200));
            std::fs::write(folder.join(&name), "t\n").unwrap();
            total += "bulk/".len() + name.len() + 1;
            index += 1;
        }
        git(&["add", "bulk"]);
        assert_eq!(git_tracks(&path, &folder), Ok(true));
        assert_eq!(none_tracked(&path, &[folder]), Err("hidden_path_tracked"));
    }

    #[test]
    fn saved_folders_read_back_in_order_and_are_empty_without_a_table() {
        let connection = Connection::open_in_memory().unwrap();
        assert!(load(&connection, "app").unwrap().is_empty());
        let paths = vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")];
        save(&connection, "app", &paths).unwrap();
        assert_eq!(load(&connection, "app").unwrap(), paths);
        save(&connection, "app", &[]).unwrap();
        assert!(load(&connection, "app").unwrap().is_empty());
    }
}
