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

/// Lists the skill folders inside one entry folder: the folder itself when it holds a
/// SKILL.md, otherwise each child folder that does, sorted by name and never through a
/// link. Input: the entry folder. Output: 1 to 100 folders, or `pack_no_skills`.
pub fn skill_folders(folder: &Path) -> Result<Vec<PathBuf>, InstallError> {
    let has_skill =
        |dir: &Path| std::fs::symlink_metadata(dir.join("SKILL.md")).is_ok_and(|m| m.is_file());
    if has_skill(folder) {
        return Ok(vec![folder.to_path_buf()]);
    }
    let entries = std::fs::read_dir(folder).map_err(|_| InstallError("pack_no_skills"))?;
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| InstallError("pack_no_skills"))?;
        let path = entry.path();
        let hidden = entry.file_name().to_string_lossy().starts_with('.');
        let directory = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
        if !hidden && directory && has_skill(&path) {
            found.push(path);
        }
    }
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

/// Builds how an MCP server entry starts from its `install` line: words split on spaces
/// with no shell, the first naming the program (absolute, relative to the entry folder,
/// or found in `<setup>/bin` and the usual program folders) and `{dir}` in any argument
/// standing for the entry folder. Inputs: the setup folder, the entry and its folder.
/// Output: a command spec, `pack_server_invalid` or `pack_server_command_missing`.
pub fn server_spec(
    setup: &Path,
    entry: &PackManifestEntry,
    folder: &Path,
) -> Result<ServerSpec, InstallError> {
    let invalid = InstallError("pack_server_invalid");
    let words: Vec<&str> = entry.install.split_whitespace().collect();
    let (first, rest) = words.split_first().ok_or(invalid)?;
    if words.len() > WORDS_MAX {
        return Err(invalid);
    }
    let program = if first.starts_with('/') {
        PathBuf::from(first)
    } else if first.contains('/') {
        if !safe_relative(first) {
            return Err(invalid);
        }
        folder.join(first)
    } else {
        let mut folders = vec![setup.join("bin")];
        folders.extend(PROGRAM_FOLDERS.iter().map(PathBuf::from));
        folders
            .into_iter()
            .map(|dir| dir.join(first))
            .find(|candidate| runnable(candidate))
            .ok_or(InstallError("pack_server_command_missing"))?
    };
    if !runnable(&program) {
        return Err(InstallError("pack_server_command_missing"));
    }
    let dir = folder.to_str().ok_or(invalid)?;
    let spec = ServerSpec::Stdio {
        command: program.to_str().ok_or(invalid)?.to_owned(),
        args: rest.iter().map(|word| word.replace("{dir}", dir)).collect(),
    };
    spec.validate().map_err(|_| invalid)?;
    Ok(spec)
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
