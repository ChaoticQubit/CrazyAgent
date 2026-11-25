use kernel_processes::finding::permission_fix::*;
use kernel_processes::finding::test_support::*;
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Names the path a filesystem step failed on. Inputs: the path and the step's result.
/// Output: the value, or an error naming the path.
fn at<T>(path: &Path, result: std::io::Result<T>) -> TestResult<T> {
    result.map_err(|error| format!("{path:?}: {error}").into())
}

fn mode(path: &Path) -> TestResult<u32> {
    Ok(at(path, std::fs::symlink_metadata(path))?
        .permissions()
        .mode()
        & 0o7777)
}

fn set(path: &Path, mode: u32) -> TestResult {
    at(
        path,
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)),
    )
}

fn file(path: &Path, mode: u32) -> TestResult {
    let parent = path.parent().unwrap();
    at(parent, std::fs::create_dir_all(parent))?;
    at(path, std::fs::write(path, b"#!/bin/sh\nexit 0\n"))?;
    set(path, mode)
}

fn folder(path: &Path, mode: u32) -> TestResult {
    at(path, std::fs::create_dir_all(path))?;
    set(path, mode)
}

/// A fixture HOME in its own temp folder, with a uv-like Python prefix five folders below
/// it whose interpreter and folders are group writable. Output: the temp folder, HOME and
/// the prefix.
fn uv_like() -> TestResult<(tempfile::TempDir, PathBuf, PathBuf)> {
    let scratch = tempfile::tempdir()?;
    let home = scratch.path().join("home");
    let prefix = home.join(".local/share/uv/python/cpython-9.9.9-dummy");
    file(&prefix.join("bin/python3"), 0o775)?;
    folder(&prefix.join("bin"), 0o775)?;
    folder(&prefix, 0o775)?;
    Ok((scratch, home, prefix))
}

fn whole(path: &Path) -> Target {
    Target {
        path: path.to_path_buf(),
        recursive: true,
    }
}

fn only(path: &Path) -> Target {
    Target {
        path: path.to_path_buf(),
        recursive: false,
    }
}

#[test]
fn the_fix_refuses_home_itself_a_place_outside_home_and_engine_or_project_trees() -> TestResult {
    let (scratch, home, prefix) = uv_like()?;
    let outside = scratch.path().join("elsewhere");
    folder(&outside, 0o777)?;
    let project = home.join("code/app");
    folder(&project.join(".crazyagent"), 0o755)?;
    let project_bin = project.join("bin");
    folder(&project_bin, 0o777)?;
    let crazyagent = home.join("work/.crazyagent/tool");
    folder(&crazyagent, 0o777)?;
    let places = crate::tools::tools_new(Vec::new(), Some(home.clone()));

    for refused in [&home, &outside, &crazyagent, &project_bin] {
        assert_eq!(fix(&whole(refused), &places), Err(TOOL_FIX_REFUSED));
        assert_eq!(fix(&only(refused), &places), Err(TOOL_FIX_REFUSED));
    }
    assert_eq!(mode(&outside)?, 0o777);
    assert_eq!(mode(&crazyagent)?, 0o777);
    assert_eq!(mode(&project_bin)?, 0o777);
    assert!(!allowed(Path::new("/opt/homebrew/opt/python"), &places));
    assert!(!allowed(Path::new("/usr/local/Cellar/python"), &places));
    assert!(!allowed(Path::new("/"), &places));
    assert!(allowed(&prefix, &places));

    let root = home.join("engine");
    let engine =
        crate::tools::tools_engine(Some(&root), None, Some(home.clone()), Some(OsStr::new("")));
    let setup_python = root.join("setup/uv/python/cpython-9.9.9-dummy");
    folder(&setup_python, 0o775)?;
    assert!(!allowed(&setup_python, &engine));
    assert!(!allowed(&root.join("agents/one"), &engine));
    assert_eq!(fix(&whole(&setup_python), &engine), Err(TOOL_FIX_REFUSED));
    assert_eq!(mode(&setup_python)?, 0o775);
    Ok(())
}
