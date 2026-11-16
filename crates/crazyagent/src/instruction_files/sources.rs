use crate::app_plugins::yaml::Value;
use crate::engine::EngineError;
use crate::harnesses::registry::{HARNESSES_MAX, Registry};
use crate::tools::Places;
use serde::{Deserialize, Serialize};
use std::fs;
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
pub fn sources(home: &Path, registry: &Registry) -> Vec<Source> {
    let found: Vec<Source> = registry
        .all()
        .iter()
        .filter_map(|harness| {
            let manifest = harness.manifest();
            let path = file_of(registry, &manifest.id)?;
            let held = fs::metadata(home.join(path)).ok().filter(fs::Metadata::is_file)?;
            Some(Source {
                harness: manifest.id.clone(),
                name: manifest.name.clone(),
                path: path.to_owned(),
                bytes: held.len(),
            })
        })
        .collect();
    assert!(found.len() <= HARNESSES_MAX);
    found
}

/// The instruction file a harness's manifest names in `import.instructions`, under the
/// owner's home, as the registry checked it (inside one of its `home.owner_folders`, no `~`,
/// `.` or `..` part). Inputs: the registry and the harness id. Output: the path, or none for
/// an unknown harness or one that names no instruction file.
pub fn file_of<'a>(registry: &'a Registry, harness: &str) -> Option<&'a str> {
    let manifest = registry
        .get(harness)
        .map(|found| found.manifest())
        .filter(|manifest| manifest.id == harness)?;
    match manifest.block("import")?.get("instructions")? {
        Value::Str(path) => Some(path.as_str()),
        _ => None,
    }
}

/// Reads one harness's instruction file for the owner's import preview, on the owner's click
/// only: the path comes from the harness's manifest, never from a client, and is read as a
/// picked file is ([`super::read_picked`]), refused when it or a link on its way lies where
/// agents can write. Inputs: the owner's home, the registry, the places agents can write and
/// the harness id. Output: the text, or a `PROCESS_CODES` refusal.
pub fn read(
    home: &Path,
    registry: &Registry,
    places: &Places,
    harness: &str,
) -> Result<String, EngineError> {
    read_between(home, registry, places, harness, || {})
}

/// [`read`], with a step run after the file is found and before it is checked and read, so
/// a row can swap a link in that window. Inputs: as [`read`] and the step. Output: as
/// [`read`].
fn read_between(
    home: &Path,
    registry: &Registry,
    places: &Places,
    harness: &str,
    between: impl FnOnce(),
) -> Result<String, EngineError> {
    let Some(relative) = file_of(registry, harness) else {
        return Err(EngineError("process_request_invalid".into()));
    };
    let path = home.join(relative);
    let found = crate::harnesses::finding::link_hops(&path);
    between();
    let Some(hops) = found else {
        return match fs::canonicalize(&path) {
            Ok(_) => Err(EngineError("process_import_unsafe".into())),
            Err(_) => super::read_picked(&path),
        };
    };
    if places.agent_writable(&path) || hops.iter().any(|hop| places.agent_writable(hop)) {
        return Err(EngineError("process_import_agents_can_write".into()));
    }
    super::read_picked(&path)
}

#[cfg(all(test, unix))]
#[path = "sources_tests.rs"]
mod tests;
