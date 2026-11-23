use super::*;

/// F26: the xcode-select link names an Xcode app's developer folder only when it leads to a
/// real `<name>.app/Contents/Developer`; the Command Line Tools, another folder and a
/// dangling link name none.
#[cfg(unix)]
#[test]
fn the_xcode_select_link_names_only_an_xcode_app_developer_folder() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let xcode = root
        .join("Xcode-beta.app")
        .join("Contents")
        .join("Developer");
    fs::create_dir_all(&xcode).unwrap();
    let other = root.join("Tools").join("Developer");
    fs::create_dir_all(&other).unwrap();
    let link = |name: &str, target: &Path| {
        let link = root.join(name);
        std::os::unix::fs::symlink(target, &link).unwrap();
        link
    };
    assert_eq!(xcode_from_link(&link("app", &xcode)), Some(xcode.clone()));
    assert_eq!(xcode_from_link(&link("other", &other)), None);
    assert_eq!(
        xcode_from_link(&link("gone", &root.join("missing.app"))),
        None
    );
    assert_eq!(
        xcode_from_link(&link("command-line-tools", Path::new(DEVELOPER_TOOLS))),
        None
    );
}

/// A cargo cache name holding anything but the link to the owner's cache gives way only
/// through the hooks' removal: when that removal refuses, the cargo home is refused with the
/// code the hooks return for it, and nothing is linked over the name.
#[test]
fn a_cargo_cache_name_that_will_not_give_way_refuses_with_the_hooks_code() {
    fn refuse(
        _folder: std::os::fd::BorrowedFd<'_>,
        _name: &str,
        otherwise: &'static str,
    ) -> Result<(), &'static str> {
        Err(otherwise)
    }
    let hooks = ToolchainHooks {
        remove_home_entry: refuse,
    };
    let scratch = tempfile::tempdir().unwrap();
    let root = canonical(scratch.path()).unwrap();
    let owner = root.join("owner");
    fs::create_dir_all(owner.join(".cargo/registry")).unwrap();
    let home = root.join("agent");
    fs::create_dir_all(home.join(".cargo/registry")).unwrap();
    let refused = prepare_cargo_home(&hooks, &home, &ToolHomes::at(&owner)).unwrap_err();
    assert_eq!(refused.to_string(), "cargo cache name will not give way");
    assert!(
        fs::symlink_metadata(home.join(".cargo/registry"))
            .unwrap()
            .is_dir()
    );
}
