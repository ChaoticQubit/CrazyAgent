use super::InstallError;
use super::pack::{PackManifestEntry, PackSource};
use super::run::{private_folder, run, staging_folder};
use crate::plugins::ServerSpec;
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

const GIT: &str = "/usr/bin/git";
const FETCH_TIME: Duration = Duration::from_secs(300);
const GIT_TIME: Duration = Duration::from_secs(60);
const REPO_BYTES_MAX: usize = 512;
const PATH_BYTES_MAX: usize = 256;
const WORDS_MAX: usize = 33;
const SKILLS_MAX: usize = crate::plugins::SKILLS_PER_SCOPE_MAX;
const SKILL_DEPTH: usize = 3;
const PROGRAM_FOLDERS: [&str; 4] = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];
const GIT_CONFIG: [&str; 14] = [
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "protocol.allow=never",
    "-c",
    "protocol.https.allow=always",
    "-c",
    "protocol.file.allow=always",
    "-c",
    "credential.helper=",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "advice.detachedHead=false",
];

fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= PATH_BYTES_MAX
        && !path.chars().any(char::is_control)
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn is_commit(text: &str) -> bool {
    text.len() == 40
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Checks a pinned git source before anything runs: an `https://` repository URL with a
/// host and no user, query or fragment (or a `file:///` URL for a local repository), a
/// 40-hex commit, and an optional relative folder that never leaves the repository.
/// Output: `setup_pin_invalid` for anything else.
pub fn validate_git(repo: &str, commit: &str, path: Option<&str>) -> Result<(), InstallError> {
    let invalid = InstallError("setup_pin_invalid");
    if repo.len() > REPO_BYTES_MAX || !is_commit(commit) || path.is_some_and(|p| !safe_relative(p))
    {
        return Err(invalid);
    }
    let url = url::Url::parse(repo).map_err(|_| invalid)?;
    let plain = url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none();
    let place = match url.scheme() {
        "https" => url.host_str().is_some_and(|host| !host.is_empty()),
        "file" => url.host_str().is_none_or(str::is_empty) && url.path().starts_with('/'),
        _ => false,
    };
    if !plain || !place {
        return Err(invalid);
    }
    Ok(())
}

fn git_env(setup: &Path) -> Vec<(&'static str, OsString)> {
    vec![
        ("PATH", OsString::from("/usr/bin:/bin")),
        ("HOME", setup.as_os_str().to_owned()),
        ("GIT_CONFIG_NOSYSTEM", "1".into()),
        ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
        ("GIT_TERMINAL_PROMPT", "0".into()),
        ("GIT_ASKPASS", "/usr/bin/false".into()),
        ("LANG", "C".into()),
    ]
}

fn git(
    setup: &Path,
    cwd: &Path,
    args: &[&str],
    limit: Duration,
) -> Result<(bool, Vec<u8>), InstallError> {
    let mut all: Vec<OsString> = GIT_CONFIG.iter().map(OsString::from).collect();
    all.extend(args.iter().map(OsString::from));
    run(Path::new(GIT), &all, &git_env(setup), cwd, limit)
}

fn head(setup: &Path, checkout: &Path) -> Option<String> {
    let checkout = checkout.to_str()?;
    let (ok, output) = git(
        setup,
        setup,
        &["-C", checkout, "rev-parse", "--verify", "HEAD^{commit}"],
        GIT_TIME,
    )
    .ok()?;
    let text = String::from_utf8(output).ok()?;
    ok.then(|| text.trim().to_owned())
}

/// Fetches one pinned git source into `<setup>/pack/<repo key>-<commit>` and checks out
/// exactly that commit, refusing the source when the commit is missing or the checkout
/// ends up anywhere else. An existing checkout is reused only when it is still at the
/// pin. Inputs: the setup folder, repository URL and commit. Output: the checkout folder.
pub fn fetch_git(setup: &Path, repo: &str, commit: &str) -> Result<PathBuf, InstallError> {
    validate_git(repo, commit, None)?;
    let packs = setup.join("pack");
    private_folder(&packs)?;
    let key: String = Sha256::digest(repo.as_bytes())
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let target = packs.join(format!("{key}-{commit}"));
    if std::fs::symlink_metadata(&target).is_ok() {
        if head(setup, &target).as_deref() == Some(commit) {
            return Ok(target);
        }
        std::fs::remove_dir_all(&target).map_err(|_| InstallError("setup_store_unavailable"))?;
    }
    let staging = staging_folder(&packs, "fetch")?;
    let result = (|| {
        let place = staging
            .to_str()
            .ok_or(InstallError("setup_store_unavailable"))?;
        let (cloned, _) = git(
            setup,
            &packs,
            &["clone", "--quiet", "--no-checkout", "--", repo, place],
            FETCH_TIME,
        )?;
        if !cloned {
            return Err(InstallError("pack_fetch_failed"));
        }
        let (checked, _) = git(
            setup,
            &packs,
            &["-C", place, "checkout", "--quiet", "--detach", commit],
            GIT_TIME,
        )?;
        if !checked || head(setup, &staging).as_deref() != Some(commit) {
            return Err(InstallError("pack_commit_mismatch"));
        }
        std::fs::rename(&staging, &target).map_err(|_| InstallError("setup_store_unavailable"))
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result.map(|()| target)
}

/// Finds the folder holding one entry's files: the pinned git checkout, or its `path`
/// inside it, or a folder the app bundles. Inputs: the setup folder, the entry and the
/// bundled-skills folder when this build has one. Output: the folder, which is never a link.
pub fn entry_folder(
    setup: &Path,
    entry: &PackManifestEntry,
    bundled: Option<&Path>,
) -> Result<PathBuf, InstallError> {
    let folder = match &entry.source {
        PackSource::Git { repo, commit, path } => {
            validate_git(repo, commit, path.as_deref())?;
            let checkout = fetch_git(setup, repo, commit)?;
            match path {
                Some(path) => checkout.join(path),
                None => checkout,
            }
        }
        PackSource::Bundled { path } => {
            if !safe_relative(path) {
                return Err(InstallError("setup_pin_invalid"));
            }
            bundled
                .ok_or(InstallError("pack_bundled_unavailable"))?
                .join(path)
        }
    };
    if !std::fs::symlink_metadata(&folder).is_ok_and(|m| m.is_dir()) {
        return Err(InstallError("pack_no_skills"));
    }
    Ok(folder)
}

fn has_skill(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join("SKILL.md")).is_ok_and(|m| m.is_file())
}

fn collect_skills(
    folder: &Path,
    depth: usize,
    found: &mut Vec<PathBuf>,
) -> Result<(), InstallError> {
    if found.len() > SKILLS_MAX {
        return Err(InstallError("pack_no_skills"));
    }
    if has_skill(folder) {
        found.push(folder.to_path_buf());
        return Ok(());
    }
    if depth == 0 {
        return Ok(());
    }
    let entries = std::fs::read_dir(folder).map_err(|_| InstallError("pack_no_skills"))?;
    let mut children = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| InstallError("pack_no_skills"))?;
        let path = entry.path();
        let hidden = entry.file_name().to_string_lossy().starts_with('.');
        if !hidden && std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
            children.push(path);
        }
    }
    children.sort();
    for child in children {
        collect_skills(&child, depth - 1, found)?;
    }
    Ok(())
}

/// Lists the skill folders inside one entry folder: the folder itself when it holds a
/// SKILL.md, otherwise every folder below it that does, up to three levels down, never
/// through a link or a hidden folder and never inside a skill folder already found.
/// Input: the entry folder. Output: 1 to 100 folders in name order, or `pack_no_skills`.
pub fn skill_folders(folder: &Path) -> Result<Vec<PathBuf>, InstallError> {
    let mut found = Vec::new();
    collect_skills(folder, SKILL_DEPTH, &mut found)?;
    found.sort();
    if found.is_empty() || found.len() > SKILLS_MAX {
        return Err(InstallError("pack_no_skills"));
    }
    Ok(found)
}

fn runnable(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
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

/// How one MCP server entry is set up: an optional Python tool that `uv tool install`
/// must install first, then how the server starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerPlan {
    pub tool: Option<(String, String)>,
    pub spec: ServerSpec,
}

fn default_program(setup: &Path, name: &str) -> Option<PathBuf> {
    let mut folders = vec![setup.join("bin")];
    folders.extend(PROGRAM_FOLDERS.iter().map(PathBuf::from));
    folders
        .into_iter()
        .map(|dir| dir.join(name))
        .find(|candidate| runnable(candidate))
}

/// Builds how an MCP server entry starts, looking programs up in `<setup>/bin` and the
/// usual program folders. Same rules as `server_plan`, with no tool step allowed.
/// Inputs: the setup folder, the entry and its folder. Output: the server spec.
pub fn server_spec(
    setup: &Path,
    entry: &PackManifestEntry,
    folder: &Path,
) -> Result<ServerSpec, InstallError> {
    let plan = server_plan(entry, folder, &|name| default_program(setup, name))?;
    if plan.tool.is_some() {
        return Err(InstallError("pack_server_invalid"));
    }
    Ok(plan.spec)
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
/// is absolute, relative to the entry folder, or found by `resolve`; `uvx` runs as
/// `uv tool run`; `{dir}` in an argument stands for the entry folder. Text after the
/// server phrase is a note. Inputs: the entry, its folder and the program lookup.
/// Output: the plan, `pack_server_invalid` or `pack_server_command_missing`.
pub fn server_plan(
    entry: &PackManifestEntry,
    folder: &Path,
    resolve: &dyn Fn(&str) -> Option<PathBuf>,
) -> Result<ServerPlan, InstallError> {
    let invalid = InstallError("pack_server_invalid");
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
    } else if first.starts_with('/') {
        PathBuf::from(first)
    } else if first.contains('/') {
        if !safe_relative(first) {
            return Err(invalid);
        }
        folder.join(first)
    } else if tool.is_some() {
        PathBuf::new()
    } else {
        resolve(first).ok_or(InstallError("pack_server_command_missing"))?
    };
    let dir = folder.to_str().ok_or(invalid)?;
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
/// program lookup. Output: a spec with an absolute command.
pub fn finish_plan(
    plan: ServerPlan,
    resolve: &dyn Fn(&str) -> Option<PathBuf>,
) -> Result<ServerSpec, InstallError> {
    let invalid = InstallError("pack_server_invalid");
    let ServerSpec::Stdio { command, args } = plan.spec else {
        return Ok(plan.spec);
    };
    let command = if command.starts_with('/') {
        command
    } else {
        let program = resolve(&command).ok_or(InstallError("pack_server_command_missing"))?;
        program.to_str().ok_or(invalid)?.to_owned()
    };
    let spec = ServerSpec::Stdio { command, args };
    spec.validate().map_err(|_| invalid)?;
    Ok(spec)
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
