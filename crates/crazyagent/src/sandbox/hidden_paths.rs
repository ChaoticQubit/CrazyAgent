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
    if text.len() > PATH_TEXT_MAX
        || !typed.is_absolute()
        || text
            .chars()
            .any(|c| c.is_control() || c == '"' || c == '\\' || c == '\'')
    {
        return Err("hidden_path_not_absolute");
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

/// Whether git tracks any file inside this folder, asked in the project's own checkout. A
/// hidden folder covers only files git does not track; tracked files are copied into each
/// agent's worktree and live in history, so hiding one would be a false promise. Run outside
/// any sandbox. Inputs: the project root and the canonical folder. Output: true when tracked;
/// false when nothing is tracked or git cannot answer (no repository, say).
pub fn git_tracks(project_root: &Path, folder: &Path) -> bool {
    let root = canonical(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    let real = canonical(folder).unwrap_or_else(|_| folder.to_path_buf());
    let Ok(relative) = real.strip_prefix(&root) else {
        return false;
    };
    let git = if cfg!(unix) { "/usr/bin/git" } else { "git" };
    let mut command = std::process::Command::new(git);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .current_dir(&root)
        .args(["ls-files", "-z", "--"])
        .arg(relative)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    match crate::spawn_gate::output(&mut command) {
        Ok(output) if output.status.success() => !output.stdout.is_empty(),
        _ => false,
    }
}

/// Refuses a list that holds a folder git tracks. Inputs: the project root and the checked
/// folders. Output: ok, or the plain code for the first tracked folder.
pub fn none_tracked(project_root: &Path, folders: &[PathBuf]) -> Result<(), &'static str> {
    for folder in folders {
        if git_tracks(project_root, folder) {
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
        for bad in ["/tmp/a\"b", "/tmp/a\\b", "/tmp/a'b"] {
            assert_eq!(checked(bad, root.path()), Err("hidden_path_not_absolute"));
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
        assert!(git_tracks(&path, &tracked));
        assert!(!git_tracks(&path, &untracked));
        assert_eq!(
            none_tracked(&path, &[tracked.clone(), untracked.clone()]),
            Err("hidden_path_tracked")
        );
        assert_eq!(none_tracked(&path, &[untracked]), Ok(()));
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
