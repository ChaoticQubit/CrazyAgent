use super::*;

/// 2.4.3 W9: on Windows the user's temp folder is one root in every spelling, as %TEMP%
/// gives it (8.3 on hosted runners), plain long and verbatim, so a file directly in it
/// passes the place check, while a place outside temp whose real file lies in it still
/// leads into temp. Input: none. Output: passes or panics.
#[test]
fn a_windows_temp_folder_is_one_root_in_every_spelling_and_only_a_link_into_it_is_refused() {
    let roots = temp_roots();
    let owner = std::env::temp_dir();
    let verbatim = owner.canonicalize().unwrap();
    let plain = crate::sandbox::canonical(&owner).unwrap();
    assert!(
        verbatim.to_str().unwrap().starts_with(r"\\?\"),
        "{verbatim:?}"
    );
    assert!(!plain.to_str().unwrap().starts_with(r"\\?\"), "{plain:?}");
    for spelling in [&owner, &verbatim, &plain] {
        assert!(roots.contains(spelling), "{spelling:?} in {roots:?}");
    }
    let folder = tempfile::tempdir().unwrap();
    let file = crate::sandbox::canonical(folder.path())
        .unwrap()
        .join("tool.exe");
    std::fs::write(&file, "").unwrap();
    let real = file.canonicalize().unwrap();
    for written in [folder.path().join("tool.exe"), file.clone(), real.clone()] {
        assert!(in_temp_of(&written, &roots), "{written:?}");
        assert!(!leads_into_temp(&written, &real, &roots), "{written:?}");
    }
    let places = Places::engine(None, None, None, Some(OsStr::new("")));
    assert_eq!(check_place(&places, &file), Some(Ok(())));
    let outside = Path::new(r"C:\CrazyAgentOutsideTemp\bin\tool.exe");
    assert!(leads_into_temp(outside, &real, &roots));
}
