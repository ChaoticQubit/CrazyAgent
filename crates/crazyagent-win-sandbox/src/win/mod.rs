mod acl;
mod container;
mod file_id;
mod job;
mod launch;
mod listeners;
mod name;
mod owner;
pub(crate) mod private;
mod record;
mod tree;

pub use file_id::file_id;
pub use listeners::listener_beside;
pub use name::final_name;
pub use owner::{changeable_part, owner_only, unreadable_read_root, unreadable_root};
pub use tree::ProcessTree;

use crate::plan::{nested_read_roots, plan};
use crate::profile::{BoundaryProfile, ProfileError};
use crate::request::Request;
use std::fmt;
use std::path::Path;
use windows_sys::Win32::Foundation::GetLastError;

/// Why the boundary did not start the agent: a refused profile, a refused launch with a plain
/// reason, a Windows call that failed at a named step with its error code, an access list
/// that could not be read or changed, with the path it belongs to (V244-WIN-NPM), or a read
/// root the grant around it does not reach, with its path (V244-WIN-NESTED-READ-ROOT).
#[derive(Debug)]
pub enum BoundaryError {
    Profile(ProfileError),
    Refused(&'static str),
    Windows {
        step: &'static str,
        code: u32,
    },
    AccessList {
        step: &'static str,
        path: String,
        code: u32,
    },
    Unreached(String),
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
            Self::AccessList { step, path, code } => {
                write!(
                    f,
                    "agent not started: could not {step} of {path} (Windows error {code})"
                )
            }
            Self::Unreached(path) => write!(
                f,
                "agent not started: app containers cannot read the read root {path}, and the grant on the folder around it does not reach it"
            ),
        }
    }
}

impl std::error::Error for BoundaryError {}

fn windows(step: &'static str) -> impl FnOnce(u32) -> BoundaryError {
    move |code| BoundaryError::Windows { step, code }
}

/// The failure of an access-list step on one path, so the refusal names the folder an owner
/// would look at. Inputs: the step and the path. Output: the mapping from the Windows error.
fn access_list(step: &'static str, path: &str) -> impl FnOnce(u32) -> BoundaryError {
    let path = path.to_owned();
    move |code| BoundaryError::AccessList { step, path, code }
}

/// Starts one agent inside its boundary and waits for it: takes back what an earlier profile of
/// its AppContainer granted and this one does not, puts the planned entries in place, refuses a
/// read root the grant around it does not reach ([`reach`]), then starts the program suspended
/// in that container, in a job that kills what is left when the helper exits, and resumes it.
/// Input: the request. Output: the agent's exit code, or why nothing started.
pub fn run(request: &Request) -> Result<u32, BoundaryError> {
    let profile = BoundaryProfile::load(&request.profile).map_err(BoundaryError::Profile)?;
    let aces = plan(&profile, acl::present).map_err(BoundaryError::Refused)?;
    let container = container::open(&profile.container)?;
    record::Record::open(&profile.grants, &profile.container)?.reconcile(&container, &aces)?;
    reach(&profile, &container)?;
    let capabilities = container::capabilities(&profile.capabilities)?;
    let job = job::Job::new(profile.active_processes)?;
    launch::start(request, &container, &capabilities, &job)
}

/// Refuses, by name, the first read root the helper does not grant itself
/// ([`nested_read_roots`]) whose list lets neither every app container nor this one read it
/// once the grant is written. A missing one, a link or a junction is skipped, as the plan skips
/// such a guarded path (V244-WIN-NESTED-READ-ROOT). Inputs: the profile and the container SID.
/// Output: Ok, or the refusal or the failed read.
fn reach(profile: &BoundaryProfile, container: &Words) -> Result<(), BoundaryError> {
    for root in nested_read_roots(profile) {
        if acl::present(root) && !acl::reached(container, root)? {
            return Err(BoundaryError::Unreached(root.to_owned()));
        }
    }
    Ok(())
}

/// Takes back every access entry the boundary recorded for the profile's container, removes
/// the record, then deletes the container's AppContainer profile and its folder; a later launch
/// grants afresh. A profile naming another container than the caller derived is refused first.
/// Inputs: the profile file and that container. Output: Ok, or why it stopped, with the record
/// and container kept.
pub fn release(profile: &Path, container: &str) -> Result<(), BoundaryError> {
    let profile = BoundaryProfile::load(profile).map_err(BoundaryError::Profile)?;
    if profile.container != container {
        return Err(BoundaryError::Refused(
            "the profile names another container than the engine derives for it",
        ));
    }
    plan(&profile, |_| true).map_err(BoundaryError::Refused)?;
    let sid = container::sid(&profile.container)?;
    record::Record::open(&profile.grants, &profile.container)?.release(&sid)?;
    container::delete(&profile.container)
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
