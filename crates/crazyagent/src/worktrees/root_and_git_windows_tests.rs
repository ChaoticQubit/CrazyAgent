use super::{git, private_worktree_root, refuse_old_git};
use std::path::PathBuf;

fn plain_temp() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let full = std::fs::canonicalize(temp.path()).unwrap();
    let base = PathBuf::from(full.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
    (temp, base)
}

#[test]
fn the_private_worktree_root_is_a_w1_private_folder_in_the_runtime_folder() {
    let (_temp, runtime) = plain_temp();
    for round in ["made", "opened"] {
        let root = private_worktree_root(&runtime)
            .map_err(|refusal| refusal.0)
            .unwrap();
        assert_eq!(root, runtime.join("worktrees"), "{round}");
        crazyagent_win_sandbox::private::open_dir(&root)
            .unwrap_or_else(|e| panic!("the {round} root is W1 private (W3-5 D4): {e}"));
    }
}

#[test]
fn engine_git_runs_on_windows_through_the_one_git_resolver() {
    let (_temp, repository) = plain_temp();
    let printed = git(&repository, &["version"])
        .map_err(|refusal| refusal.0)
        .unwrap();
    assert!(
        printed.starts_with(b"git version "),
        "engine git runs on Windows (W3-5 D5): {}",
        String::from_utf8_lossy(&printed)
    );
}

#[test]
fn git_for_windows_meets_the_agent_worktree_floor_and_its_version_line_reads_as_numbers() {
    let (_temp, repository) = plain_temp();
    refuse_old_git(&repository)
        .map_err(|refusal| refusal.0)
        .unwrap();
    assert_eq!(
        crate::landing::version_numbers(b"git version 2.47.1.windows.2\n".to_vec()),
        [2, 47, 1],
        "a Git for Windows version line reads as its first three numbers (W3-5 D5)"
    );
}

/// A git config file a row planted lines in, put back as it was when the row ends, the
/// file removed when there was none. Holds the path and its bytes before the row.
struct Planted(PathBuf, Option<Vec<u8>>);

impl Drop for Planted {
    fn drop(&mut self) {
        let _ = match &self.1 {
            Some(before) => std::fs::write(&self.0, before),
            None => std::fs::remove_file(&self.0),
        };
    }
}

/// The bytes a planted config file held before the row: none when there was no file, and
/// the read's error for any other failure, so a file that exists is never taken as absent
/// and removed when the row ends (CR group 1 W3-5a). Input: the path. Output: the bytes.
fn snapshot(path: &std::path::Path) -> std::io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn plant(path: PathBuf, lines: &str) -> Planted {
    let before = snapshot(&path).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = before.clone().unwrap_or_default();
    bytes.extend_from_slice(lines.as_bytes());
    std::fs::write(&path, bytes).unwrap();
    Planted(path, before)
}

#[test]
fn a_planted_config_snapshot_is_absent_only_for_a_missing_file() {
    let (_temp, base) = plain_temp();
    assert_eq!(snapshot(&base.join("missing")).unwrap(), None);
    std::fs::write(base.join("config"), b"[core]\n").unwrap();
    assert_eq!(
        snapshot(&base.join("config")).unwrap().as_deref(),
        Some(&b"[core]\n"[..])
    );
    std::fs::create_dir(base.join("folder")).unwrap();
    assert!(
        snapshot(&base.join("folder")).is_err(),
        "an entry that exists but cannot be read is an error, never absent (CR group 1 W3-5a)"
    );
}

#[test]
fn engine_git_reads_neither_the_system_nor_the_user_git_config_on_windows() {
    if std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true") {
        eprintln!("skipped: this row writes the machine's git config, so it runs on the farm only");
        return;
    }
    let (_temp, repository) = plain_temp();
    git(&repository, &["init", "-q"])
        .map_err(|refusal| refusal.0)
        .unwrap();
    let system = PathBuf::from(std::env::var_os("ProgramData").unwrap())
        .join("Git")
        .join("config");
    let global = PathBuf::from(std::env::var_os("USERPROFILE").unwrap()).join(".gitconfig");
    let _system = plant(
        system,
        "\n[w35]\n\tplanted = system\n[alias]\n\tw35 = !echo planted\n",
    );
    let _global = plant(
        global,
        "\n[w35]\n\tplanted = global\n[core]\n\thooksPath = C:/w35-hooks\n",
    );
    let listed = git(&repository, &["config", "--list", "--show-origin"])
        .map_err(|refusal| refusal.0)
        .unwrap();
    let text = String::from_utf8_lossy(&listed);
    assert!(
        !text.contains("w35"),
        "no system or user config reaches engine git (rv-sec2 #742 D5-3): {text}"
    );
}
