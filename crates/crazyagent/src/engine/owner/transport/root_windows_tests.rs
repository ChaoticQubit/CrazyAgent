use super::Root;
use crazyagent_win_sandbox::private;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A fresh temp folder spelled as Windows names it, with no short name, junction or other
/// case on the way, so a root inside it passes the canonical twin. Output: the temp folder,
/// kept alive by the caller, and its canonical path without the `\\?\` prefix.
fn canonical_temp() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let full = std::fs::canonicalize(temp.path()).unwrap();
    let text = full.to_str().unwrap().to_owned();
    let plain = PathBuf::from(text.strip_prefix(r"\\?\").unwrap());
    (temp, plain)
}

/// A private folder made by W1 at `path`, as the engine makes an owner root. Input: the
/// absolute path, which must not exist yet. Output: the same path.
fn private_folder(path: &Path) -> PathBuf {
    drop(private::dir(path).unwrap());
    path.to_path_buf()
}

/// Runs one Windows command and requires it to succeed. Inputs: the program and its
/// arguments. Output: none; a failure panics with the command's error output.
fn run(program: &str, args: &[&std::ffi::OsStr]) {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let done = crate::spawn_gate::output(&mut command).unwrap();
    assert!(
        done.status.success(),
        "{program}: {}",
        String::from_utf8_lossy(&done.stderr)
    );
}

/// The refusal code an open or verify returned. Input: the result. Output: its code text.
fn refusal<T>(result: Result<T, crate::engine::EngineError>) -> String {
    match result {
        Ok(_) => panic!("the owner root was accepted"),
        Err(error) => error.0,
    }
}

#[test]
fn an_owner_root_opens_and_verifies_on_a_canonical_private_folder() {
    let (_temp, base) = canonical_temp();
    let path = private_folder(&base.join("root"));
    let root = Root::open(&path).expect("a canonical private folder opens as an owner root");
    root.verify().expect("the held root verifies");
    assert_eq!(root.engine_root().unwrap(), base.as_path());
}

#[test]
fn a_held_owner_root_and_its_parent_cannot_be_renamed() {
    let (_temp, base) = canonical_temp();
    let parent = private_folder(&base.join("parent"));
    let path = private_folder(&parent.join("root"));
    let root = Root::open(&path).unwrap();
    assert!(
        std::fs::rename(&path, parent.join("moved")).is_err(),
        "the held root must not be renamed"
    );
    assert!(
        std::fs::rename(&parent, base.join("moved")).is_err(),
        "a folder above the held root must not be renamed either"
    );
    root.verify()
        .expect("the root still verifies after both refused renames");
}

#[test]
fn a_root_replaced_while_no_one_holds_it_fails_verify() {
    let (_temp, base) = canonical_temp();
    let path = private_folder(&base.join("root"));
    let first = Root::open(&path).unwrap();
    let spare = private::dir(&base.join("spare")).unwrap();
    let recorded = Root {
        path: path.clone(),
        directory: spare,
        ..first
    };
    drop(first);
    std::fs::rename(&path, base.join("old")).unwrap();
    private_folder(&path);
    assert_eq!(
        refusal(recorded.verify()),
        "owner_root_replaced_or_unsafe",
        "a new folder at the root's path is not the root that was opened"
    );
}

#[test]
fn a_junction_ancestor_fails_the_canonical_twin() {
    let (_temp, base) = canonical_temp();
    let real = private_folder(&base.join("real"));
    let path = private_folder(&real.join("root"));
    let link = base.join("link");
    run(
        "cmd",
        &[
            "/C".as_ref(),
            "mklink".as_ref(),
            "/J".as_ref(),
            link.as_os_str(),
            real.as_os_str(),
        ],
    );
    assert_eq!(
        refusal(Root::open(&link.join("root"))),
        "owner_root_requires_canonical_trusted_path"
    );
    Root::open(&path).expect("the same root through its real path opens");
}

#[test]
fn another_spelling_of_the_root_fails_the_canonical_twin() {
    let (_temp, base) = canonical_temp();
    private_folder(&base.join("root"));
    assert_eq!(
        refusal(Root::open(&base.join("ROOT"))),
        "owner_root_requires_canonical_trusted_path"
    );
}

#[test]
fn a_missing_root_is_refused_and_not_made() {
    let (_temp, base) = canonical_temp();
    let path = base.join("missing");
    assert!(Root::open(&path).is_err());
    assert!(
        std::fs::symlink_metadata(&path).is_err(),
        "opening an owner root never creates it"
    );
}

#[test]
fn a_root_with_a_foreign_entry_is_refused() {
    let (_temp, base) = canonical_temp();
    let path = private_folder(&base.join("root"));
    run(
        "icacls",
        &[path.as_os_str(), "/grant".as_ref(), "*S-1-1-0:(R)".as_ref()],
    );
    assert_eq!(
        refusal(Root::open(&path)),
        "owner_root_requires_private_owner_directory"
    );
}

#[test]
fn create_directory_makes_one_private_folder_once() {
    let (_temp, base) = canonical_temp();
    let path = private_folder(&base.join("root"));
    let root = Root::open(&path).unwrap();
    root.create_directory("r00")
        .expect("a new folder is made in the root");
    Root::open(&path.join("r00")).expect("the made folder opens as a private root");
    assert!(
        root.create_directory("r00").is_err(),
        "a second create of the same name is refused, as mkdirat refuses it"
    );
}
