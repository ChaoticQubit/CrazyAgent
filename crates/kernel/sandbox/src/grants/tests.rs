use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
#[test]
fn the_temp_folder_is_given_only_when_it_is_the_owner_s_private_per_user_folder() {
    assert_eq!(private_temp(Path::new("/tmp")), Err("temp_folder_shared"));
    let open = tempfile::tempdir_in("/tmp").unwrap();
    std::fs::set_permissions(open.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        private_temp(open.path()),
        Err("temp_folder_shared"),
        "a private folder outside /private/var/folders is not the owner's temp folder"
    );
    if cfg!(target_os = "macos") {
        let own = std::env::temp_dir().canonicalize().unwrap();
        if own.starts_with(PER_USER_TEMP) {
            assert_eq!(owner_temp_folder(), Ok(own));
        }
    } else {
        assert_eq!(owner_temp_folder(), Err("temp_folder_needs_macos"));
    }
}
