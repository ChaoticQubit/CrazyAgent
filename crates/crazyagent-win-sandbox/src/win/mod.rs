mod acl;
mod container;
mod job;
mod launch;
mod record;

use crate::plan::plan;
use crate::profile::{BoundaryProfile, ProfileError};
use crate::request::Request;
use std::fmt;
use std::path::Path;
use windows_sys::Win32::Foundation::GetLastError;

/// Why the boundary did not start the agent: a refused profile, a refused launch with a plain
/// reason, or a Windows call that failed at a named step with its error code.
#[derive(Debug)]
pub enum BoundaryError {
    Profile(ProfileError),
    Refused(&'static str),
    Windows { step: &'static str, code: u32 },
}

impl fmt::Display for BoundaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Profile(error) => write!(f, "{error}"),
            Self::Refused(reason) => write!(f, "agent not started: {reason}"),
            Self::Windows { step, code } => {
                write!(
                    f,
                    "agent not started: could not {step} (Windows error {code})"
                )
            }
        }
    }
}

impl std::error::Error for BoundaryError {}

fn windows(step: &'static str) -> impl FnOnce(u32) -> BoundaryError {
    move |code| BoundaryError::Windows { step, code }
}

/// Starts one agent inside its boundary and waits for it: reads the profile, opens the agent's
/// AppContainer, takes back the access entries an earlier profile of that container granted
/// and this one does not, puts the planned entries on its roots and protected paths, starts
/// the program suspended in that container with the profile's capabilities, places it in a
/// job that kills every process left when the helper exits, then resumes it. Nothing starts
/// when any step fails. Input: the request. Output: the agent's exit code, or why it did not
/// start.
pub fn run(request: &Request) -> Result<u32, BoundaryError> {
    let profile = BoundaryProfile::load(&request.profile).map_err(BoundaryError::Profile)?;
    let aces = plan(&profile, acl::present).map_err(BoundaryError::Refused)?;
    let container = container::open(&profile.container)?;
    record::Record::open(&profile.grants, &profile.container)?.reconcile(&container, &aces)?;
    let capabilities = container::capabilities(&profile.capabilities)?;
    let job = job::Job::new(profile.active_processes)?;
    launch::start(request, &container, &capabilities, &job)
}

/// Takes back every access entry the boundary recorded for the profile's container and removes
/// the record, as when the agent's profile is removed; a later launch of that container grants
/// afresh. Input: the profile file. Output: Ok, or why it stopped, with the record kept so a
/// retry withdraws what is left.
pub fn release(profile: &Path) -> Result<(), BoundaryError> {
    let profile = BoundaryProfile::load(profile).map_err(BoundaryError::Profile)?;
    plan(&profile, |_| true).map_err(BoundaryError::Refused)?;
    let container = container::sid(&profile.container)?;
    record::Record::open(&profile.grants, &profile.container)?.release(&container)
}

/// Bytes Windows wrote, such as a SID or an access list, kept in 4-byte words so they stay
/// aligned when passed back to Windows.
pub(crate) struct Words {
    words: Vec<u32>,
    len: usize,
}

impl Words {
    fn from_bytes(bytes: &[u8]) -> Self {
        let mut words = vec![0u32; bytes.len().div_ceil(4)];
        for (word, chunk) in words.iter_mut().zip(bytes.chunks(4)) {
            let mut four = [0u8; 4];
            four[..chunk.len()].copy_from_slice(chunk);
            *word = u32::from_ne_bytes(four);
        }
        Self {
            words,
            len: bytes.len(),
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let mut bytes: Vec<u8> = self
            .words
            .iter()
            .flat_map(|word| word.to_ne_bytes())
            .collect();
        bytes.truncate(self.len);
        bytes
    }

    /// Copies `len` bytes Windows wrote at `source` into owned words.
    /// Safety: `source` must point at `len` readable bytes that stay valid for this call.
    unsafe fn copy_from(source: *const u8, len: usize) -> Self {
        let bytes = unsafe { std::slice::from_raw_parts(source, len) };
        Self::from_bytes(bytes)
    }
}

fn wide(text: &str) -> Vec<u16> {
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    assert_eq!(units.iter().filter(|unit| **unit == 0).count(), 1);
    units
}

/// The calling thread's last Windows error code. Relies on being called right after the
/// failed call, before anything else on this thread can change it.
fn last_error() -> u32 {
    unsafe { GetLastError() }
}
