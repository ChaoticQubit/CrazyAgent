use super::LaunchWord;
use crate::engine::{EngineError, io_error};
use rustix::fs::{Mode, OFlags};
use sha2::{Digest, Sha256};
use std::fs::{File, Metadata};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) const EXECUTABLE_BYTES_MAX: u64 = 1024 * 1024 * 1024;

/// How long after a harness identity was first seen a matching hash must start to prove its
/// bytes for good off macOS. A write after that stamps a change time more than one clock tick
/// and the coarsest file system time grain (2 s) past the seen one, so it cannot keep the
/// identity, and from then on the identity check alone sees every write.
#[cfg(any(test, not(target_os = "macos")))]
pub(super) const SETTLE_AFTER: std::time::Duration = std::time::Duration::from_secs(3);

/// Whether a matching hash proves a harness's bytes for good: it started [`SETTLE_AFTER`]
/// past the first look at the identity by the monotonic clock and past the identity's change
/// time by the wall clock, so a change time ahead of the clock keeps the harness hashing.
/// Inputs: the first look, the hash start on the monotonic and wall clocks, and the change
/// time as (seconds, nanoseconds). Output: true when the harness settles.
#[cfg(any(test, not(target_os = "macos")))]
pub(super) fn settles(
    seen: std::time::Instant,
    started: std::time::Instant,
    wall: std::time::SystemTime,
    changed: (i64, i64),
) -> bool {
    let after = i64::try_from(SETTLE_AFTER.as_secs()).unwrap();
    started >= seen + SETTLE_AFTER
        && wall
            .duration_since(std::time::UNIX_EPOCH)
            .is_ok_and(|wall| {
                (
                    i64::try_from(wall.as_secs()).unwrap_or(i64::MAX),
                    i64::from(wall.subsec_nanos()),
                ) >= (changed.0.saturating_add(after), changed.1)
            })
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ExecutableIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl ExecutableIdentity {
    /// The identity of a file the engine may pin and run: a regular file of 1 byte to
    /// [`EXECUTABLE_BYTES_MAX`] the owner may run, owned by the engine's user or by root
    /// (only root can change a root file), with no group or world write. Input: the file's
    /// metadata. Output: the identity, or `native_auth_host_executable_unsafe`.
    fn of(metadata: &Metadata) -> Result<Self, EngineError> {
        let owner = metadata.uid();
        if !metadata.is_file()
            || (owner != rustix::process::geteuid().as_raw() && owner != 0)
            || metadata.mode() & 0o022 != 0
            || metadata.mode() & 0o100 == 0
            || !(1..=EXECUTABLE_BYTES_MAX).contains(&metadata.len())
        {
            return Err(EngineError("native_auth_host_executable_unsafe".into()));
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            owner: metadata.uid(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

/// The pin's file rule alone, so a harness search refuses exactly the files a pin would
/// ([`ExecutableIdentity::of`]). Input: the real file's metadata. Output: Ok, or
/// `native_auth_host_executable_unsafe`.
pub(crate) fn pinnable(metadata: &Metadata) -> Result<(), EngineError> {
    ExecutableIdentity::of(metadata).map(|_| ())
}

/// The identity a host's checks compare against: the captured one, or the one a re-hash
/// proved after only the change time moved ([`RegisteredNativeAuthHost::retime`]), with when
/// the checks first saw it off macOS.
#[derive(Clone, Copy)]
struct Pin {
    identity: ExecutableIdentity,
    #[cfg(not(target_os = "macos"))]
    seen: std::time::Instant,
}

pub(crate) struct RegisteredNativeAuthHost {
    pub(super) path: PathBuf,
    file: File,
    identity: ExecutableIdentity,
    pub(super) digest: [u8; 32],
    entry: Option<PathBuf>,
    retimed: std::sync::Mutex<Option<Pin>>,
    pub(super) launch: Vec<LaunchWord>,
    #[cfg(not(target_os = "macos"))]
    pub(super) seen: std::time::Instant,
    #[cfg(not(target_os = "macos"))]
    pub(super) settled: std::sync::atomic::AtomicBool,
    #[cfg(all(test, not(target_os = "macos")))]
    pub(super) rehashes: std::sync::atomic::AtomicUsize,
}

impl RegisteredNativeAuthHost {
    fn open(path: &Path) -> Result<File, EngineError> {
        if !path.is_absolute() {
            return Err(EngineError("native_auth_host_executable_unsafe".into()));
        }
        rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(io_error)
    }

    pub(super) fn hash(file: &File, length: u64) -> Result<[u8; 32], EngineError> {
        assert!((1..=EXECUTABLE_BYTES_MAX).contains(&length));
        let mut digest = Sha256::new();
        let mut buffer = [0; 16 * 1024];
        let mut offset = 0;
        while offset < length {
            let requested = (length - offset).min(buffer.len() as u64) as usize;
            let read = match file.read_at(&mut buffer[..requested], offset) {
                Ok(read) => read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(io_error(error)),
            };
            if read == 0 {
                return Err(EngineError("native_auth_host_executable_changed".into()));
            }
            offset += read as u64;
            assert!(offset <= length);
            digest.update(&buffer[..read]);
        }
        Ok(digest.finalize().into())
    }

    pub(crate) fn capture(path: &Path) -> Result<Self, EngineError> {
        let file = Self::open(path)?;
        let identity = ExecutableIdentity::of(&file.metadata().map_err(io_error)?)?;
        #[cfg(not(target_os = "macos"))]
        let seen = std::time::Instant::now();
        let digest = Self::hash(&file, identity.length)?;
        if ExecutableIdentity::of(&file.metadata().map_err(io_error)?)? != identity
            || ExecutableIdentity::of(&std::fs::symlink_metadata(path).map_err(io_error)?)?
                != identity
        {
            return Err(EngineError("native_auth_host_executable_changed".into()));
        }
        Ok(Self {
            path: path.to_path_buf(),
            file,
            identity,
            digest,
            entry: None,
            retimed: std::sync::Mutex::new(None),
            launch: Vec::new(),
            #[cfg(not(target_os = "macos"))]
            seen,
            #[cfg(not(target_os = "macos"))]
            settled: std::sync::atomic::AtomicBool::new(false),
            #[cfg(all(test, not(target_os = "macos")))]
            rehashes: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Captures a harness the engine found through a PATH entry: the entry, often a link
    /// into a versioned install, is resolved to its real file now and that file is pinned
    /// as `capture` pins it; the entry is kept for `current`. Input: the absolute entry.
    /// Output: the host, or `native_auth_host_unavailable` when no file is behind the entry.
    pub(crate) fn capture_entry(entry: &Path) -> Result<Self, EngineError> {
        let real = crate::paths::launch_executable(entry, std::env::var_os("PATH").as_deref())
            .map_err(|_| EngineError("native_auth_host_unavailable".into()))?;
        Self::captured_as(entry, &real)
    }

    /// Captures a harness found through an entry as `capture_entry` does, once `check` passes
    /// the real file pinned, and only while the entry still leads to that file after the
    /// capture, so a link in the entry's chain moved after the entry was checked never gets
    /// an unchecked file pinned. Inputs: the absolute entry and the check. Output: the host,
    /// the check's refusal, `native_auth_host_executable_changed`, or as `capture_entry`.
    pub(crate) fn capture_entry_checked(
        entry: &Path,
        check: impl Fn(&Path) -> Result<(), EngineError>,
    ) -> Result<Self, EngineError> {
        let real = crate::paths::launch_executable(entry, std::env::var_os("PATH").as_deref())
            .map_err(|_| EngineError("native_auth_host_unavailable".into()))?;
        Self::captured_checked(entry, &real, check)
    }

    /// Captures the real file an entry leads to, pinned as `capture` pins it, with the entry
    /// kept for `current`. Inputs: the absolute entry and its real file. Output: the host, or
    /// the capture refusal.
    fn captured_as(entry: &Path, real: &Path) -> Result<Self, EngineError> {
        let mut host = Self::capture(real)?;
        assert!(host.entry.is_none());
        host.entry = Some(entry.to_path_buf());
        Ok(host)
    }

    /// Captures the real file an entry led to as `captured_as` does, once `check` passes
    /// that very file, and refuses the pin when the entry no longer leads to the file pinned,
    /// so the file checked, the file pinned and the file the entry leads to are one file
    /// (RV-DS241-140). Inputs: the absolute entry, its real file and the check. Output: the
    /// host, the check's or capture's refusal, or `native_auth_host_executable_changed`.
    fn captured_checked(
        entry: &Path,
        real: &Path,
        check: impl Fn(&Path) -> Result<(), EngineError>,
    ) -> Result<Self, EngineError> {
        assert!(real.is_absolute());
        check(real)?;
        let host = Self::captured_as(entry, real)?;
        assert_eq!(host.path, real);
        let now = crate::paths::launch_executable(entry, std::env::var_os("PATH").as_deref());
        if now.ok().as_deref() != Some(real) {
            return Err(EngineError("native_auth_host_executable_changed".into()));
        }
        Ok(host)
    }

    /// The host while its entry, if any, still leads to the pinned file and that file
    /// verifies, a change of its ctime alone included; with an entry, a fresh capture when it
    /// leads to another path or inode, or when other metadata of the pinned inode changed and
    /// its bytes still hash to the pin, once `trusted` passes the entry the launch now goes
    /// through and the real file pinned, so a re-pin runs the checks of engine start on the
    /// file it pins. A rewrite in place stays refused. Inputs: the shared host and the
    /// engine-start check. Output: the host to use, the verify error, the check's refusal,
    /// `native_auth_host_executable_changed`, or `native_auth_host_unavailable`.
    pub(crate) fn current(
        self: &Arc<Self>,
        trusted: impl Fn(&Path) -> Result<(), EngineError>,
    ) -> Result<Arc<Self>, EngineError> {
        let Some(entry) = self.entry.as_deref() else {
            self.verified_path()?;
            return Ok(self.clone());
        };
        let unavailable = |_| EngineError("native_auth_host_unavailable".into());
        let used = crate::paths::launch_entry(entry, std::env::var_os("PATH").as_deref())
            .map_err(unavailable)?;
        let real = std::fs::canonicalize(&used).map_err(unavailable)?;
        if real == self.path && self.verified_path().is_ok() {
            return Ok(self.clone());
        }
        trusted(&used)?;
        let fresh = Self::captured_checked(entry, &real, &trusted)?;
        assert_eq!(fresh.entry.as_deref(), Some(entry));
        if fresh.path == self.path
            && (fresh.identity.device, fresh.identity.inode)
                == (self.identity.device, self.identity.inode)
            && fresh.digest != self.digest
        {
            return Err(EngineError("native_auth_host_executable_changed".into()));
        }
        Ok(Arc::new(fresh))
    }

    /// Whether the harness this host pins is gone from disk: the entry a launch resolves (the
    /// pinned path for a host with no entry) leads to nothing, so it answers NotFound. A file
    /// that is there but changed or unsafe is not gone and stays refused. Input: none.
    /// Output: true when the entry leads to nothing.
    pub(crate) fn vanished(&self) -> bool {
        let named = self.entry.as_deref().unwrap_or(&self.path);
        matches!(std::fs::metadata(named), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    }

    /// The PATH entry this host was captured through, which a launch resolves again each
    /// time. Input: none. Output: the entry, or none for a host captured by its real path.
    pub(crate) fn entry(&self) -> Option<&Path> {
        self.entry.as_deref()
    }

    /// Returns the captured path while the retained file, the file at that path and the path
    /// show the pinned identity; a change of the change time alone is hashed once and adopted
    /// ([`Self::retime`]). macOS moves ctime on every write by the fine clock, so equal identity
    /// means the hashed bytes; off macOS the coarse tick can hide a same-size write, so there
    /// the file is hashed again until a match [`settles`] it. Output: the path, or the refusal.
    pub(crate) fn verified_path(&self) -> Result<&Path, EngineError> {
        let pin = self.pin();
        let named = Self::open(&self.path)?;
        let seen = self.identities(&named)?;
        if seen != [pin.identity; 3] {
            self.retime(pin.identity, &named, seen)?;
            return Ok(&self.path);
        }
        #[cfg(not(target_os = "macos"))]
        if !self.settled.load(std::sync::atomic::Ordering::Acquire) {
            let started = std::time::Instant::now();
            let wall = std::time::SystemTime::now();
            #[cfg(test)]
            self.rehashes
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if crate::engine::off_worker(|| Self::hash(&self.file, pin.identity.length))?
                != self.digest
            {
                return Err(EngineError("native_auth_host_executable_changed".into()));
            }
            if settles(pin.seen, started, wall, pin.identity.changed) {
                let held = self
                    .retimed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if held.map_or(self.identity, |kept| kept.identity) == pin.identity {
                    self.settled
                        .store(true, std::sync::atomic::Ordering::Release);
                }
            }
        }
        Ok(&self.path)
    }

    /// The identity the checks compare against now: the latest one [`Self::retime`] proved,
    /// else the captured one, with when the checks first saw it off macOS. Input: none.
    /// Output: the pin.
    fn pin(&self) -> Pin {
        let retimed = *self
            .retimed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        retimed.unwrap_or(Pin {
            identity: self.identity,
            #[cfg(not(target_os = "macos"))]
            seen: self.seen,
        })
    }

    /// The identities of the retained file, the file opened at the path and the path itself,
    /// in that order. Input: the file opened at the path. Output: the three, or
    /// `native_auth_host_executable_unsafe` or the read error for the first that fails.
    fn identities(&self, named: &File) -> Result<[ExecutableIdentity; 3], EngineError> {
        Ok([
            ExecutableIdentity::of(&self.file.metadata().map_err(io_error)?)?,
            ExecutableIdentity::of(&named.metadata().map_err(io_error)?)?,
            ExecutableIdentity::of(&std::fs::symlink_metadata(&self.path).map_err(io_error)?)?,
        ])
    }

    /// Adopts what the pinned inode shows now when all three looks agree and differ from the
    /// pin in ctime alone, as a hard link, an xattr or a backup tool leaves it, and its bytes
    /// still hash to the pin with the same three looks after the hash. Anything else, a write
    /// during the hash included, is refused. Inputs: the pin, the file at the path and the
    /// three looks. Output: Ok once adopted, or `native_auth_host_executable_changed`.
    fn retime(
        &self,
        pin: ExecutableIdentity,
        named: &File,
        seen: [ExecutableIdentity; 3],
    ) -> Result<(), EngineError> {
        let changed = || EngineError("native_auth_host_executable_changed".into());
        let now = seen[0];
        let others = ExecutableIdentity {
            changed: pin.changed,
            ..now
        };
        if seen != [now; 3] || others != pin {
            return Err(changed());
        }
        assert_ne!(now.changed, pin.changed);
        assert_eq!(now.length, self.identity.length);
        if crate::engine::off_worker(|| Self::hash(&self.file, now.length))? != self.digest
            || self.identities(named)? != [now; 3]
        {
            return Err(changed());
        }
        let mut held = self
            .retimed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *held = Some(Pin {
            identity: now,
            #[cfg(not(target_os = "macos"))]
            seen: std::time::Instant::now(),
        });
        #[cfg(not(target_os = "macos"))]
        self.settled
            .store(false, std::sync::atomic::Ordering::Release);
        drop(held);
        Ok(())
    }

    pub(crate) fn verified_sha256(&self) -> Result<String, EngineError> {
        self.verified_path()?;
        Ok(self
            .digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }
}
