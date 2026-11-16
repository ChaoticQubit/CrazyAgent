use super::reproof::skip_cause;
use super::{NativeAuthHostRegistration, RegisteredNativeAuthHost};
use crate::engine::owner::NativeAuthMethod;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

/// SIGNOUT-AFTER-RESTART: the engine-start re-proof names why it skips a sign-in, for each
/// way the account-check host cannot run it, and skips none the host can run.
#[test]
fn a_skipped_re_proof_names_why_the_host_cannot_run_it() {
    let root = tempfile::Builder::new()
        .prefix("ca-reproof-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("host");
    std::fs::write(&path, b"reproof-host").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let host = || Some(Arc::new(RegisteredNativeAuthHost::capture(&path).unwrap()));
    let mut registration = NativeAuthHostRegistration::default();
    assert_eq!(
        skip_cause(&registration, NativeAuthMethod::ClaudeOfficial),
        Some("native_auth_method_not_reprovable")
    );
    assert_eq!(
        skip_cause(&registration, NativeAuthMethod::ClaudeExisting),
        Some("native_auth_host_unregistered")
    );
    registration.executable = host();
    assert_eq!(
        skip_cause(&registration, NativeAuthMethod::CodexExisting),
        Some("native_auth_harness_unregistered")
    );
    registration.claude_executable = host();
    assert_eq!(
        skip_cause(&registration, NativeAuthMethod::ClaudeExisting),
        None
    );
    registration.disabled = true;
    assert_eq!(
        skip_cause(&registration, NativeAuthMethod::ClaudeExisting),
        Some("native_auth_host_disabled")
    );
}
