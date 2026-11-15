use super::launch::{LaunchError, failure};
use super::manifest::{Manifest, Refused};
use crate::protocol::CredentialRef;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The most environment variables one manifest's `home.env` sets.
pub const ENV_MAX: usize = super::manifest::LIST_MAX;

/// What a manifest's `home` block asks of the private home a harness runs in: the
/// environment variables it sets, each value plain text or a folder under `{home}`, and
/// whether HOME itself is set to the private home. `owner_folders` is read by the manifest
/// reader and the sandbox.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Home {
    pub env: Vec<(String, String)>,
    pub set_home: bool,
}

impl Home {
    /// The environment one launch gets from the home block. Input: the private home.
    /// Output: each `home.env` name with `{home}` filled in, then HOME when the block asks
    /// for it.
    pub fn environment(&self, home: &Path) -> Vec<(OsString, OsString)> {
        let _ = home;
        Vec::new()
    }
}

/// Reads the `home` block of one manifest. Input: the manifest. Output: what it asks, the
/// default for a manifest with no block, or the refusal that names the setting.
pub fn read(manifest: &Manifest) -> Result<Home, Refused> {
    let _ = manifest;
    Ok(Home::default())
}

/// Makes the private home one harness runs in for one account, `<base>/<harness id>/<account>`,
/// each folder 0700 and owned by this user, made through no-follow steps. Inputs: the
/// agent's own 0700 home, a harness id or alias and the account. Output: the home, or the
/// refusal.
pub fn prepare(
    base: &Path,
    harness: &str,
    account: &CredentialRef,
) -> Result<PathBuf, LaunchError> {
    let _ = (base, harness, account);
    Err(failure("launch_harness_unsupported"))
}

#[cfg(all(test, unix))]
#[path = "home_tests.rs"]
mod tests;
