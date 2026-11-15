use super::launch::{LaunchError, failure};
use super::manifest::{Manifest, Refused};
use crate::plugins::LaunchServer;
use std::path::{Path, PathBuf};

/// How a harness reads the MCP servers it is given. `json-mcp-servers` is one JSON file
/// holding an `mcpServers` map, the shape Claude Code reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpRender {
    JsonMcpServers,
}

/// How a harness reads its instruction text. `file` is the whole text as one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionsRender {
    File,
}

/// One file a manifest asks for: how it is written, and where, relative to the private
/// home, as plain folder names joined by `/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Render<K> {
    pub kind: K,
    pub path: String,
}

/// The files a manifest's `mcp` and `instructions` blocks ask for, none for a block it
/// leaves out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Renders {
    pub mcp: Option<Render<McpRender>>,
    pub instructions: Option<Render<InstructionsRender>>,
}

/// The text formats a template is written in, each with its own way to quote a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    Toml,
    Yaml,
}

/// Reads the `mcp` and `instructions` blocks of one manifest. Input: the manifest. Output:
/// the files they ask for, or the refusal that names the setting.
pub fn read(manifest: &Manifest) -> Result<Renders, Refused> {
    let _ = manifest;
    Ok(Renders::default())
}

/// Writes the files a manifest asks for into one launch's private home. Inputs: the files,
/// the private home, the MCP servers the launch gives and its instruction text. Output: the
/// paths written, or the refusal.
pub fn write(
    renders: &Renders,
    home: &Path,
    servers: &[LaunchServer],
    instructions: &str,
) -> Result<Vec<PathBuf>, LaunchError> {
    let _ = (renders, home, servers, instructions);
    Ok(Vec::new())
}

/// Fills a template's placeholders, each value quoted for the template's format so it stays
/// one string value there. Inputs: the template, its format and the values by placeholder
/// name. Output: the text, or the refusal.
pub fn fill(
    template: &str,
    format: Format,
    values: &[(&str, &str)],
) -> Result<String, LaunchError> {
    let _ = (template, format, values);
    Err(failure("launch_harness_unsupported"))
}

#[cfg(all(test, unix))]
#[path = "render_tests.rs"]
mod tests;
