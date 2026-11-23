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
