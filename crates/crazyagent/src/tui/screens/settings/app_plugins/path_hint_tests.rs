use super::*;

#[test]
fn on_windows_the_add_path_hint_names_the_drive_form() {
    assert_eq!(
        ABSOLUTE,
        "Type the full path of the plugin folder, starting with its drive, such as C:\\."
    );
}
