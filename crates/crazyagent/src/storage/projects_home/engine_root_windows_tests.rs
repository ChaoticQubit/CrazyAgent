use super::engine_root;
use std::path::PathBuf;

#[test]
fn the_engine_root_is_named_plain_so_a_projects_home_holding_it_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let full = std::fs::canonicalize(temp.path()).unwrap();
    let base = PathBuf::from(full.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
    let data = base.join("data");
    std::fs::create_dir(&data).unwrap();
    let root = engine_root(&data.join("engine.db")).unwrap();
    assert_eq!(
        root, data,
        "the engine root is in the one plain spelling, as one_spelling gives every folder it is compared with (rv-sec2 #717 C1)"
    );
    assert_eq!(
        crate::projects_home::check_home(&base, &root)
            .unwrap_err()
            .0,
        "projects_home_unsafe",
        "a projects home that holds the engine root is refused"
    );
}
