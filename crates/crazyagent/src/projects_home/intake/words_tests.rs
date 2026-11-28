use crate::projects_home::plain_problem;

#[cfg(target_os = "macos")]
#[test]
fn a_permission_refusal_names_the_macos_privacy_settings_on_macos() {
    assert_eq!(
        plain_problem("home_permission_denied"),
        Some(
            "macOS did not let CrazyAgent read or move this folder. Allow CrazyAgent in System Settings > Privacy & Security > Files and Folders, check the folder's permissions, then try again. Nothing was removed."
        )
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn a_permission_refusal_names_the_folder_permissions_off_macos() {
    let line = plain_problem("home_permission_denied").expect("the code has a sentence");
    assert_eq!(
        line,
        "CrazyAgent couldn't read or move this folder: its permissions don't allow it. Check the folder's owner and permissions, then try again. Nothing was removed."
    );
    assert!(
        !line.contains("macOS") && !line.contains("System Settings"),
        "{line}"
    );
}
