use super::pack::PackManifestEntry;
use super::{InstallError, Installer, PLATFORM_UNSUPPORTED};
use crate::plugins::ServerSpec;
use std::path::Path;

/// Sets up how one MCP server entry starts and the name it registers under. Not written
/// yet: every entry is refused. Inputs: the installer, the entry and its checkout folder,
/// none where setup fetches no checkout. Output: `setup_platform_unsupported`.
pub fn pack_server(
    installer: &dyn Installer,
    entry: &PackManifestEntry,
    folder: Option<&Path>,
) -> Result<(String, ServerSpec), InstallError> {
    let _ = (installer, entry, folder);
    Err(InstallError(PLATFORM_UNSUPPORTED))
}

/// Sets up one pack entry where setup fetches no checkout. Not written yet: every entry is
/// refused. Inputs: the installer and the entry. Output: `setup_platform_unsupported`.
#[cfg(any(not(unix), test))]
pub fn server_only(
    installer: &dyn Installer,
    entry: &PackManifestEntry,
) -> Result<(String, (String, ServerSpec)), InstallError> {
    let _ = (installer, entry);
    Err(InstallError(PLATFORM_UNSUPPORTED))
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
