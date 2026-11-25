#![cfg(unix)]
use super::*;
use crate::update::test_support::{TestResult, at};

/// An executable to link and an empty bin folder under one durable folder. Output: the
/// folder, the executable and the bin folder, or the folder step that failed.
fn setup() -> TestResult<(tempfile::TempDir, PathBuf, PathBuf)> {
    let temp = durable_folder();
    let executable = temp.path().join("app/crazyagent");
    let app = temp.path().join("app");
    at(&app, std::fs::create_dir_all(&app))?;
    at(&executable, std::fs::write(&executable, "#!/bin/sh\n"))?;
    let bin = temp.path().join("home/.local/bin");
    at(&bin, std::fs::create_dir_all(&bin))?;
    Ok((temp, executable, bin))
}

/// The names in a folder, sorted. Input: the folder. Output: the names, or the listing
/// step that failed, naming the folder.
fn names(folder: &Path) -> TestResult<Vec<String>> {
    let mut names = at(folder, std::fs::read_dir(folder))?
        .map(|entry| {
            Ok(at(folder, entry)?
                .file_name()
                .to_string_lossy()
                .into_owned())
        })
        .collect::<TestResult<Vec<String>>>()?;
    names.sort();
    Ok(names)
}

/// An absent name gets the link, and a link already there is replaced, with nothing staged
/// left behind.
#[test]
fn an_absent_name_gets_the_link_and_a_link_there_is_replaced() -> TestResult {
    let (temp, executable, bin) = setup()?;
    let link = link_cli(&executable, &bin).unwrap();
    assert_eq!(at(&link, std::fs::read_link(&link))?, executable);
    let newer = temp.path().join("newer/crazyagent");
    let folder = temp.path().join("newer");
    at(&folder, std::fs::create_dir_all(&folder))?;
    at(&newer, std::fs::write(&newer, "#!/bin/sh\n"))?;
    assert_eq!(link_cli(&newer, &bin).unwrap(), link);
    assert_eq!(at(&link, std::fs::read_link(&link))?, newer);
    assert_eq!(names(&bin)?, vec!["crazyagent"]);
    Ok(())
}

/// A regular file written at an absent name after the check keeps its bytes, and the
/// publish is refused as for a file there before.
#[test]
fn a_file_written_at_an_absent_name_after_the_check_is_kept() -> TestResult {
    let (_temp, executable, bin) = setup()?;
    cli_link::before_publish(|link| std::fs::write(link, "user file").unwrap());
    let error = link_cli(&executable, &bin).unwrap_err();
    assert!(error.0.contains("not a link"), "{error}");
    let link = bin.join("crazyagent");
    assert_eq!(at(&link, std::fs::read_to_string(&link))?, "user file");
    assert_eq!(names(&bin)?, vec!["crazyagent"]);
    Ok(())
}

/// A regular file that replaces the old link after the check keeps its bytes, and the
/// publish is refused.
#[test]
fn a_file_that_replaces_the_link_after_the_check_is_kept() -> TestResult {
    let (_temp, executable, bin) = setup()?;
    let link = link_cli(&executable, &bin).unwrap();
    cli_link::before_publish(|link| {
        std::fs::remove_file(link).unwrap();
        std::fs::write(link, "user file").unwrap();
    });
    let error = link_cli(&executable, &bin).unwrap_err();
    assert!(error.0.contains("not a link"), "{error}");
    assert_eq!(at(&link, std::fs::read_to_string(&link))?, "user file");
    assert_eq!(names(&bin)?, vec!["crazyagent"]);
    Ok(())
}
