use super::*;
use std::os::unix::fs::PermissionsExt;

/// A renewal whose harness file the tool resolver refuses to start, here a script whose
/// interpreter is missing, fails as a renewal that could not start does, with
/// `native_profile_renewal_failed` (a transient master reason), never the resolver's
/// sentence as its code.
#[test]
fn a_renewal_whose_harness_cannot_start_fails_with_the_renewal_code() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let codex = root.join("codex");
    std::fs::write(&codex, "#!/nonexistent/crazyagent-test-interpreter\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    let failed = managed_command(&codex, &root).unwrap_err();
    assert_eq!(failed.0, "native_profile_renewal_failed");
}
