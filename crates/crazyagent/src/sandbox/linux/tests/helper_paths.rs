use super::*;

/// An engine whose own folder is not cargo's `deps` takes only the helper beside it, even
/// with none there and a helper in the folder above, and a `deps` folder further up the
/// path changes nothing (V243-LANDLOCK-TEST-HELPER-PATH).
#[test]
fn an_engine_outside_a_deps_folder_never_takes_another_helper() {
    let root = tempfile::tempdir().unwrap();
    for folder in ["opt/bin", "deps/bin", "depsx"] {
        let engine = root.path().join(folder).join("crazyagent");
        fs::create_dir_all(engine.parent().unwrap()).unwrap();
        fs::write(engine.parent().unwrap().with_file_name(HELPER_NAME), "").unwrap();
        assert_eq!(
            helper_for(&engine),
            engine.with_file_name(HELPER_NAME),
            "{folder}"
        );
    }
}

/// A debug test binary in cargo's `deps` folder with no helper beside it takes cargo's own
/// build of the helper in the folder above; one beside it wins, and with neither it keeps
/// the missing one beside it (V243-LANDLOCK-TEST-HELPER-PATH).
#[cfg(debug_assertions)]
#[test]
fn a_test_binary_in_cargos_deps_folder_takes_cargos_helper_build_only_when_none_is_beside_it() {
    let root = tempfile::tempdir().unwrap();
    let deps = root.path().join("debug").join("deps");
    fs::create_dir_all(&deps).unwrap();
    let engine = deps.join("linux_sandbox-0123abcd");
    let (beside, built) = (deps.join(HELPER_NAME), deps.with_file_name(HELPER_NAME));
    assert_eq!(
        helper_for(&engine),
        beside,
        "neither: the missing one beside it"
    );
    fs::write(&built, "").unwrap();
    assert_eq!(helper_for(&engine), built, "cargo's build, one folder up");
    fs::write(&beside, "").unwrap();
    assert_eq!(helper_for(&engine), beside, "the one beside it wins");
}
