use super::launch::{LaunchError, failure};
use super::manifest::{self, Manifest, Refused};
use crate::app_plugins::yaml::Value;
use crate::plugins::LaunchServer;
use std::path::{Path, PathBuf};

/// The most folder names in a render path, the file's own name included.
const PATH_PARTS_MAX: usize = 8;
/// The most bytes of one template.
const TEMPLATE_BYTES_MAX: usize = 16 * 1024;
/// The start of a render path: the private home, then a file inside it.
const HOME_PREFIX: &str = "{home}/";

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

const MCP_RENDERS: [(&str, McpRender); 1] = [("json-mcp-servers", McpRender::JsonMcpServers)];
const INSTRUCTIONS_RENDERS: [(&str, InstructionsRender); 1] = [("file", InstructionsRender::File)];

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

fn refuse<T>(code: &'static str, message: String) -> Result<T, Refused> {
    Err(Refused { code, message })
}

/// Reads the `mcp` and `instructions` blocks of one manifest. Input: the manifest. Output:
/// the files they ask for, or the refusal that names the setting.
pub fn read(manifest: &Manifest) -> Result<Renders, Refused> {
    let mcp = manifest
        .block("mcp")
        .map(|block| entry(block, "mcp", "MCP", &MCP_RENDERS))
        .transpose()?;
    let instructions = manifest
        .block("instructions")
        .map(|block| entry(block, "instructions", "instructions", &INSTRUCTIONS_RENDERS))
        .transpose()?;
    if let (Some(mcp), Some(instructions)) = (&mcp, &instructions)
        && mcp.path == instructions.path
    {
        return refuse(
            manifest::VALUE,
            "instructions.path: mcp.path already names this file.".into(),
        );
    }
    Ok(Renders { mcp, instructions })
}

/// Reads one render block: `render`, one of the kinds CrazyAgent writes, and `path`, a file
/// inside the private home. Inputs: the block, its key, the word its refusals name the kind
/// with and the kinds by name. Output: the render, or the refusal that names the setting.
fn entry<K: Copy>(
    block: &Value,
    key: &str,
    label: &str,
    kinds: &[(&str, K)],
) -> Result<Render<K>, Refused> {
    let Value::Map(entries) = block else {
        return refuse(
            manifest::TYPE,
            format!("{key}: this must be a block of settings."),
        );
    };
    let mut render = None;
    let mut path = None;
    for (name, value) in entries {
        let setting = format!("{key}.{name}");
        let slot = match name.as_str() {
            "render" => &mut render,
            "path" => &mut path,
            _ => {
                return refuse(
                    manifest::UNKNOWN_KEY,
                    format!("{setting}: this key is not one the {key} block has."),
                );
            }
        };
        let Value::Str(text) = value else {
            return refuse(
                manifest::TYPE,
                format!("{setting}: this must be text in quotes."),
            );
        };
        *slot = Some(text.clone());
    }
    let Some(render) = render else {
        return refuse(
            manifest::MISSING,
            format!("{key}.render: this setting is needed."),
        );
    };
    let Some(kind) = kinds
        .iter()
        .find(|(name, _)| *name == render)
        .map(|(_, kind)| *kind)
    else {
        return refuse(
            manifest::KIND,
            format!("{key}.render: CrazyAgent has no {label} render named {render}."),
        );
    };
    let Some(path) = path else {
        return refuse(
            manifest::MISSING,
            format!("{key}.path: this setting is needed."),
        );
    };
    let Some(path) = inside_home(&path) else {
        return refuse(
            manifest::VALUE,
            format!("{key}.path: this must be {{home}} followed by a file inside it."),
        );
    };
    Ok(Render { kind, path })
}

/// The part of a render path after `{home}/`: one to [`PATH_PARTS_MAX`] plain names, none
/// `.`, `..` or holding a brace. Input: the path as written. Output: that part, or none.
fn inside_home(path: &str) -> Option<String> {
    let rest = path.strip_prefix(HOME_PREFIX)?;
    let parts: Vec<&str> = rest.split('/').collect();
    let plain = parts.len() <= PATH_PARTS_MAX
        && parts.iter().all(|part| {
            !part.is_empty() && *part != "." && *part != ".." && !part.contains(['{', '}'])
        });
    plain.then(|| rest.to_owned())
}

/// Writes the files a manifest asks for into one launch's private home, each 0600, made new
/// through no-follow steps from 0700 folders, so a link planted in the home is replaced or
/// refused, never written through. Inputs: the files, the private home, the MCP servers the
/// launch gives and its instruction text. Output: the paths written, or the refusal.
#[cfg(unix)]
pub fn write(
    renders: &Renders,
    home: &Path,
    servers: &[LaunchServer],
    instructions: &str,
) -> Result<Vec<PathBuf>, LaunchError> {
    super::launch::validate_home(home)?;
    let mut written = Vec::with_capacity(2);
    if let Some(mcp) = &renders.mcp {
        let text = match mcp.kind {
            McpRender::JsonMcpServers => crate::launch_context::claude_config(servers)?,
        };
        written.push(put(home, &mcp.path, text)?);
    }
    if let Some(file) = &renders.instructions {
        let text = match file.kind {
            InstructionsRender::File => instructions.to_owned(),
        };
        written.push(put(home, &file.path, text)?);
    }
    assert!(
        written
            .iter()
            .all(|path| path.starts_with(home) && path != home)
    );
    Ok(written)
}

/// Refuses render files where owner-only no-follow creation does not exist. Inputs: unused.
/// Output: `launch_context_unavailable`.
#[cfg(not(unix))]
pub fn write(
    renders: &Renders,
    home: &Path,
    servers: &[LaunchServer],
    instructions: &str,
) -> Result<Vec<PathBuf>, LaunchError> {
    let _ = (renders, home, servers, instructions);
    Err(failure("launch_context_unavailable"))
}

/// Writes one file below the private home: its folders through
/// [`super::home::private_folder`], any entry at its name removed, then the file made new,
/// 0600, never through a link. The text is zeroed once written, as it may hold MCP secrets.
/// Inputs: the home, the path below it and the text. Output: the file's path, or the
/// refusal.
#[cfg(unix)]
fn put(home: &Path, path: &str, text: String) -> Result<PathBuf, LaunchError> {
    use rustix::fs::{AtFlags, Mode, OFlags};
    use std::io::Write;
    let (folders, name) = match path.rsplit_once('/') {
        Some((folders, name)) => (folders.split('/').collect::<Vec<_>>(), name),
        None => (Vec::new(), path),
    };
    let mut bytes = text.into_bytes();
    let written = super::home::private_folder(home, &folders).and_then(|folder| {
        match rustix::fs::unlinkat(&folder, name, AtFlags::empty()) {
            Ok(()) | Err(rustix::io::Errno::NOENT) => {}
            Err(_) => return Err(failure("launch_account_home_unsafe")),
        }
        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let file = rustix::fs::openat(&folder, name, flags, Mode::RUSR | Mode::WUSR)
            .map_err(|_| failure("launch_account_home_unsafe"))?;
        let mut file = std::fs::File::from(file);
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| failure("launch_context_unwritable"))
    });
    bytes.fill(0);
    written?;
    Ok(home.join(path))
}

/// Fills a template's placeholders, each value quoted for the template's format so it stays
/// one string value there: JSON string escapes for JSON, and for TOML and YAML double quotes
/// the quote, the backslash and every control character escaped. Braces that do not name a
/// placeholder stay as they are, and a value is never read again for placeholders. Inputs:
/// the template, its format and the values by placeholder name. Output: the text,
/// `launch_template_value_missing` for a placeholder with no value, or
/// `launch_template_invalid` for a template over [`TEMPLATE_BYTES_MAX`].
pub fn fill(
    template: &str,
    format: Format,
    values: &[(&str, &str)],
) -> Result<String, LaunchError> {
    if template.len() > TEMPLATE_BYTES_MAX {
        return Err(failure("launch_template_invalid"));
    }
    let mut filled = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        filled.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let named = after
            .find('}')
            .map(|close| &after[..close])
            .filter(|name| manifest::PLACEHOLDERS.contains(name));
        match named {
            Some(name) => {
                let value = values
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| *value)
                    .ok_or_else(|| failure("launch_template_value_missing"))?;
                filled.push_str(&quoted(value, format));
                rest = &after[name.len() + 1..];
            }
            None => {
                filled.push('{');
                rest = after;
            }
        }
    }
    filled.push_str(rest);
    Ok(filled)
}

/// Quotes one value for the inside of a double-quoted string of a format. Inputs: the value
/// and the format. Output: the escaped text, without the quotes around it.
fn quoted(value: &str, format: Format) -> String {
    match format {
        Format::Json => {
            let text = serde_json::Value::from(value).to_string();
            assert!(text.len() >= 2 && text.starts_with('"') && text.ends_with('"'));
            text[1..text.len() - 1].to_owned()
        }
        Format::Toml | Format::Yaml => {
            let mut text = String::with_capacity(value.len());
            for character in value.chars() {
                match character {
                    '"' => text.push_str("\\\""),
                    '\\' => text.push_str("\\\\"),
                    '\n' => text.push_str("\\n"),
                    '\t' => text.push_str("\\t"),
                    '\r' => text.push_str("\\r"),
                    other if other.is_control() => {
                        text.push_str(&format!("\\u{:04x}", u32::from(other)));
                    }
                    other => text.push(other),
                }
            }
            text
        }
    }
}

#[cfg(all(test, unix))]
#[path = "render_tests.rs"]
mod tests;
