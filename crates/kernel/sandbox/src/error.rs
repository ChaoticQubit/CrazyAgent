use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Refusal to build or install a project sandbox: `Invalid` for an unsafe path or policy,
/// `Unavailable` with a plain reason, such as a missing Windows helper or loopback limit,
/// `Unreadable` for a Windows read root no app container may read and this user cannot change, or
/// `Io`.
#[derive(Debug)]
pub enum SandboxError {
    Invalid(String),
    Unavailable(String),
    Unreadable(PathBuf),
    Io(io::Error),
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "sandbox refused: {reason}"),
            Self::Unavailable(reason) => write!(f, "sandbox unavailable: {reason}"),
            Self::Unreadable(folder) => {
                write!(f, "sandbox unavailable: {}", unreadable_reason(folder))
            }
            Self::Io(error) => write!(f, "sandbox I/O: {error}"),
        }
    }
}

/// Why a Windows read root refuses a launch, in plain words naming the folder: no app
/// container may read it and this Windows user cannot change who may. Input: the folder.
/// Output: the reason.
pub fn unreadable_reason(folder: &Path) -> String {
    format!(
        "Windows does not let sandboxed programs read {}, and this Windows user cannot change who reads it",
        folder.display()
    )
}

impl std::error::Error for SandboxError {}

impl From<io::Error> for SandboxError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// The refusal for an unsafe path, policy or value, in the plain words given. Input: the
/// reason. Output: [`SandboxError::Invalid`] carrying it.
pub fn invalid(reason: &str) -> SandboxError {
    SandboxError::Invalid(reason.into())
}
