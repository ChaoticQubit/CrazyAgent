use super::RegisteredNativeAuthHost;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[test]
fn native_auth_hash_backend_independent_known_answers() {
    let root = tempfile::tempdir().unwrap();
    let cases = [
        (
            "abc",
            b"abc".to_vec(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            "standard56",
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq".to_vec(),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
        (
            "million_a",
            vec![b'a'; 1_000_000],
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
        ),
        (
            "boundary16383",
            vec![b'a'; 16_383],
            "2e1023f5ad46df0d01aa4b73ce3db452ed0a768387949efa5a858b532b763012",
        ),
        (
            "boundary16384",
            vec![b'a'; 16_384],
            "f3336bea752b5a28743033dd2c844a4a63fba08871aaee2586a2bf2d69be83a2",
        ),
        (
            "boundary16385",
            vec![b'a'; 16_385],
            "608b801060d579a7b659a01755259a13dec8853b97d3d8062763c97ffac6ddae",
        ),
    ];
    assert_eq!(cases.len(), 6);
    for (name, bytes, expected) in cases {
        let path = root.path().join(name);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&path)
            .unwrap();
        file.write_all(&bytes).unwrap();
        file.sync_all().unwrap();
        let before = file.metadata().unwrap();
        assert_eq!(before.len(), u64::try_from(bytes.len()).unwrap());
        let digest = RegisteredNativeAuthHost::hash(&file, before.len()).unwrap();
        let actual: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(actual, expected, "{name}: direct hash");
        let registered = RegisteredNativeAuthHost::capture(&path).unwrap();
        assert_eq!(registered.verified_sha256().unwrap(), expected, "{name}");
        assert_eq!(registered.verified_path().unwrap(), path);
        let after = file.metadata().unwrap();
        let named = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
        assert_eq!((named.dev(), named.ino()), (before.dev(), before.ino()));
        assert_eq!(after.len(), before.len());
    }
}

#[test]
fn native_auth_hash_backend_sixteen_real_hash_calls() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("million-a");
    let bytes = vec![b'a'; 1_000_000];
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(&path)
        .unwrap();
    file.write_all(&bytes).unwrap();
    file.sync_all().unwrap();
    let before = file.metadata().unwrap();
    assert_eq!(before.len(), 1_000_000);
    let expected = [
        0xcd, 0xc7, 0x6e, 0x5c, 0x99, 0x14, 0xfb, 0x92, 0x81, 0xa1, 0xc7, 0xe2, 0x84, 0xd7, 0x3e,
        0x67, 0xf1, 0x80, 0x9a, 0x48, 0xa4, 0x97, 0x20, 0x0e, 0x04, 0x6d, 0x39, 0xcc, 0xc7, 0x11,
        0x2c, 0xd0,
    ];
    let epoch_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    eprintln!(
        "hash_backend_timing_started epoch_ms={epoch_ms} dev={} inode={} file_bytes={} iterations=16",
        before.dev(),
        before.ino(),
        before.len(),
    );
    let started = Instant::now();
    for _ in 0..16 {
        assert!(started.elapsed() <= Duration::from_secs(30));
        assert_eq!(
            RegisteredNativeAuthHost::hash(&file, before.len()).unwrap(),
            expected,
        );
    }
    let elapsed = started.elapsed();
    assert!(elapsed <= Duration::from_secs(30));
    let after = file.metadata().unwrap();
    let named = std::fs::symlink_metadata(&path).unwrap();
    assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
    assert_eq!((named.dev(), named.ino()), (before.dev(), before.ino()));
    assert_eq!(after.len(), before.len());
    eprintln!(
        "hash_backend_timing_finished elapsed_ns={} dev={} inode={} file_bytes={} hashed_bytes=16000000 iterations=16",
        elapsed.as_nanos(),
        before.dev(),
        before.ino(),
        before.len(),
    );
}
