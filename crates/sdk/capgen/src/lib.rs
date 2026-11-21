#![forbid(unsafe_code)]

use std::fs;
use std::path::Path;

use crazyagent_cap::File;

/// The most `.cap` files one SDK may hold.
pub const CAP_FILES_MAX: usize = 64;

/// Why capgen refused: the `.cap` file, the refusal code and what was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapgenError {
    pub file: String,
    pub code: &'static str,
    pub detail: String,
}

/// Writes the Rust SDK for the parsed `.cap` files, deterministic for the same input.
/// Input: each file's stem (its module name) with its parsed file, any order.
/// Output: each generated file's path below `src/generated` with its text, sorted by path,
/// or the first refusal (`capgen_reserved_name`, `capgen_name_collision`, ...).
pub fn rust(files: &[(&str, File)]) -> Result<Vec<(String, String)>, CapgenError> {
    rust::files(files)
}

/// Reads and parses every `.cap` file of a folder. Input: the folder.
/// Output: each file's stem with its parsed file, sorted by stem, or the first refusal.
pub fn load(caps: &Path) -> Result<Vec<(String, File)>, CapgenError> {
    let refuse = |file: &str, code: &'static str, detail: String| CapgenError {
        file: file.to_owned(),
        code,
        detail,
    };
    let entries = fs::read_dir(caps).map_err(|error| {
        refuse(
            &caps.display().to_string(),
            "capgen_read",
            error.to_string(),
        )
    })?;
    let mut stems = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| refuse("", "capgen_read", error.to_string()))?
            .path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("cap") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            return Err(refuse(
                &path.display().to_string(),
                "capgen_file_name",
                String::new(),
            ));
        };
        stems.push(stem.to_owned());
    }
    stems.sort();
    if stems.len() > CAP_FILES_MAX {
        return Err(refuse("", "capgen_too_many_files", stems.len().to_string()));
    }
    let mut loaded = Vec::with_capacity(stems.len());
    for stem in stems {
        let name = format!("{stem}.cap");
        let source = fs::read_to_string(caps.join(&name))
            .map_err(|error| refuse(&name, "capgen_read", error.to_string()))?;
        let file = crazyagent_cap::parse(&source).map_err(|error| {
            refuse(
                &name,
                error.code,
                format!("line {} column {}", error.line, error.column),
            )
        })?;
        loaded.push((stem, file));
    }
    Ok(loaded)
}

/// Compares generated files with a folder's files. Inputs: the folder and the generated
/// files. Output: every path that differs, is missing or is extra in the folder, sorted.
pub fn drift(generated: &Path, files: &[(String, String)]) -> Vec<String> {
    let mut differing = Vec::new();
    for (path, text) in files {
        if fs::read_to_string(generated.join(path)).ok().as_deref() != Some(text.as_str()) {
            differing.push(path.clone());
        }
    }
    if let Ok(entries) = fs::read_dir(generated) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !files.iter().any(|(path, _)| *path == name) {
                differing.push(name);
            }
        }
    }
    differing.sort();
    differing
}

mod rust;
#[cfg(test)]
mod tests;
