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
/// The 8.3 alias Windows gave `long`, read through cmd's `%~s` form, which asks
/// GetShortPathNameW, so a row names a short spelling it made on purpose (CR-1341 HIGH 1).
/// Input: an existing folder with a long name. Output: the alias, or None when this volume
/// makes no 8.3 names, which the row then reports as a named SKIP.
fn short_alias(long: &Path) -> Option<PathBuf> {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new("cmd");
    command
        .raw_arg(format!(
            "/D /C for %I in (\"{}\") do @echo %~sI",
            long.display()
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let done = crate::spawn_gate::output(&mut command).unwrap();
    assert!(done.status.success(), "{done:?}");
    let short = PathBuf::from(String::from_utf8(done.stdout).unwrap().trim());
    assert!(short.is_absolute(), "{}", short.display());
    (short != long && short.to_str()?.contains('~')).then_some(short)
}

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
    drop(root);
    std::fs::rename(&path, parent.join("moved"))
        .expect("the same rename works once the root is no longer held");
    std::fs::rename(&parent, base.join("moved"))
        .expect("the same rename of the folder above works once nothing below it is held");
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

#[test]
fn a_root_syncs_its_entries_through_a_write_handle() {
    let (_temp, base) = canonical_temp();
    let path = private_folder(&base.join("root"));
    let root = Root::open(&path).unwrap();
    root.create_directory("r00").unwrap();
    root.sync()
        .expect("the held root flushes, though its own handle only lists the folder");
    root.verify().unwrap();
}

#[test]
fn a_root_spelled_as_canonicalize_spells_it_opens_and_a_junction_still_fails() {
    let (_temp, base) = canonical_temp();
    let real = private_folder(&base.join("real"));
    let path = private_folder(&real.join("root"));
    let spelled = std::fs::canonicalize(&path).unwrap();
    assert!(spelled.to_str().unwrap().starts_with(r"\\?\"));
    let root = Root::open(&spelled).expect("the verbatim spelling of the canonical root opens");
    assert_eq!(
        root.path, path,
        "a verbatim root is stored in its one plain spelling (rv-sec2 #717 C1)"
    );
    root.verify().unwrap();
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
    let mut through = std::ffi::OsString::from(r"\\?\");
    through.push(link.join("root").as_os_str());
    assert_eq!(
        refusal(Root::open(Path::new(&through))),
        "owner_root_requires_canonical_trusted_path"
    );
}

#[test]
fn a_short_name_root_and_a_verbatim_unc_root_are_refused() {
    let temp = tempfile::tempdir().unwrap();
    let full = std::fs::canonicalize(temp.path()).unwrap();
    let plain = PathBuf::from(full.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
    let long = plain.join("a long root folder name");
    std::fs::create_dir(&long).unwrap();
    let path = private_folder(&long.join("root"));
    Root::open(&path).expect("the root opens at its plain long name");
    match short_alias(&long) {
        Some(alias) => assert_eq!(
            refusal(Root::open(&alias.join("root"))),
            "owner_root_requires_canonical_trusted_path"
        ),
        None => println!("SKIP a_short_name_root: this volume makes no 8.3 names"),
    }
    let text = path.to_str().unwrap();
    let unc = format!(r"\\?\UNC\localhost\{}$\{}", &text[..1], &text[3..]);
    assert_eq!(
        refusal(Root::open(Path::new(&unc))),
        "owner_root_requires_canonical_trusted_path",
        "a verbatim UNC root is refused by its spelling, before any open"
    );
}
