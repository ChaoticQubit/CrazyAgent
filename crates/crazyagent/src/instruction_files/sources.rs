use crate::engine::EngineError;
use crate::harnesses::registry::Registry;
use crate::tools::Places;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One harness's own instruction file in the owner's home that the owner may import once
/// into the app-wide instructions (DEC-106): the harness's catalog id and name, the file as
/// its manifest's `import.instructions` names it under the home, and its size in bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub harness: String,
    pub name: String,
    pub path: String,
    pub bytes: u64,
}

/// Lists, in catalog order, each harness whose manifest names an `import.instructions` file
/// that is there in the owner's home; reads no file. Inputs: the owner's home and the
/// registry. Output: the sources, at most one per harness.
pub fn sources(_home: &Path, _registry: &Registry) -> Vec<Source> {
    Vec::new()
}

/// Reads one harness's instruction file for the owner's import preview, on the owner's click
/// only: the path comes from the harness's manifest, never from a client, and is read as a
/// picked file is ([`super::read_picked`]), refused when it or a link on its way lies where
/// agents can write. Inputs: the owner's home, the registry, the places agents can write and
/// the harness id. Output: the text, or a `PROCESS_CODES` refusal.
pub fn read(
    _home: &Path,
    _registry: &Registry,
    _places: &Places,
    _harness: &str,
) -> Result<String, EngineError> {
    Err(EngineError("process_request_invalid".into()))
}

#[cfg(all(test, unix))]
#[path = "sources_tests.rs"]
mod tests;
