use crate::engine::{EngineError, io_error};
use rustix::fs::{Mode, OFlags};
use sha2::{Digest, Sha256};
use std::fs::{File, Metadata};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};

const EXECUTABLE_BYTES_MAX: u64 = 256 * 1024 * 1024;

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
    fn of(metadata: &Metadata) -> Result<Self, EngineError> {
        if !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
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

pub(crate) struct RegisteredNativeAuthHost {
    path: PathBuf,
    file: File,
    identity: ExecutableIdentity,
    digest: [u8; 32],
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

    fn hash(file: &File, length: u64) -> Result<[u8; 32], EngineError> {
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
        })
    }

    pub(crate) fn verified_path(&self) -> Result<&Path, EngineError> {
        let named = Self::open(&self.path)?;
        if ExecutableIdentity::of(&self.file.metadata().map_err(io_error)?)? != self.identity
            || ExecutableIdentity::of(&named.metadata().map_err(io_error)?)? != self.identity
            || Self::hash(&self.file, self.identity.length)? != self.digest
            || ExecutableIdentity::of(&self.file.metadata().map_err(io_error)?)? != self.identity
            || ExecutableIdentity::of(&std::fs::symlink_metadata(&self.path).map_err(io_error)?)?
                != self.identity
        {
            return Err(EngineError("native_auth_host_executable_changed".into()));
        }
        Ok(&self.path)
    }
}

pub(super) mod custody;
#[cfg(test)]
mod custody_session_tests;
#[cfg(test)]
mod custody_tests;
pub(in crate::engine) mod entry;
#[cfg(test)]
mod entry_tests;
#[cfg(test)]
mod executable_tests;
