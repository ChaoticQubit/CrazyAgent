use super::*;

/// A folder on this Mac whose `.git` entry cannot be looked up counts as a linked checkout in
/// the early check as in the full plan: only a source that is itself an iCloud placeholder
/// leaves that lookup to the plan after its download.
#[test]
fn a_copy_of_a_folder_whose_git_entry_cannot_be_read_is_a_linked_checkout_in_either_check() {
    let case = case();
    let shut = case.sources.join("shut");
    fs::create_dir(&shut).unwrap();
    fs::write(shut.join("README.md"), "shut\n").unwrap();
    fs::set_permissions(&shut, fs::Permissions::from_mode(0o600)).unwrap();
    let copy = AddMode::Copy;
    let checked = check_add(&case.home, &case.engine, text(&shut), copy, &[], &[]);
    let planned = plan_add(&case.home, &case.engine, text(&shut), copy, &[], &[]);
    fs::set_permissions(&shut, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(checked.unwrap_err().0, "home_copy_linked_checkout", "early");
    assert_eq!(planned.unwrap_err().0, "home_copy_linked_checkout", "full");
    assert_eq!(names(&case.home), Vec::<String>::new());
}
