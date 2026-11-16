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
/// Names CrazyAgent keeps at the top of a private home: the launch state, the home lock and
/// the key file. A render path may not start with one, in any letter case.
const ENGINE_NAMES: [&str; 3] = [
    crate::launch_context::STATE,
    crate::launch_context::LOCK,
    super::interactive::KEY_FILE,
];

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
        && mcp.path.eq_ignore_ascii_case(&instructions.path)
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
            format!(
                "{key}.path: this must be {{home}} followed by a file inside it, each name only ASCII letters, digits, ., _ and -."
            ),
        );
    };
    let first = path.split('/').next().unwrap_or_default();
    if ENGINE_NAMES
        .iter()
        .any(|name| first.eq_ignore_ascii_case(name))
    {
        return refuse(
            manifest::VALUE,
            format!("{key}.path: CrazyAgent keeps its own files there, so a manifest cannot."),
        );
    }
    Ok(Render { kind, path })
}

/// The part of a render path after `{home}/`, when that part is [`plain`]. Input: the path
/// as written. Output: that part, or none.
fn inside_home(path: &str) -> Option<String> {
    let rest = path.strip_prefix(HOME_PREFIX)?;
    plain(rest).then(|| rest.to_owned())
}

/// Whether a path inside the private home is one to [`PATH_PARTS_MAX`] names joined by `/`,
/// each only ASCII letters, digits, `.`, `_` and `-`, none empty, `.` or `..`. A macOS folder
/// folds Unicode letter case and spelling, so with ASCII names only, two names open one file
/// exactly when they match ignoring ASCII case, which is how the engine-name and duplicate
/// checks compare them. Input: the path. Output: the answer.
pub(crate) fn plain(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    parts.len() <= PATH_PARTS_MAX
        && parts.iter().all(|part| {
            !part.is_empty()
                && *part != "."
                && *part != ".."
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
}

/// Writes the files a manifest asks for into one launch's private home, each 0600, made new
/// through no-follow steps from the agent home through 0700 folders, so a link planted in
/// the home or at its harness or account folder is replaced or refused, never written
/// through. Inputs: the files, the private home, the MCP servers the launch gives and its
/// instruction text. Output: the paths written, or the refusal.
#[cfg(unix)]
pub fn write<'a>(
    renders: &Renders,
    home: &Path,
    servers: impl IntoIterator<Item = &'a LaunchServer>,
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
pub fn write<'a>(
    renders: &Renders,
    home: &Path,
    servers: impl IntoIterator<Item = &'a LaunchServer>,
    instructions: &str,
) -> Result<Vec<PathBuf>, LaunchError> {
    let _ = (renders, home, servers.into_iter(), instructions);
    Err(failure("launch_context_unavailable"))
}

/// Writes one file below the private home: its folders through
/// [`super::home::listed_folder`] from the agent home, any entry at its name removed, then
/// the file made new, 0600, never through a link. The text is zeroed once written, as it
/// may hold MCP secrets. Inputs: the home, the path below it and the text. Output: the
/// file's path, or the refusal.
#[cfg(unix)]
fn put(home: &Path, path: &str, text: String) -> Result<PathBuf, LaunchError> {
    assert!(plain(path));
    let (folders, name) = match path.rsplit_once('/') {
        Some((folders, name)) => (folders.split('/').collect::<Vec<_>>(), name),
        None => (Vec::new(), path),
    };
    let mut bytes = text.into_bytes();
    let written = put_in(home, &folders, name, &bytes);
    bytes.fill(0);
    written?;
    Ok(home.join(path))
}

/// Writes one file in a folder of the private home reached through
/// [`super::home::listed_folder`] from the agent home: any entry at its name removed, then
/// the file made new, 0600, never through a link. Inputs: the home, the folders below it,
/// the file's name and its bytes. Output: done, or the refusal.
#[cfg(unix)]
fn put_in(home: &Path, folders: &[&str], name: &str, bytes: &[u8]) -> Result<(), LaunchError> {
    use rustix::fs::{AtFlags, Mode, OFlags};
    use std::io::Write;
    let folder = super::home::listed_folder(home, None, folders, super::home::Walk::Make)?;
    match rustix::fs::unlinkat(&folder, name, AtFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => {}
        Err(_) => return Err(failure("launch_account_home_unsafe")),
    }
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let file = rustix::fs::openat(&folder, name, flags, Mode::RUSR | Mode::WUSR)
        .map_err(|_| failure("launch_account_home_unsafe"))?;
    let mut file = std::fs::File::from(file);
    let unwritable = |_| failure("launch_context_unwritable");
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(unwritable)?;
    whole(&file, bytes.len())
}

/// Checks a file a launch just wrote and synced: owner-only, holding exactly the bytes
/// written. Inputs: the open file and the count of bytes written. Output: done,
/// `launch_context_unwritable` when it cannot be read, or `launch_account_home_unsafe`.
#[cfg(unix)]
fn whole(file: &std::fs::File, len: usize) -> Result<(), LaunchError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file
        .metadata()
        .map_err(|_| failure("launch_context_unwritable"))?;
    if metadata.mode() & 0o077 != 0 {
        return Err(failure("launch_account_home_unsafe"));
    }
    assert_eq!(metadata.len(), len as u64);
    Ok(())
}

/// Writes a keyed listed launch's key file, `.crazyagent-key`, into its private home as a
/// rendered file is written: through no-follow steps from the agent home, any entry at the
/// name removed, the file made new, 0600, so a link planted at the harness or account folder
/// after the install's walk is refused, never written through (T18 LEAD-1). Inputs: the
/// private home and the key. Output: the file's path, or `launch_account_home_unsafe`.
#[cfg(unix)]
pub(crate) fn put_key(home: &Path, key: &[u8]) -> Result<PathBuf, LaunchError> {
    let name = super::interactive::KEY_FILE;
    put_in(home, &[], name, key)?;
    let path = home.join(name);
    assert_eq!(path.parent(), Some(home));
    Ok(path)
}

/// Refuses a key file where owner-only no-follow creation does not exist. Inputs: the
/// private home and the key, both unused. Output: `launch_key_file_unavailable`.
#[cfg(not(unix))]
pub(crate) fn put_key(home: &Path, key: &[u8]) -> Result<PathBuf, LaunchError> {
    let _ = (home, key);
    Err(failure("launch_key_file_unavailable"))
}

/// Deletes one file a render wrote, by its path inside the private home, through no-follow
/// steps from the agent home ([`super::home::listed_folder`]): a path that is not [`plain`],
/// a home of another shape, a folder on the way that is a link or is missing, and a name
/// that is a folder are each left alone; a link at the name itself is removed, never what
/// it names. Inputs: the home and the path inside it.
#[cfg(unix)]
pub(crate) fn remove(home: &Path, path: &str) {
    use rustix::fs::AtFlags;
    if !plain(path) {
        return;
    }
    let (folders, name) = match path.rsplit_once('/') {
        Some((folders, name)) => (folders.split('/').collect::<Vec<_>>(), name),
        None => (Vec::new(), path),
    };
    let Ok(folder) = super::home::listed_folder(home, None, &folders, super::home::Walk::Open)
    else {
        return;
    };
    let _ = rustix::fs::unlinkat(&folder, name, AtFlags::empty());
}

/// Leaves render files alone where [`write`] never makes any. Inputs: unused.
#[cfg(not(unix))]
pub(crate) fn remove(home: &Path, path: &str) {
    let _ = (home, path);
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
