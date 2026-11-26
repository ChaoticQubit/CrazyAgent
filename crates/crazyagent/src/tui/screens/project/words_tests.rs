use super::plain_problem;
use crate::tui::screens::project_view::MOVE_ASK_UNREAD;

#[cfg(target_os = "macos")]
#[test]
fn an_unread_folder_names_icloud_on_macos() {
    assert_eq!(
        MOVE_ASK_UNREAD,
        "CrazyAgent couldn't read this folder. Anything in it only in iCloud is downloaded first, rebuildable folders too, which can be slow."
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn an_unread_folder_says_only_that_off_macos() {
    assert_eq!(MOVE_ASK_UNREAD, "CrazyAgent couldn't read this folder.");
    assert!(!MOVE_ASK_UNREAD.contains("iCloud"), "{MOVE_ASK_UNREAD}");
}

#[cfg(target_os = "macos")]
#[test]
fn a_missing_project_folder_names_finder_on_macos() {
    assert_eq!(
        plain_problem("project_folder_missing"),
        "The project folder is missing. Check it in Finder."
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn a_missing_project_folder_names_the_file_manager_off_macos() {
    let line = plain_problem("project_folder_missing");
    assert_eq!(
        line,
        "The project folder is missing. Check it in your file manager."
    );
    assert!(!line.contains("Finder"), "{line}");
}
