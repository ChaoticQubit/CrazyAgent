use super::*;

/// The refusals about the owner's git name what to change: an include placed inside the
/// project folder names moving it, and a git older than 2.40 names updating it, the Mac
/// words naming Xcode or its Command Line Tools and never Homebrew (W-GIT-FLOOR).
#[test]
fn the_owner_git_refusals_name_what_to_change() {
    assert!(
        crate::engine::owner::transport::cleanup::CLEANUP_CODES.contains(&"changes_git_include")
    );
    assert_eq!(
        refusal("changes_git_include"),
        "Your project's Git config includes a file inside your project folder, where accepted changes can rewrite it. Move that file outside the project folder and include it by its full path, then try again."
    );
    assert!(crate::engine::owner::transport::cleanup::CLEANUP_CODES.contains(&"changes_git_old"));
    let old = refusal("changes_git_old");
    assert!(
        old.starts_with("Landing changes needs Git 2.40 or newer"),
        "{old}"
    );
    assert_eq!(
        old.contains("Command Line Tools") && old.contains("xcode-select --install"),
        cfg!(target_os = "macos"),
        "{old}"
    );
    assert!(
        !old.contains("brew upgrade") && !old.contains("brew install"),
        "{old}"
    );
}
