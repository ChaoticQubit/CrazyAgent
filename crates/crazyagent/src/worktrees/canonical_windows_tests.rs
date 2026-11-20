use super::canonical;
use std::path::PathBuf;

#[test]
fn a_repository_named_in_its_plain_spelling_is_canonical_and_its_verbatim_twin_is_not() {
    let temp = tempfile::tempdir().unwrap();
    let full = std::fs::canonicalize(temp.path()).unwrap();
    let plain = PathBuf::from(full.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
    assert!(
        canonical(&plain).is_ok(),
        "a plain repository path is canonical, as one_spelling names every stored folder (W3-4c)"
    );
    assert_eq!(
        canonical(&full).unwrap_err().0,
        "repository_path_not_canonical",
        "the verbatim spelling of that folder is not its stored spelling"
    );
}
