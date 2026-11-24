use kernel_processes::finding::test_support::*;
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Names the path a filesystem step failed on. Inputs: the path and the step's result.
/// Output: the value, or an error naming the path.
fn at<T>(path: &Path, result: std::io::Result<T>) -> TestResult<T> {
    result.map_err(|error| format!("{path:?}: {error}").into())
}

/// A scratch folder no other user can change, its real path, for rows that need a folder
/// kept: the probe drops every folder others can change, and a plain temp folder sits in
/// the shared `/tmp` on Linux. Output: the folder and its real path.
fn private_scratch() -> TestResult<(tempfile::TempDir, PathBuf)> {
    let root = crate::harnesses::finding::private_root();
    let folder = at(
        &root,
        tempfile::Builder::new()
            .prefix("tools-probe-")
            .tempdir_in(&root),
    )?;
    let real = at(folder.path(), folder.path().canonicalize())?;
    assert_eq!(kernel_processes::service::changeable_by_others(&real), None);
    Ok((folder, real))
}

fn joined(folders: &[PathBuf]) -> OsString {
    std::env::join_paths(folders).unwrap()
}

#[test]
fn kept_folders_drop_relative_missing_repeated_shared_crazyagent_and_project_folders() -> TestResult
{
    let (_folder, home) = private_scratch()?;
    let tools = home.join("tools");
    let project = home.join("projects/app");
    let shared = home.join("shared");
    let inside = home.join("x/.crazyagent/bin");
    for folder in [&tools, &project.join(".crazyagent"), &shared, &inside] {
        at(folder, std::fs::create_dir_all(folder))?;
    }
    let bin = project.join("node_modules/.bin");
    at(&bin, std::fs::create_dir_all(&bin))?;
    at(
        &shared,
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)),
    )?;
    let file = home.join("plain-file");
    at(&file, std::fs::write(&file, "x"))?;
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
    let own = home.join(".crazyagent");
    at(&own, std::fs::create_dir_all(&own))?;
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
            at(&folder, std::fs::create_dir_all(&folder))?;
            Ok(folder)
        })
        .collect::<TestResult<_>>()?;
    assert_eq!(
        kept(&crate::tools::HOOKS, &joined(&many), Some(&home)).len(),
        FOLDERS_MAX
    );
    Ok(())
}

/// The engine's hooks turn folder runs off in crazyagent's own unit rows, so no row runs the
/// developer's shell files, and on in every other build, the engine a `cargo test` launches
/// included.
#[test]
fn the_engine_hooks_turn_folder_runs_off_only_in_its_own_unit_rows() {
    assert!(!crate::tools::HOOKS.folder_probes);
}
