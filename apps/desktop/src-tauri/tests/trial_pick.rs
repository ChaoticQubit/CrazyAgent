#![cfg(all(unix, feature = "native-webdriver"))]

use crazyagent_desktop::bridge::picker::FolderPurpose;
use crazyagent_desktop::bridge::trial_pick::{PICK_BYTES_MAX, PICKS_VARIABLE, take_from};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// A private picks folder and a real folder to pick, in one temp dir. Output: the dir, the
/// picks folder and the folder to pick.
fn setup() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let root = tempfile::tempdir().unwrap();
    let picks = root.path().join("picks");
    fs::create_dir(&picks).unwrap();
    fs::set_permissions(&picks, fs::Permissions::from_mode(0o700)).unwrap();
    let repo = root.path().join("outside").join("ca-repo");
    fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap().to_str().unwrap().to_owned();
    (root, picks, repo)
}

/// Writes one private answer file. Inputs: the picks folder, the file name and the text.
fn answer(picks: &Path, name: &str, text: &str) {
    let file = picks.join(name);
    fs::write(&file, text).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn a_waiting_answer_picks_its_purpose_once_and_leaves_the_other_purposes_to_the_chooser() {
    let (_root, picks, repo) = setup();
    answer(&picks, "project", &format!("{repo}\n"));
    let folder = Some(picks.as_os_str());
    assert_eq!(take_from(folder, FolderPurpose::Skill).unwrap(), None);
    assert_eq!(
        take_from(folder, FolderPurpose::Project).unwrap(),
        Some(repo.clone())
    );
    assert!(!picks.join("project").exists(), "the answer is used once");
    assert_eq!(take_from(folder, FolderPurpose::Project).unwrap(), None);
    for (purpose, name) in [
        (FolderPurpose::Skill, "skill"),
        (FolderPurpose::ProjectsHome, "projects_home"),
        (FolderPurpose::Memory, "memory"),
        (FolderPurpose::Plugin, "plugin"),
    ] {
        answer(&picks, name, &repo);
        assert_eq!(
            take_from(folder, purpose).unwrap(),
            Some(repo.clone()),
            "{name}"
        );
    }
}

#[test]
fn no_picks_folder_leaves_every_pick_to_the_chooser() {
    assert_eq!(PICKS_VARIABLE, "CRAZYAGENT_TRIAL_PICKS");
    for purpose in FolderPurpose::ALL {
        assert_eq!(take_from(None, purpose).unwrap(), None);
    }
}

#[test]
fn an_unsafe_picks_folder_or_answer_is_refused_instead_of_opening_the_chooser() {
    let (root, picks, repo) = setup();
    let refused = |folder: &OsStr| {
        take_from(Some(folder), FolderPurpose::Project)
            .err()
            .map(|error| error.code)
    };
    assert_eq!(refused(OsStr::new("picks")), Some("trial_pick_invalid"));
    fs::set_permissions(&picks, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(refused(picks.as_os_str()), Some("trial_pick_invalid"));
    fs::set_permissions(&picks, fs::Permissions::from_mode(0o700)).unwrap();
    let linked = root.path().join("linked-picks");
    std::os::unix::fs::symlink(&picks, &linked).unwrap();
    assert_eq!(refused(linked.as_os_str()), Some("trial_pick_invalid"));
    let folder = picks.as_os_str();
    for text in [
        "outside/ca-repo".to_owned(),
        format!("{repo}/missing"),
        format!("{repo}\n{repo}"),
        "a".repeat(usize::try_from(PICK_BYTES_MAX).unwrap() + 1),
    ] {
        answer(&picks, "project", &text);
        assert_eq!(refused(folder), Some("trial_pick_invalid"), "{text:.80}");
        let _ = fs::remove_file(picks.join("project"));
    }
    answer(&picks, "project", &repo);
    fs::set_permissions(picks.join("project"), fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(refused(folder), Some("trial_pick_invalid"));
    fs::remove_file(picks.join("project")).unwrap();
    let target = root.path().join("answer");
    fs::write(&target, &repo).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(&target, picks.join("project")).unwrap();
    assert_eq!(refused(folder), Some("trial_pick_invalid"));
    assert!(target.exists(), "a linked answer is never followed");
}
