use super::{InstallRecipe, InstallResult};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The most bytes of a download source.
pub const SOURCE_BYTES_MAX: usize = 2048;

/// The folders and files the engine picks for one install run, and the download source and
/// archive member from its pins. Every field a variant does not use is unset. The engine
/// sets them from its own setup layout, never from a plugin.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallPaths {
    pub program: Option<PathBuf>,
    pub archive: Option<PathBuf>,
    pub out: Option<PathBuf>,
    pub source: Option<String>,
    pub member: Option<String>,
    pub venv: Option<PathBuf>,
    pub venv_python: Option<PathBuf>,
    pub requirements: Option<PathBuf>,
    pub npm_prefix: Option<PathBuf>,
}

/// One program run of an install: the absolute program, its arguments in order, and the
/// environment the template fixes on top of the caller's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(&'static str, OsString)>,
}

/// Builds the runs of one recipe from its variant's fixed template.
/// Inputs: the recipe and the engine's paths for this run.
/// Output: the steps in order, or the refusal code.
pub fn steps(recipe: &InstallRecipe, paths: &InstallPaths) -> Result<Vec<Step>, &'static str> {
    super::validate(recipe)?;
    let _ = paths;
    Ok(Vec::new())
}

/// Runs one step with no input and a time bound.
/// Inputs: the step, the caller's base environment, the working folder and the bound.
/// Output: the exit as 0 or 1 and the bounded standard output, or the refusal code.
#[cfg(unix)]
pub fn run(
    step: &Step,
    base: &[(&str, OsString)],
    cwd: &Path,
    limit: std::time::Duration,
) -> Result<InstallResult, &'static str> {
    let _ = (step, base, cwd, limit);
    Err(super::TEMPLATE_MISMATCH)
}

#[cfg(test)]
mod tests;
