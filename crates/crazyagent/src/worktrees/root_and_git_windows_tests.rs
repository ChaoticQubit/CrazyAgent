use super::{git, private_worktree_root};
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
