use super::*;

#[test]
fn capture_allocates_distinct_actual_private_artifacts() {
    let temporary = private_root();
    let root = Root::open(temporary.path()).unwrap();
    let (first_runtime, first_capture, first) = allocate_master_capture(&root).unwrap();
    let (second_runtime, second_capture, second) = allocate_master_capture(&root).unwrap();
    assert_ne!(first_runtime, second_runtime);
    assert_ne!(first_capture, second_capture);
    assert_ne!(first_runtime, first_capture);
    assert_ne!(
        first.metadata().unwrap().ino(),
        second.metadata().unwrap().ino()
    );
    for (runtime_ref, capture_ref, capture) in [
        (first_runtime, first_capture, first),
        (second_runtime, second_capture, second),
    ] {
        let runtime = temporary
            .path()
            .join(format!("master-runtime-{}", runtime_ref.as_str()));
        let path = runtime.join(format!("capture-{}.jsonl", capture_ref.as_str()));
        let named = std::fs::symlink_metadata(path).unwrap();
        let held = capture.metadata().unwrap();
        assert_eq!(
            std::fs::symlink_metadata(runtime).unwrap().mode() & 0o7777,
            0o700
        );
        assert_eq!(named.ino(), held.ino());
        assert_eq!(named.dev(), held.dev());
        assert_eq!(held.mode() & 0o7777, 0o600);
        assert_eq!(held.nlink(), 1);
        assert_eq!(held.len(), 0);
        assert_eq!(held.uid(), rustix::process::geteuid().as_raw());
    }
}

#[test]
fn capture_refuses_replaced_owner_root_without_writing_replacement() {
    let temporary = private_root();
    let path = temporary.path().join("runtime");
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = Root::open(&path).unwrap();
    std::fs::rename(&path, temporary.path().join("retained")).unwrap();
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(allocate_master_capture(&root).is_err());
    assert_eq!(std::fs::read_dir(path).unwrap().count(), 0);
    assert_eq!(
        std::fs::read_dir(temporary.path().join("retained"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn capture_refuses_owner_root_permission_drift() {
    let temporary = private_root();
    let root = Root::open(temporary.path()).unwrap();
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(allocate_master_capture(&root).is_err());
    assert_eq!(std::fs::read_dir(temporary.path()).unwrap().count(), 0);
}
