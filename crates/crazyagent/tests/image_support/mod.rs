use std::time::{Duration, Instant};

const LOCK_NAME: &str = "crazyagent-test-disk-images.lock";
const LOCK_WAIT: Duration = Duration::from_secs(300);
const LOCK_POLL: Duration = Duration::from_millis(100);

/// Takes this user's one lock for test disk images, shared by every test process, so no
/// test attaches, renumbers or detaches an image while another expects a device number to
/// stay or change: device numbers belong to the whole machine. The lock file in the temp
/// folder is opened without following a link and must be a regular file this user owns.
/// Output: the locked file; the lock ends when it is dropped. Panics after `LOCK_WAIT`.
pub fn lock() -> std::fs::File {
    use rustix::fs::{FileType, Mode, OFlags};
    let path = std::env::temp_dir().join(LOCK_NAME);
    let opened = rustix::fs::open(
        &path,
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .unwrap_or_else(|error| panic!("disk image lock {path:?} did not open: {error}"));
    let stat = rustix::fs::fstat(&opened).unwrap();
    assert_eq!(
        FileType::from_raw_mode(stat.st_mode),
        FileType::RegularFile,
        "disk image lock {path:?} is not a regular file"
    );
    assert_eq!(
        stat.st_uid,
        rustix::process::getuid().as_raw(),
        "disk image lock {path:?} belongs to another user"
    );
    let file = std::fs::File::from(opened);
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return file,
            Err(std::fs::TryLockError::WouldBlock) => {
                assert!(
                    Instant::now() < deadline,
                    "disk image lock {path:?} still held by another test after {LOCK_WAIT:?}"
                );
                std::thread::sleep(LOCK_POLL);
            }
            Err(std::fs::TryLockError::Error(error)) => {
                panic!("disk image lock {path:?} could not be taken: {error}")
            }
        }
    }
}
