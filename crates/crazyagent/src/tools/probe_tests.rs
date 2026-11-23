use kernel_processes::finding::test_support::*;
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// A scratch folder no other user can change, its real path, for rows that need a folder
/// kept: the probe drops every folder others can change, and a plain temp folder sits in
/// the shared `/tmp` on Linux. Output: the folder and its real path.
fn private_scratch() -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::Builder::new()
        .prefix("tools-probe-")
        .tempdir_in(crate::harnesses::finding::private_root())
        .unwrap();
    let real = folder.path().canonicalize().unwrap();
    assert_eq!(kernel_processes::service::changeable_by_others(&real), None);
    (folder, real)
}

fn joined(folders: &[PathBuf]) -> OsString {
    std::env::join_paths(folders).unwrap()
}

#[test]
fn kept_folders_drop_relative_missing_repeated_shared_crazyagent_and_project_folders() {
    let (_folder, home) = private_scratch();
    let tools = home.join("tools");
    let project = home.join("projects/app");
    let shared = home.join("shared");
    let inside = home.join("x/.crazyagent/bin");
    for folder in [&tools, &project.join(".crazyagent"), &shared, &inside] {
        std::fs::create_dir_all(folder).unwrap();
    }
    std::fs::create_dir_all(project.join("node_modules/.bin")).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
    let file = home.join("plain-file");
    std::fs::write(&file, "x").unwrap();
    let path = joined(&[
        tools.clone(),
        PathBuf::from("relative/bin"),
        home.join("missing"),
        tools.clone(),
        project.join("node_modules/.bin"),
        project.clone(),
        inside,
        shared,
        file,
    ]);
    assert_eq!(kept(&crate::tools::HOOKS, &path, Some(&home)), [tools]);
    assert_eq!(
        kept(&crate::tools::HOOKS, OsStr::new(""), Some(&home)),
        Vec::<PathBuf>::new()
    );
    std::fs::create_dir_all(home.join(".crazyagent")).unwrap();
    assert_eq!(
        kept(
            &crate::tools::HOOKS,
            &joined(&[home.join("tools")]),
            Some(&home)
        ),
        [home.join("tools")],
        "a .crazyagent folder in HOME itself makes no project of HOME"
    );
    let many: Vec<PathBuf> = (0..FOLDERS_MAX + 10)
        .map(|index| {
            let folder = home.join(format!("many/{index}"));
            std::fs::create_dir_all(&folder).unwrap();
            folder
        })
        .collect();
    assert_eq!(
        kept(&crate::tools::HOOKS, &joined(&many), Some(&home)).len(),
        FOLDERS_MAX
    );
}
