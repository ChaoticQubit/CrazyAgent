use crazyagent_cap::File;

use crate::CapgenError;

/// Writes the Rust SDK files for the parsed `.cap` files. Input: stems with parsed files.
/// Output: the generated files sorted by path, or the first refusal.
pub(crate) fn files(files: &[(&str, File)]) -> Result<Vec<(String, String)>, CapgenError> {
    let _ = files;
    Ok(Vec::new())
}
