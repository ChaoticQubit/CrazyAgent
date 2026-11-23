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

#[test]
fn a_port_the_hooks_name_as_an_ide_port_is_refused_and_held_now_drops_it() {
    fn ide_ports(_data_root: &Path) -> Vec<u16> {
        vec![20_123]
    }
    fn harness_folders() -> Vec<&'static str> {
        Vec::new()
    }
    let hooks = GrantHooks {
        harness_folders,
        ide_ports,
    };
    let data = Path::new("/nonexistent-engine-data");
    let ide = crate::policy::AccessGrant::Port { port: 20_123 };
    let free = crate::policy::AccessGrant::Port { port: 20_124 };
    assert_eq!(checked(&hooks, &ide, data), Err("grant_port_reserved"));
    assert_eq!(checked(&hooks, &free, data), Ok(free.clone()));
    assert_eq!(
        held_now(&hooks, &[ide, free.clone()], &[], data),
        vec![free.clone()]
    );
    assert!(held_now(&hooks, &[free], &[20_124], data).is_empty());
}
