use super::pack::PackManifestEntry;
use super::{InstallError, Installer, PLATFORM_UNSUPPORTED};
use crate::plugins::ServerSpec;
use std::path::{Component, Path, PathBuf};

const PATH_BYTES_MAX: usize = 256;
const WORDS_MAX: usize = 33;

/// Whether a relative path stays inside the folder it is joined to: nonempty, bounded,
/// without control characters and made only of plain names. Input: the path text. Output:
/// the answer.
pub(super) fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= PATH_BYTES_MAX
        && !path.chars().any(char::is_control)
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

/// Whether a program file can start: a file with an execute bit. Input: the file. Output:
/// the answer.
#[cfg(unix)]
pub(super) fn runnable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// Whether a program file can start where no execute bit exists and Windows starts a file
/// by its extension: a file. Input: the file. Output: the answer.
#[cfg(not(unix))]
pub(super) fn runnable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// The MCP server name a pack entry registers under: its id in lowercase with every
/// other character than letters, digits, `-` and `_` turned into `-`.
pub fn server_name(entry: &PackManifestEntry) -> Result<String, InstallError> {
    let name: String = entry
        .id
        .chars()
        .map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9' | '-' | '_') => c,
            _ => '-',
        })
        .collect();
    crate::plugins::validate_server_name(&name).map_err(|_| InstallError("pack_server_invalid"))?;
    Ok(name)
}

/// How one MCP server entry is set up: an optional Python tool setup must install first
/// from its bundled hash lock, then how the server starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerPlan {
    pub tool: Option<(String, String)>,
    pub spec: ServerSpec,
}

fn tool_step(text: &str) -> Result<(String, String), InstallError> {
    let invalid = InstallError("pack_server_invalid");
    let (package, version) = text.trim().split_once("==").ok_or(invalid)?;
    let plain = |part: &str, extra: &[u8]| {
        !part.is_empty()
            && part.len() <= 64
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || extra.contains(&b))
    };
    if !plain(package, b"-_.") || !plain(version, b".-+") {
        return Err(invalid);
    }
    Ok((package.to_owned(), version.to_owned()))
}

/// Reads how an MCP server entry starts from its `install` line, with no shell. The line
/// is `add <https URL> as an HTTP MCP server`, or `[uv tool install <package>==<version>,
/// then ]run <program> <args> as a stdio MCP server`, or plain program words. The program
/// is a full path on this system ([`crate::plugins::absolute_command`]), relative to the
/// entry folder, or found by `resolve`; `uvx` runs as `uv tool run`; `{dir}` in an argument
/// stands for the entry folder. Text after the server phrase is a note. Inputs: the entry,
/// its folder, none where setup fetches no checkout, and the program lookup. Output: the
/// plan, `pack_server_invalid`, `pack_server_command_missing`, or
/// `setup_platform_unsupported` for a program or argument inside a folder that is not there.
pub fn server_plan(
    entry: &PackManifestEntry,
    folder: Option<&Path>,
    resolve: &dyn Fn(&str) -> Option<PathBuf>,
) -> Result<ServerPlan, InstallError> {
    let invalid = InstallError("pack_server_invalid");
    let unsupported = InstallError(PLATFORM_UNSUPPORTED);
    let line = entry.install.trim();
    if let Some(rest) = line.strip_prefix("add ")
        && let Some((url, _)) = rest.split_once(" as an HTTP MCP server")
    {
        let spec = ServerSpec::Http {
            url: url.trim().to_owned(),
        };
        spec.validate().map_err(|_| invalid)?;
        return Ok(ServerPlan { tool: None, spec });
    }
    let (tool, command) = match line.strip_prefix("uv tool install ") {
        Some(rest) => {
            let (step, command) = rest.split_once(", then run ").ok_or(invalid)?;
            (Some(tool_step(step)?), command)
        }
        None => (None, line.strip_prefix("run ").unwrap_or(line)),
    };
    let command = command
        .split_once(" as a stdio MCP server")
        .map_or(command, |(words, _)| words);
    let words: Vec<&str> = command.split_whitespace().collect();
    let (first, rest) = words.split_first().ok_or(invalid)?;
    if words.len() > WORDS_MAX {
        return Err(invalid);
    }
    let mut args: Vec<String> = Vec::new();
    let program = if *first == "uvx" {
        args.extend(["tool".to_owned(), "run".to_owned()]);
        resolve("uv").ok_or(InstallError("pack_server_command_missing"))?
    } else if crate::plugins::absolute_command(first) {
        PathBuf::from(first)
    } else if first.contains('/') {
        if !safe_relative(first) {
            return Err(invalid);
        }
        folder.ok_or(unsupported)?.join(first)
    } else if tool.is_some() {
        PathBuf::new()
    } else {
        resolve(first).ok_or(InstallError("pack_server_command_missing"))?
    };
    let dir = folder.map(|folder| folder.to_str().ok_or(invalid)).transpose()?;
    if dir.is_none() && rest.iter().any(|word| word.contains("{dir}")) {
        return Err(unsupported);
    }
    let dir = dir.unwrap_or_default();
    args.extend(rest.iter().map(|word| word.replace("{dir}", dir)));
    let command = if program.as_os_str().is_empty() {
        (*first).to_owned()
    } else {
        if !runnable(&program) {
            return Err(InstallError("pack_server_command_missing"));
        }
        program.to_str().ok_or(invalid)?.to_owned()
    };
    let spec = ServerSpec::Stdio { command, args };
    if !program.as_os_str().is_empty() {
        spec.validate().map_err(|_| invalid)?;
    }
    Ok(ServerPlan { tool, spec })
}

/// Finishes a plan whose program comes from its tool step: looks the program up again
/// after the tool is installed and checks the final spec. Inputs: the plan and the
/// program lookup. Output: a spec with a full command path on this system.
pub fn finish_plan(
    plan: ServerPlan,
    resolve: &dyn Fn(&str) -> Option<PathBuf>,
) -> Result<ServerSpec, InstallError> {
    let invalid = InstallError("pack_server_invalid");
    let ServerSpec::Stdio { command, args } = plan.spec else {
        return Ok(plan.spec);
    };
    let command = if crate::plugins::absolute_command(&command) {
        command
    } else {
        let program = resolve(&command).ok_or(InstallError("pack_server_command_missing"))?;
        program.to_str().ok_or(invalid)?.to_owned()
    };
    let spec = ServerSpec::Stdio { command, args };
    spec.validate().map_err(|_| invalid)?;
    Ok(spec)
}

/// Sets up how one MCP server entry starts and the name it registers under: its tool
/// installs first when it has one, then its program is looked up through the installer.
/// Inputs: the installer, the entry and its checkout folder, none where setup fetches no
/// checkout. Output: the name and spec, or why not ([`server_plan`]).
pub fn pack_server(
    installer: &dyn Installer,
    entry: &PackManifestEntry,
    folder: Option<&Path>,
) -> Result<(String, ServerSpec), InstallError> {
    let resolve = |name: &str| installer.program(name);
    let plan = server_plan(entry, folder, &resolve)?;
    if let Some((package, version)) = &plan.tool {
        installer.install_tool(package, version)?;
    }
    let spec = finish_plan(plan, &resolve)?;
    Ok((server_name(entry)?, spec))
}

/// The key a pack row records for a pinned git source, so a new pin replaces the old one.
/// Inputs: the repository, the commit and the folder inside it. Output: the key.
pub fn git_key(repo: &str, commit: &str, path: Option<&str>) -> String {
    format!("{repo}@{commit}/{}", path.unwrap_or_default())
}

/// Sets up one pack entry where setup fetches no checkout, as on Windows: an MCP server
/// from a git source whose program and arguments need no entry folder. Inputs: the
/// installer and the entry. Output: the pack row's source key with the server name and
/// spec, `setup_platform_unsupported` for a skill, a bundled source or anything inside the
/// entry folder, or why the server could not be set up ([`pack_server`]).
#[cfg(any(not(unix), test))]
pub fn server_only(
    installer: &dyn Installer,
    entry: &PackManifestEntry,
) -> Result<(String, (String, ServerSpec)), InstallError> {
    let super::pack::PackSource::Git { repo, commit, path } = &entry.source else {
        return Err(InstallError(PLATFORM_UNSUPPORTED));
    };
    if entry.kind != super::pack::PackKind::McpServer {
        return Err(InstallError(PLATFORM_UNSUPPORTED));
    }
    let server = pack_server(installer, entry, None)?;
    Ok((git_key(repo, commit, path.as_deref()), server))
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
