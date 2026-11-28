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

/// A link removed after the check leaves the name absent, and the next round makes the
/// link there in place, with nothing staged left behind.
#[test]
fn a_link_removed_after_the_check_is_made_again_in_place() -> TestResult {
    let (_temp, executable, bin) = setup()?;
    let link = link_cli(&executable, &bin).unwrap();
    cli_link::before_publish(|link| std::fs::remove_file(link).unwrap());
    assert_eq!(link_cli(&executable, &bin).unwrap(), link);
    assert_eq!(at(&link, std::fs::read_link(&link))?, executable);
    assert_eq!(names(&bin)?, vec!["crazyagent"]);
    Ok(())
}

/// A regular file at the staged name keeps its bytes, the link already there is left
/// as it was, and the publish is refused.
#[test]
fn a_file_at_the_staged_name_is_kept_and_the_link_there_is_left() -> TestResult {
    let (temp, executable, bin) = setup()?;
    let link = link_cli(&executable, &bin).unwrap();
    let staged = bin.join(format!(".crazyagent.link.{}", std::process::id()));
    at(&staged, std::fs::write(&staged, "user file"))?;
    let newer = temp.path().join("app/crazyagent-newer");
    at(&newer, std::fs::write(&newer, "#!/bin/sh\n"))?;
    let error = link_cli(&newer, &bin).unwrap_err();
    assert!(error.0.contains(&staged.display().to_string()), "{error}");
    assert_eq!(at(&staged, std::fs::read_to_string(&staged))?, "user file");
    assert_eq!(at(&link, std::fs::read_link(&link))?, executable);
    Ok(())
}

/// A link left at the staged name by an earlier run is replaced, and the publish goes on
/// with nothing staged left behind.
#[test]
fn a_link_left_at_the_staged_name_is_replaced_and_the_publish_goes_on() -> TestResult {
    let (temp, executable, bin) = setup()?;
    let link = link_cli(&executable, &bin).unwrap();
    let staged = bin.join(format!(".crazyagent.link.{}", std::process::id()));
    at(&staged, std::os::unix::fs::symlink("/nowhere", &staged))?;
    let newer = temp.path().join("app/crazyagent-newer");
    at(&newer, std::fs::write(&newer, "#!/bin/sh\n"))?;
    assert_eq!(link_cli(&newer, &bin).unwrap(), link);
    assert_eq!(at(&link, std::fs::read_link(&link))?, newer);
    assert_eq!(names(&bin)?, vec!["crazyagent"]);
    Ok(())
}

/// The inode and change time `lstat` reads of a name, which a swap or a remake changes.
/// Input: the name. Output: the pair, or the read that failed, naming the path.
fn stamp_of(name: &Path) -> TestResult<(u64, i64, i64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = at(name, std::fs::symlink_metadata(name))?;
    Ok((metadata.ino(), metadata.ctime(), metadata.ctime_nsec()))
}

/// A link this user owns that already names the executable, by the same bytes, is left as
/// it is: nothing is made, swapped or removed, and nothing is staged.
#[test]
fn a_link_that_already_names_the_executable_is_left_unwritten() -> TestResult {
    let (_temp, executable, bin) = setup()?;
    let link = link_cli(&executable, &bin).unwrap();
    let before = stamp_of(&link)?;
    assert_eq!(link_cli(&executable, &bin).unwrap(), link);
    assert_eq!(stamp_of(&link)?, before);
    assert_eq!(at(&link, std::fs::read_link(&link))?, executable);
    assert_eq!(names(&bin)?, vec!["crazyagent".to_string()]);
    Ok(())
}

/// A link that names another executable is swapped for one that names this executable.
#[test]
fn a_link_naming_another_executable_is_replaced() -> TestResult {
    let (temp, executable, bin) = setup()?;
    let other = temp.path().join("app/other");
    at(&other, std::fs::write(&other, "#!/bin/sh\n"))?;
    let link = bin.join("crazyagent");
    at(&link, std::os::unix::fs::symlink(&other, &link))?;
    let before = stamp_of(&link)?;
    assert_eq!(link_cli(&executable, &bin).unwrap(), link);
    assert_eq!(at(&link, std::fs::read_link(&link))?, executable);
    assert_ne!(stamp_of(&link)?.0, before.0);
    assert_eq!(names(&bin)?, vec!["crazyagent".to_string()]);
    Ok(())
}

/// A link that names the executable by another spelling of the same path is not the same
/// bytes, so it is swapped for one that names the executable exactly.
#[test]
fn a_link_naming_the_executable_by_another_spelling_is_swapped() -> TestResult {
    let (temp, executable, bin) = setup()?;
    let spelled = temp.path().join("app/./crazyagent");
    let link = bin.join("crazyagent");
    at(&link, std::os::unix::fs::symlink(&spelled, &link))?;
    let before = stamp_of(&link)?;
    assert_eq!(link_cli(&executable, &bin).unwrap(), link);
    assert_eq!(
        at(&link, std::fs::read_link(&link))?.as_os_str(),
        executable.as_os_str()
    );
    assert_ne!(stamp_of(&link)?.0, before.0);
    Ok(())
}

/// A link that names the executable but turns into a regular file between the check and the
/// read of what it names is refused, and the file is kept as it was.
#[test]
fn a_link_whose_name_turns_into_a_file_before_the_read_is_refused_and_kept() -> TestResult {
    let (_temp, executable, bin) = setup()?;
    let link = link_cli(&executable, &bin).unwrap();
    cli_link::before_publish(|name| {
        std::fs::remove_file(name).unwrap();
        std::fs::write(name, "another installer\n").unwrap();
    });
    assert!(link_cli(&executable, &bin).is_err());
    assert_eq!(
        at(&link, std::fs::read_to_string(&link))?,
        "another installer\n"
    );
    assert_eq!(names(&bin)?, vec!["crazyagent".to_string()]);
    Ok(())
}

/// A link that names the executable but carries an access list letting another user change
/// it is swapped for a new link of this user, though it names the right file.
#[cfg(target_os = "macos")]
#[test]
fn a_link_with_an_access_list_for_others_is_swapped_for_a_new_one() -> TestResult {
    let (_temp, executable, bin) = setup()?;
    let link = link_cli(&executable, &bin).unwrap();
    let status = crate::spawn_gate::status(
        std::process::Command::new("/bin/chmod")
            .args([
                "-h",
                "+a",
                "group:staff allow writeattr,writeextattr,delete",
            ])
            .arg(&link),
    );
    assert!(at(&link, status)?.success());
    let before = stamp_of(&link)?;
    assert_eq!(link_cli(&executable, &bin).unwrap(), link);
    assert_eq!(at(&link, std::fs::read_link(&link))?, executable);
    assert_ne!(stamp_of(&link)?.0, before.0);
    Ok(())
}

/// The keep rule says yes only for this user's link with no shared access list that names
/// the executable by the same bytes, and no on every single miss.
#[test]
fn the_keep_rule_refuses_every_miss() {
    let executable = Path::new("/apps/crazyagent");
    let respelled = Path::new("/apps/./crazyagent");
    let other = Path::new("/apps/other");
    assert!(cli_link::keeps(
        501,
        501,
        Some(false),
        Some(executable),
        executable
    ));
    assert!(!cli_link::keeps(
        502,
        501,
        Some(false),
        Some(executable),
        executable
    ));
    assert!(!cli_link::keeps(
        0,
        501,
        Some(false),
        Some(executable),
        executable
    ));
    assert!(!cli_link::keeps(
        501,
        501,
        Some(true),
        Some(executable),
        executable
    ));
    assert!(!cli_link::keeps(
        501,
        501,
        None,
        Some(executable),
        executable
    ));
    assert!(!cli_link::keeps(501, 501, Some(false), None, executable));
    assert!(!cli_link::keeps(
        501,
        501,
        Some(false),
        Some(other),
        executable
    ));
    assert!(!cli_link::keeps(
        501,
        501,
        Some(false),
        Some(respelled),
        executable
    ));
}

/// A link that names the executable but another user owns is swapped for a new link of this
/// user. Making a link of another user needs root, so this row runs where `sudo` asks no
/// password, on the macOS farm runner.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "needs sudo without a password: the macos-15 farm row"]
fn a_link_another_user_owns_is_swapped_for_a_new_one() -> TestResult {
    use std::os::unix::fs::MetadataExt;
    let (_temp, executable, bin) = setup()?;
    let link = link_cli(&executable, &bin).unwrap();
    let status = crate::spawn_gate::status(
        std::process::Command::new("/usr/bin/sudo")
            .args(["-n", "/usr/sbin/chown", "-h", "nobody"])
            .arg(&link),
    );
    assert!(at(&link, status)?.success());
    let owner = at(&link, std::fs::symlink_metadata(&link))?.uid();
    assert_ne!(owner, rustix::process::getuid().as_raw());
    assert_eq!(link_cli(&executable, &bin).unwrap(), link);
    assert_eq!(at(&link, std::fs::read_link(&link))?, executable);
    assert_eq!(
        at(&link, std::fs::symlink_metadata(&link))?.uid(),
        rustix::process::getuid().as_raw()
    );
    assert_eq!(names(&bin)?, vec!["crazyagent".to_string()]);
    Ok(())
}

/// A link that names another executable is swapped without its access list read: the
/// names differ, so the keep rule says no before any list is read.
#[cfg(target_os = "macos")]
#[test]
fn a_link_naming_another_executable_is_swapped_with_no_access_list_read() -> TestResult {
    let (temp, executable, bin) = setup()?;
    let other = temp.path().join("app/other");
    at(&other, std::fs::write(&other, "#!/bin/sh\n"))?;
    let link = bin.join("crazyagent");
    at(&link, std::os::unix::fs::symlink(&other, &link))?;
    let before = cli_link::acl_reads();
    assert_eq!(link_cli(&executable, &bin).unwrap(), link);
    assert_eq!(cli_link::acl_reads(), before);
    assert_eq!(at(&link, std::fs::read_link(&link))?, executable);
    Ok(())
}

/// A bin folder replaced by a link after the publish began is never followed: the link
/// lands in the folder first opened, and the folder the new link names stays empty.
#[test]
fn a_bin_folder_replaced_by_a_link_after_the_open_is_not_followed() -> TestResult {
    let (temp, executable, bin) = setup()?;
    let held = temp.path().join("home/.local/bin-held");
    let decoy = temp.path().join("decoy");
    at(&decoy, std::fs::create_dir_all(&decoy))?;
    let (moved, aside, planted) = (bin.clone(), held.clone(), decoy.clone());
    cli_link::before_publish(move |_| {
        std::fs::rename(&moved, &aside).unwrap();
        std::os::unix::fs::symlink(&planted, &moved).unwrap();
    });
    let _ = link_cli(&executable, &bin);
    assert_eq!(names(&decoy)?, Vec::<String>::new());
    assert_eq!(names(&held)?, vec!["crazyagent".to_string()]);
    assert_eq!(
        at(&held, std::fs::read_link(held.join("crazyagent")))?,
        executable
    );
    Ok(())
}

/// A bin folder that is itself a link to a real folder, as a dotfiles setup makes it, still
/// gets the link: the one open follows it, and the link lands in the folder it names.
#[test]
fn a_bin_folder_that_is_a_link_to_a_real_folder_still_publishes() -> TestResult {
    let (temp, executable, _) = setup()?;
    let real = temp.path().join("dotfiles/bin");
    at(&real, std::fs::create_dir_all(&real))?;
    let bin = temp.path().join("home/.local/bin-linked");
    at(&bin, std::os::unix::fs::symlink(&real, &bin))?;
    assert_eq!(link_cli(&executable, &bin).unwrap(), bin.join("crazyagent"));
    assert_eq!(names(&real)?, vec!["crazyagent".to_string()]);
    assert_eq!(
        at(&real, std::fs::read_link(real.join("crazyagent")))?,
        executable
    );
    Ok(())
}
