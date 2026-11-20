use super::{bounded_read, git, git_program_in, identity, inspect};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn plain_temp() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let full = std::fs::canonicalize(temp.path()).unwrap();
    let base = PathBuf::from(full.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
    (temp, base)
}

fn run(program: &str, args: &[&std::ffi::OsStr]) {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let done = crate::spawn_gate::output(&mut command).unwrap();
    assert!(done.status.success(), "{program} {args:?}: {done:?}");
}

fn grant(path: &Path, rule: &str) {
    run(
        "icacls",
        &[path.as_os_str(), "/grant".as_ref(), rule.as_ref()],
    );
}

/// The `git.cmd` npm 11's cmd-shim writes for a package `git` whose bin is `bin\\git.js`, the
/// Codex shim of tools/batch.rs's rows with its target renamed.
const NPM_SHIM: &str = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\git\\bin\\git.js\" %*\r\n";

fn code<T: std::fmt::Debug>(result: Result<T, crate::engine::EngineError>) -> String {
    result.unwrap_err().0
}

#[test]
fn a_plain_folder_and_its_one_named_file_have_a_stable_identity() {
    let (_temp, base) = plain_temp();
    let file = base.join("file");
    std::fs::write(&file, b"x").unwrap();
    let folder = identity(&base, true).map_err(|refusal| refusal.0).unwrap();
    let named = identity(&file, false).map_err(|refusal| refusal.0).unwrap();
    assert_ne!(
        folder, named,
        "a folder and a file in it are two identities (W3-5 D1)"
    );
    assert_eq!(
        identity(&base, true).unwrap(),
        folder,
        "one folder keeps one identity"
    );
}

#[test]
fn a_junction_a_hard_linked_file_and_a_kind_mismatch_are_untrusted() {
    let (_temp, base) = plain_temp();
    let target = base.join("target");
    std::fs::create_dir(&target).unwrap();
    let link = base.join("link");
    run(
        "cmd",
        &[
            "/C".as_ref(),
            "mklink".as_ref(),
            "/J".as_ref(),
            link.as_os_str(),
            target.as_os_str(),
        ],
    );
    let file = base.join("file");
    std::fs::write(&file, b"x").unwrap();
    std::fs::hard_link(&file, base.join("twin")).unwrap();
    let single = base.join("single");
    std::fs::write(&single, b"x").unwrap();
    for (case, refused) in [
        ("a junction", code(identity(&link, true))),
        ("a hard-linked file", code(identity(&file, false))),
        ("a file read as a folder", code(identity(&single, true))),
        ("a folder read as a file", code(identity(&target, false))),
    ] {
        assert_eq!(refused, "untrusted_filesystem_identity", "{case} (W3-5 D1)");
    }
}

#[test]
fn a_folder_or_file_another_account_or_any_app_container_can_change_is_untrusted() {
    let (_temp, base) = plain_temp();
    for (name, rule) in [
        ("everyone", "*S-1-1-0:(W)"),
        ("authenticated", "*S-1-5-11:(OI)(CI)M"),
        ("packages", "*S-1-15-2-1:(OI)(CI)M"),
    ] {
        let folder = base.join(name);
        std::fs::create_dir(&folder).unwrap();
        let file = base.join(format!("{name}.txt"));
        std::fs::write(&file, b"x").unwrap();
        grant(&folder, rule);
        grant(&file, &rule.replace("(OI)(CI)", ""));
        assert_eq!(
            code(identity(&folder, true)),
            "untrusted_filesystem_identity",
            "folder {rule} (rv-sec2 #742 Q2)"
        );
        assert_eq!(
            code(identity(&file, false)),
            "untrusted_filesystem_identity",
            "file {rule} (rv-sec2 #742 Q2)"
        );
    }
}

#[test]
fn a_plain_repository_is_admitted_with_its_root_and_git_folder_identities() {
    let (_temp, base) = plain_temp();
    let repository = base.join("repo");
    std::fs::create_dir(&repository).unwrap();
    git(&repository, &["init", "-q"])
        .map_err(|refusal| refusal.0)
        .unwrap();
    let admitted = inspect(repository.to_str().unwrap())
        .map_err(|refusal| refusal.0)
        .unwrap();
    assert_eq!(
        admitted.root,
        identity(&repository, true).unwrap(),
        "W3-5 D1 D8"
    );
    assert_eq!(
        admitted.metadata,
        identity(&repository.join(".git"), true).unwrap()
    );
}

#[test]
fn engine_git_refuses_a_batch_git_and_a_git_another_account_can_change() {
    let (_temp, base) = plain_temp();
    let batch = base.join("batch");
    std::fs::create_dir(&batch).unwrap();
    std::fs::write(batch.join("git.cmd"), b"@echo off\r\n").unwrap();
    if let Ok(found) = git_program_in(Some(batch.as_os_str())) {
        assert!(
            found
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe")),
            "a git.cmd would run the engine's arguments through cmd.exe (rv-sec2 #742 D5-1): {found:?}"
        );
    }
    let shim = base.join("shim");
    let script = shim.join("node_modules").join("git").join("bin");
    std::fs::create_dir_all(&script).unwrap();
    std::fs::write(script.join("git.js"), b"").unwrap();
    std::fs::write(shim.join("git.cmd"), NPM_SHIM).unwrap();
    assert_eq!(
        code(git_program_in(Some(shim.as_os_str()))),
        "worktree_git_untrusted",
        "an npm git.cmd the resolver maps to its script is refused: engine git is an .exe (rv-sec2 #742 D5-1)"
    );
    let real = git_program_in(std::env::var_os("PATH").as_deref()).unwrap();
    let open = base.join("open");
    std::fs::create_dir(&open).unwrap();
    std::fs::copy(&real, open.join("git.exe")).unwrap();
    grant(&open, "*S-1-1-0:(OI)(CI)M");
    assert_eq!(
        code(git_program_in(Some(open.as_os_str()))),
        "worktree_git_untrusted",
        "a git in a folder everyone can change is refused (rv-sec2 #742 D5-2)"
    );
}

#[test]
fn a_bounded_read_keeps_a_plain_file_and_refuses_a_link_a_folder_and_an_oversized_file() {
    let (_temp, base) = plain_temp();
    let file = base.join("file");
    std::fs::write(&file, b"abc").unwrap();
    let link = base.join("link");
    std::os::windows::fs::symlink_file(&file, &link).unwrap();
    assert_eq!(
        bounded_read(&file, 3).map_err(|refusal| refusal.0).unwrap(),
        b"abc",
        "W3-5 D3"
    );
    assert_eq!(code(bounded_read(&file, 2)), "file_capacity");
    assert_eq!(
        code(bounded_read(&link, 3)),
        "expected_regular_file",
        "a link is never followed"
    );
    assert_eq!(code(bounded_read(&base, 3)), "expected_regular_file");
}
