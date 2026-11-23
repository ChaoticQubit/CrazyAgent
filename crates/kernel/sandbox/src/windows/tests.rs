use super::{
    APPCONTAINER_MISSING, GRANTS_FOLDER, HELPER_NAME, SEATBELT_MISSING, container_for,
    existing_helper, helper_beside_engine,
};
use crate::error::SandboxError;
use std::path::Path;

#[test]
fn container_for_is_the_prefix_and_32_hex_digits_of_both_folders() {
    let name = container_for(Path::new("/p"), Path::new("/h"));
    assert_eq!(name, "crazyagent.fc27506bbcb6475cd7a4b7bbd50c2721");
    assert_eq!(name, container_for(Path::new("/p"), Path::new("/h")));
    for other in [
        container_for(Path::new("/q"), Path::new("/h")),
        container_for(Path::new("/p"), Path::new("/i")),
        container_for(Path::new("/h"), Path::new("/p")),
    ] {
        assert_ne!(other, name);
        assert_eq!(other.len(), name.len());
        let digits = other.strip_prefix("crazyagent.").expect("the prefix");
        assert!(
            digits
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
    }
}

#[test]
fn existing_helper_refuses_a_missing_or_relative_helper() {
    let folder = tempfile::tempdir().expect("a temp folder");
    let missing = folder.path().join(HELPER_NAME);
    match existing_helper(&missing) {
        Err(SandboxError::Unavailable(reason)) => assert_eq!(reason, APPCONTAINER_MISSING),
        other => panic!("{other:?}"),
    }
    std::fs::write(&missing, b"").expect("the helper is written");
    let held = existing_helper(&missing).expect("an absolute plain file");
    assert_eq!(held, missing);
    let relative = Path::new("Cargo.toml");
    assert!(
        relative.is_file(),
        "the crate manifest is beside the test run"
    );
    assert!(matches!(
        existing_helper(relative),
        Err(SandboxError::Invalid(_))
    ));
    assert!(matches!(
        existing_helper(folder.path()),
        Err(SandboxError::Unavailable(_))
    ));
}

#[test]
fn the_helper_ships_beside_the_engine_under_its_one_name() {
    let beside = helper_beside_engine().expect("the running program has a path");
    let program = std::env::current_exe().expect("the running program has a path");
    assert_eq!(beside, program.with_file_name(HELPER_NAME));
    assert_eq!(HELPER_NAME, "crazyagent-win-boundary.exe");
    assert_eq!(GRANTS_FOLDER, "boundary-grants");
    assert!(APPCONTAINER_MISSING.contains(HELPER_NAME));
    assert!(SEATBELT_MISSING.contains("AppContainer"));
}
