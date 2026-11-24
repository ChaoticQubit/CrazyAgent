#![cfg(unix)]
use super::*;

/// An executable to link and an empty bin folder under one durable folder. Output: the
/// folder, the executable and the bin folder.
fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = durable_folder();
    let executable = temp.path().join("app/crazyagent");
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::write(&executable, "#!/bin/sh\n").unwrap();
    let bin = temp.path().join("home/.local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    (temp, executable, bin)
}

/// The names in a folder, sorted. Input: the folder. Output: the names.
fn names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// An absent name gets the link, and a link already there is replaced, with nothing staged
/// left behind.
#[test]
fn an_absent_name_gets_the_link_and_a_link_there_is_replaced() {
    let (temp, executable, bin) = setup();
    let link = link_cli(&executable, &bin).unwrap();
    assert_eq!(std::fs::read_link(&link).unwrap(), executable);
    let newer = temp.path().join("newer/crazyagent");
    std::fs::create_dir_all(newer.parent().unwrap()).unwrap();
    std::fs::write(&newer, "#!/bin/sh\n").unwrap();
    assert_eq!(link_cli(&newer, &bin).unwrap(), link);
    assert_eq!(std::fs::read_link(&link).unwrap(), newer);
    assert_eq!(names(&bin), vec!["crazyagent"]);
}

/// A regular file written at an absent name after the check keeps its bytes, and the
/// publish is refused as for a file there before.
#[test]
fn a_file_written_at_an_absent_name_after_the_check_is_kept() {
    let (_temp, executable, bin) = setup();
    cli_link::before_publish(|link| std::fs::write(link, "user file").unwrap());
    let error = link_cli(&executable, &bin).unwrap_err();
    assert!(error.0.contains("not a link"), "{error}");
    let link = bin.join("crazyagent");
    assert_eq!(std::fs::read_to_string(&link).unwrap(), "user file");
    assert_eq!(names(&bin), vec!["crazyagent"]);
}

/// A regular file that replaces the old link after the check keeps its bytes, and the
/// publish is refused.
#[test]
fn a_file_that_replaces_the_link_after_the_check_is_kept() {
    let (_temp, executable, bin) = setup();
    let link = link_cli(&executable, &bin).unwrap();
    cli_link::before_publish(|link| {
        std::fs::remove_file(link).unwrap();
        std::fs::write(link, "user file").unwrap();
    });
    let error = link_cli(&executable, &bin).unwrap_err();
    assert!(error.0.contains("not a link"), "{error}");
    assert_eq!(std::fs::read_to_string(&link).unwrap(), "user file");
    assert_eq!(names(&bin), vec!["crazyagent"]);
}
