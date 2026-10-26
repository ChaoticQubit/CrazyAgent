use super::{master_turn_diagnostic, native_auth_dispatch_diagnostic};
use crate::engine::EngineError;
use crate::engine::native_auth_host::RegisteredNativeAuthHost;
use std::os::unix::fs::PermissionsExt;

#[test]
fn native_auth_dispatch_diagnostic_preserves_real_guard_error() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("host");
    std::fs::write(&path, b"before").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let host = RegisteredNativeAuthHost::capture(&path).unwrap();
    assert_eq!(host.verified_path().unwrap(), path.as_path());

    std::fs::write(&path, b"changed").unwrap();
    let error = host.verified_path().unwrap_err();
    assert_eq!(error.0.as_str(), "native_auth_host_executable_changed");
    assert_eq!(
        native_auth_dispatch_diagnostic(&error),
        "native_auth_dispatch_failed_fence_retained cause=native_auth_host_executable_changed"
    );
}

#[test]
fn native_auth_dispatch_diagnostic_redacts_opaque_error() {
    let error = EngineError("opaque-private-text".into());
    let line = native_auth_dispatch_diagnostic(&error);
    assert_eq!(
        line,
        "native_auth_dispatch_failed_fence_retained cause=native_auth_dispatch_error_redacted"
    );
    assert!(!line.contains("opaque-private-text"));
    assert!(!line.contains('\n'));
}

#[test]
fn native_auth_dispatch_diagnostic_redacts_known_code_with_newline_suffix() {
    let error = EngineError("native_auth_host_executable_changed\nopaque-private-text".into());
    let line = native_auth_dispatch_diagnostic(&error);
    assert_eq!(
        line,
        "native_auth_dispatch_failed_fence_retained cause=native_auth_dispatch_error_redacted"
    );
    assert!(!line.contains("opaque-private-text"));
    assert!(!line.contains('\n'));
}

#[test]
fn native_auth_dispatch_diagnostic_shows_joined_internal_codes_but_not_paths() {
    let joined = EngineError(
        "master_runtime_cleanup_uncertain:rcli_master_wait_failed;rcli_master_closed".into(),
    );
    assert_eq!(
        native_auth_dispatch_diagnostic(&joined),
        "native_auth_dispatch_failed_fence_retained cause=master_runtime_cleanup_uncertain:rcli_master_wait_failed;rcli_master_closed"
    );
    let path = EngineError("master_capture_unavailable:/private/tmp/secret".into());
    assert!(!native_auth_dispatch_diagnostic(&path).contains("/private"));
}

#[test]
fn master_turn_diagnostic_names_the_refusal_and_claims_no_fence() {
    let refused = EngineError("repository_config_refused:core_hookspath".into());
    let line = master_turn_diagnostic(&refused);
    assert_eq!(
        line,
        "master_turn_not_completed cause=repository_config_refused:core_hookspath"
    );
    assert!(!line.contains("fence"));
    let path = EngineError("master_capture_unavailable:/private/tmp/secret".into());
    assert_eq!(
        master_turn_diagnostic(&path),
        "master_turn_not_completed cause=master_turn_error_redacted"
    );
}
