use super::{OwnerRuntimeHandle, RegisteredNativeAuthHost};
use crate::engine::bootstrap::EngineService;
use sha2::{Digest, Sha256};
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::watch;

struct RegistryCase {
    root: tempfile::TempDir,
    service: EngineService,
    _stop: watch::Sender<bool>,
}

impl RegistryCase {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-rreg-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, b"timeout_ms = 1000\n").unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        Self {
            root,
            service,
            _stop: stop,
        }
    }

    fn host(&self) -> OwnerRuntimeHandle {
        let retained = self.service.owner_runtime().unwrap();
        OwnerRuntimeHandle {
            engine: Arc::clone(&retained.engine),
            authority: Arc::clone(&retained.authority),
        }
    }
}

async fn registry_case<F, T>(case: F)
where
    F: FnOnce(OwnerRuntimeHandle, PathBuf) -> T,
    T: Future<Output = ()> + Send + 'static,
{
    let mut fixture = RegistryCase::start().await;
    let host = fixture.host();
    let root = fixture.root.path().to_path_buf();
    let outcome = tokio::spawn(case(host, root)).await;
    fixture.service.shutdown().await.unwrap();
    outcome.unwrap();
}

fn physical_file(root: &Path, name: &str, contents: &[u8]) -> PathBuf {
    let path = root.join(name);
    std::fs::write(&path, contents).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

#[tokio::test]
async fn rcli_registry_absent_role_returns_actual_unavailable() {
    registry_case(|host, _root| async move {
        let error = host.registered_rcli_master_executable().err().unwrap();
        assert_eq!(error.0, "native_auth_host_unavailable");
        let registration = host.authority.native_auth_host.lock().unwrap();
        assert!(registration.executable.is_none());
        assert!(registration.codex_executable.is_none());
        assert!(!registration.frozen);
        assert!(!registration.disabled);
    })
    .await;
}

#[tokio::test]
async fn rcli_registry_repeated_access_retains_same_file_and_measured_digest() {
    registry_case(|host, root| async move {
        let path = physical_file(&root, "rcli", b"physical-rcli-registry-bytes");
        let expected = format!("{:x}", Sha256::digest(std::fs::read(&path).unwrap()));
        host.register_rcli_master_executable(RegisteredNativeAuthHost::capture(&path).unwrap())
            .unwrap();
        let first = host.registered_rcli_master_executable().unwrap();
        let second = host.registered_rcli_master_executable().unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.verified_path().unwrap(), path);
        assert_eq!(first.verified_sha256().unwrap(), expected);
        assert_eq!(second.verified_sha256().unwrap(), expected);
    })
    .await;
}

#[tokio::test]
async fn rcli_registry_duplicate_registration_preserves_original_retained_file() {
    registry_case(|host, root| async move {
        let first_path = physical_file(&root, "first", b"first-physical-file");
        let other_path = physical_file(&root, "other", b"other-physical-file");
        host.register_rcli_master_executable(
            RegisteredNativeAuthHost::capture(&first_path).unwrap(),
        )
        .unwrap();
        let retained = host.registered_rcli_master_executable().unwrap();
        let error = host
            .register_rcli_master_executable(
                RegisteredNativeAuthHost::capture(&other_path).unwrap(),
            )
            .unwrap_err();
        assert_eq!(error.0, "native_auth_host_registration_frozen");
        let current = host.registered_rcli_master_executable().unwrap();
        assert!(Arc::ptr_eq(&retained, &current));
        assert_eq!(current.verified_path().unwrap(), first_path);
        assert_eq!(std::fs::read(other_path).unwrap(), b"other-physical-file");
    })
    .await;
}

#[tokio::test]
async fn rcli_registry_actual_disable_rejects_lookup_and_replacement() {
    registry_case(|host, root| async move {
        let path = physical_file(&root, "rcli", b"disabled-physical-file");
        host.register_rcli_master_executable(RegisteredNativeAuthHost::capture(&path).unwrap())
            .unwrap();
        let retained = host.registered_rcli_master_executable().unwrap();
        host.invalidate_native_auth_dispatch().unwrap();
        let error = host.registered_rcli_master_executable().err().unwrap();
        assert_eq!(error.0, "native_auth_host_unavailable");
        let error = host
            .register_rcli_master_executable(RegisteredNativeAuthHost::capture(&path).unwrap())
            .unwrap_err();
        assert_eq!(error.0, "native_auth_host_registration_frozen");
        assert_eq!(retained.verified_path().unwrap(), path);
        let registration = host.authority.native_auth_host.lock().unwrap();
        assert!(registration.disabled);
        assert!(registration.frozen);
    })
    .await;
}

#[tokio::test]
async fn rcli_registry_frozen_empty_role_rejects_late_registration() {
    registry_case(|host, root| async move {
        let path = physical_file(&root, "rcli", b"late-physical-file");
        host.authority.native_auth_host.lock().unwrap().frozen = true;
        let error = host
            .register_rcli_master_executable(RegisteredNativeAuthHost::capture(&path).unwrap())
            .unwrap_err();
        assert_eq!(error.0, "native_auth_host_registration_frozen");
        let error = host.registered_rcli_master_executable().err().unwrap();
        assert_eq!(error.0, "native_auth_host_unavailable");
        assert!(!host.authority.native_auth_host.lock().unwrap().disabled);
    })
    .await;
}

#[tokio::test]
async fn rcli_registry_frozen_registered_role_still_verifies_existing_file() {
    registry_case(|host, root| async move {
        let path = physical_file(&root, "rcli", b"frozen-physical-file");
        let other = physical_file(&root, "other", b"other-frozen-file");
        host.register_rcli_master_executable(RegisteredNativeAuthHost::capture(&path).unwrap())
            .unwrap();
        let original = host.registered_rcli_master_executable().unwrap();
        host.authority.native_auth_host.lock().unwrap().frozen = true;
        let current = host.registered_rcli_master_executable().unwrap();
        assert!(Arc::ptr_eq(&original, &current));
        assert_eq!(current.verified_path().unwrap(), path);
        let error = host
            .register_rcli_master_executable(RegisteredNativeAuthHost::capture(&other).unwrap())
            .unwrap_err();
        assert_eq!(error.0, "native_auth_host_registration_frozen");
        assert!(!host.authority.native_auth_host.lock().unwrap().disabled);
    })
    .await;
}

#[tokio::test]
async fn rcli_registry_real_owner_shutdown_invalidates_retained_handle() {
    let mut fixture = RegistryCase::start().await;
    let host = fixture.host();
    let path = physical_file(fixture.root.path(), "rcli", b"shutdown-physical-file");
    host.register_rcli_master_executable(RegisteredNativeAuthHost::capture(&path).unwrap())
        .unwrap();
    let retained = host.registered_rcli_master_executable().unwrap();
    fixture.service.shutdown().await.unwrap();
    let error = host.registered_rcli_master_executable().err().unwrap();
    assert_eq!(error.0, "owner_runtime_stale");
    let error = host
        .register_rcli_master_executable(RegisteredNativeAuthHost::capture(&path).unwrap())
        .unwrap_err();
    assert_eq!(error.0, "owner_runtime_stale");
    assert_eq!(retained.verified_path().unwrap(), path);
}

#[tokio::test]
async fn rcli_registry_changed_file_before_registration_leaves_role_absent() {
    registry_case(|host, root| async move {
        let path = physical_file(&root, "rcli", b"physical-file-a");
        let captured = RegisteredNativeAuthHost::capture(&path).unwrap();
        std::fs::write(&path, b"physical-file-b").unwrap();
        let error = host.register_rcli_master_executable(captured).unwrap_err();
        assert_eq!(error.0, "native_auth_host_executable_changed");
        let error = host.registered_rcli_master_executable().err().unwrap();
        assert_eq!(error.0, "native_auth_host_unavailable");
        let registration = host.authority.native_auth_host.lock().unwrap();
        assert!(!registration.frozen);
        assert!(!registration.disabled);
    })
    .await;
}

#[tokio::test]
async fn rcli_registry_changed_file_after_registration_is_rejected() {
    registry_case(|host, root| async move {
        let path = physical_file(&root, "rcli", b"physical-file-a");
        host.register_rcli_master_executable(RegisteredNativeAuthHost::capture(&path).unwrap())
            .unwrap();
        let retained = host.registered_rcli_master_executable().unwrap();
        std::fs::write(&path, b"physical-file-b").unwrap();
        let error = host.registered_rcli_master_executable().err().unwrap();
        assert_eq!(error.0, "native_auth_host_executable_changed");
        assert!(retained.verified_sha256().is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"physical-file-b");
    })
    .await;
}

#[tokio::test]
async fn rcli_registry_same_bytes_replaced_inode_is_rejected() {
    registry_case(|host, root| async move {
        let bytes = b"identical-physical-file-bytes";
        let path = physical_file(&root, "rcli", bytes);
        host.register_rcli_master_executable(RegisteredNativeAuthHost::capture(&path).unwrap())
            .unwrap();
        let retained = host.registered_rcli_master_executable().unwrap();
        let replacement = physical_file(&root, "replacement", bytes);
        std::fs::rename(&replacement, &path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let error = host.registered_rcli_master_executable().err().unwrap();
        assert_eq!(error.0, "native_auth_host_executable_changed");
        assert!(retained.verified_path().is_err());
        assert!(!replacement.exists());
    })
    .await;
}

#[tokio::test]
async fn rcli_registry_unsafe_mode_drift_is_rejected() {
    registry_case(|host, root| async move {
        let path = physical_file(&root, "rcli", b"mode-change-file");
        host.register_rcli_master_executable(RegisteredNativeAuthHost::capture(&path).unwrap())
            .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o720)).unwrap();
        let error = host.registered_rcli_master_executable().err().unwrap();
        assert_eq!(error.0, "native_auth_host_executable_unsafe");
        assert_eq!(std::fs::read(path).unwrap(), b"mode-change-file");
    })
    .await;
}

#[tokio::test]
async fn rcli_registry_codex_and_custodian_never_fill_the_absent_rcli_role() {
    registry_case(|host, root| async move {
        let custodian = physical_file(&root, "custodian", b"custodian-physical-file");
        let codex = physical_file(&root, "codex", b"codex-physical-file");
        host.register_native_auth_host(RegisteredNativeAuthHost::capture(&custodian).unwrap())
            .unwrap();
        host.register_codex_executable(RegisteredNativeAuthHost::capture(&codex).unwrap())
            .unwrap();
        let error = host.registered_rcli_master_executable().err().unwrap();
        assert_eq!(error.0, "native_auth_host_unavailable");
        assert_eq!(
            host.registered_codex_executable()
                .unwrap()
                .verified_path()
                .unwrap(),
            codex
        );
        let registration = host.authority.native_auth_host.lock().unwrap();
        assert_eq!(
            registration
                .executable
                .as_ref()
                .unwrap()
                .verified_path()
                .unwrap(),
            custodian
        );
        assert!(!registration.frozen);
        assert!(!registration.disabled);
    })
    .await;
}

#[tokio::test]
async fn rcli_registry_roles_keep_distinct_files_and_independent_drift() {
    registry_case(|host, root| async move {
        let rcli = physical_file(&root, "rcli", b"rcli-physical-file");
        let codex = physical_file(&root, "codex", b"codex-physical-file");
        let custodian = physical_file(&root, "custodian", b"custodian-physical-file");
        host.register_rcli_master_executable(RegisteredNativeAuthHost::capture(&rcli).unwrap())
            .unwrap();
        assert!(host.registered_codex_executable().is_err());
        assert!(
            host.authority
                .native_auth_host
                .lock()
                .unwrap()
                .executable
                .is_none()
        );
        host.register_codex_executable(RegisteredNativeAuthHost::capture(&codex).unwrap())
            .unwrap();
        host.register_native_auth_host(RegisteredNativeAuthHost::capture(&custodian).unwrap())
            .unwrap();
        let rcli_file = host.registered_rcli_master_executable().unwrap();
        let codex_file = host.registered_codex_executable().unwrap();
        let custodian_file = host
            .authority
            .native_auth_host
            .lock()
            .unwrap()
            .executable
            .as_ref()
            .unwrap()
            .clone();
        assert!(!Arc::ptr_eq(&rcli_file, &codex_file));
        assert!(!Arc::ptr_eq(&rcli_file, &custodian_file));
        assert!(!Arc::ptr_eq(&codex_file, &custodian_file));
        assert_ne!(
            rcli_file.verified_sha256().unwrap(),
            codex_file.verified_sha256().unwrap()
        );
        assert_eq!(rcli_file.verified_path().unwrap(), rcli);
        std::fs::write(&rcli, b"changed-rcli-file").unwrap();
        assert!(host.registered_rcli_master_executable().is_err());
        assert_eq!(
            host.registered_codex_executable()
                .unwrap()
                .verified_path()
                .unwrap(),
            codex
        );
        assert_eq!(custodian_file.verified_path().unwrap(), custodian);
        assert_ne!(
            codex_file.verified_sha256().unwrap(),
            custodian_file.verified_sha256().unwrap()
        );
    })
    .await;
}

#[tokio::test]
async fn rcli_registry_real_runtime_directory_drift_rejects_both_methods() {
    registry_case(|host, root| async move {
        let path = physical_file(&root, "rcli", b"runtime-directory-file");
        let captured = RegisteredNativeAuthHost::capture(&path).unwrap();
        let generation = host.authority.root.path.clone();
        let moved = root.join("moved-owner-generation");
        std::fs::rename(&generation, &moved).unwrap();
        let registration = host.register_rcli_master_executable(captured);
        std::fs::rename(&moved, &generation).unwrap();
        assert!(registration.is_err());
        host.register_rcli_master_executable(RegisteredNativeAuthHost::capture(&path).unwrap())
            .unwrap();
        std::fs::rename(&generation, &moved).unwrap();
        let lookup = host.registered_rcli_master_executable();
        std::fs::rename(&moved, &generation).unwrap();
        assert!(lookup.is_err());
        assert_eq!(
            host.registered_rcli_master_executable()
                .unwrap()
                .verified_path()
                .unwrap(),
            path
        );
    })
    .await;
}
