use super::super::master_host_launch::allocate_master_capture;
use super::super::master_runtime::Capture;
use super::super::{OwnerRuntimeHandle, Root};
use crate::engine::EngineError;
use crate::protocol::RequestId;
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

type AllocateCapture = fn(&OwnerRuntimeHandle) -> Result<(RequestId, RequestId, File), EngineError>;

const _: AllocateCapture = OwnerRuntimeHandle::allocate_master_capture;

const _: fn(&mut Capture, &crate::rcli_host::Event) -> Result<(), EngineError> = Capture::record;

const _: fn(&mut Capture, &crate::adapters::events::ObservedEvent) -> Result<(), EngineError> =
    Capture::record;

const _: fn(&Capture) -> u64 = Capture::sequence;

fn private_capture_root() -> (tempfile::TempDir, Root) {
    let temporary = tempfile::Builder::new()
        .prefix("ca-rcli-master-capture-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = Root::open(temporary.path()).unwrap();
    (temporary, root)
}

#[test]
fn retained_allocator_file_enters_shared_capture_without_replacement() {
    let (temporary, root) = private_capture_root();
    let (runtime_ref, capture_ref, file) = allocate_master_capture(&root).unwrap();
    let held = file.metadata().unwrap();
    let capture = Capture::new(file, 8192).unwrap();
    assert_eq!(capture.sequence(), 0);
    let path = temporary
        .path()
        .join(format!("master-runtime-{}", runtime_ref.as_str()))
        .join(format!("capture-{}.jsonl", capture_ref.as_str()));
    let named = std::fs::symlink_metadata(path).unwrap();
    assert_ne!(runtime_ref, capture_ref);
    assert_eq!(named.dev(), held.dev());
    assert_eq!(named.ino(), held.ino());
    assert_eq!(named.mode() & 0o7777, 0o600);
    assert_eq!(named.nlink(), 1);
    assert_eq!(named.len(), 0);
    drop(capture);
}

#[test]
fn shared_capture_rejects_preexisting_bytes_on_allocated_file() {
    let (_temporary, root) = private_capture_root();
    let (_, _, mut file) = allocate_master_capture(&root).unwrap();
    file.write_all(b"occupied\n").unwrap();
    file.sync_all().unwrap();
    assert_eq!(
        Capture::new(file, 8192).err().unwrap().0,
        "master_capture_identity"
    );
}

#[test]
fn shared_capture_rejects_permission_drift_after_allocation() {
    let (_temporary, root) = private_capture_root();
    let (_, _, file) = allocate_master_capture(&root).unwrap();
    file.set_permissions(std::fs::Permissions::from_mode(0o644))
        .unwrap();
    assert_eq!(
        Capture::new(file, 8192).err().unwrap().0,
        "master_capture_identity"
    );
}

#[test]
fn shared_capture_cannot_finish_without_observed_events() {
    let (_temporary, root) = private_capture_root();
    let (_, _, file) = allocate_master_capture(&root).unwrap();
    let capture = Capture::new(file, 8192).unwrap();
    assert!(std::panic::catch_unwind(|| capture.finish()).is_err());
}
