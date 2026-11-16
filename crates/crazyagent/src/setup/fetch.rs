use super::InstallError;
use super::pack::{PackManifestEntry, PackSource, repo_key};
use super::process::{private_folder, run, staging_folder};
use super::launch::{runnable, safe_relative, server_plan};
use crate::plugins::ServerSpec;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

const GIT: &str = "/usr/bin/git";
const FETCH_TIME: Duration = Duration::from_secs(300);
const GIT_TIME: Duration = Duration::from_secs(60);
const REPO_BYTES_MAX: usize = 512;
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
    let mut env = vec![
        ("PATH", OsString::from("/usr/bin:/bin")),
        ("HOME", setup.as_os_str().to_owned()),
        ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
        ("GIT_ASKPASS", "/usr/bin/false".into()),
        ("LANG", "C".into()),
    ];
    env.extend(
        crate::research::runner::GIT_SAFE_ENV.map(|(name, value)| (name, OsString::from(value))),
    );
    env
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

/// Tells whether a checkout's files are exactly its commit: no changed, untracked or
/// ignored file, judged with its own `.git` and folder whatever its config says.
/// Inputs: the setup folder and the checkout. Output: true only for a clean tree.
fn pristine(setup: &Path, checkout: &Path) -> bool {
    let store = checkout.join(".git");
    let (Some(tree), Some(store)) = (checkout.to_str(), store.to_str()) else {
        return false;
    };
    let git_dir = format!("--git-dir={store}");
    let work_tree = format!("--work-tree={tree}");
    matches!(
        git(
            setup,
            setup,
            &[
                "--no-optional-locks",
                &git_dir,
                &work_tree,
                "status",
                "--porcelain",
                "--ignored",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
            GIT_TIME,
        ),
        Ok((true, output)) if output.is_empty()
    )
}

/// Fetches one pinned git source into `<setup>/pack/<repo key>-<commit>` and checks out
/// exactly that commit, refusing the source when the commit is missing or the checkout
/// ends up anywhere else. An existing checkout is reused only when it is still at the
/// pin with no changed, untracked or ignored file; otherwise it is fetched again.
/// Inputs: the setup folder, repository URL and commit. Output: the checkout folder.
pub fn fetch_git(setup: &Path, repo: &str, commit: &str) -> Result<PathBuf, InstallError> {
    validate_git(repo, commit, None)?;
    let packs = setup.join("pack");
    private_folder(&packs)?;
    let target = checkout_folder(setup, repo, commit);
    if std::fs::symlink_metadata(&target).is_ok() {
        if head(setup, &target).as_deref() == Some(commit) && pristine(setup, &target) {
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

/// Names the folder a pinned git source is checked out in, `<setup>/pack/<repo key>-<commit>`,
/// without fetching it. Inputs: the setup folder, repository URL and commit. Output: the path.
pub fn checkout_folder(setup: &Path, repo: &str, commit: &str) -> PathBuf {
    setup
        .join("pack")
        .join(format!("{}-{commit}", repo_key(repo)))
}

/// Names the plugin folder an entry's hooks come from: the hook folder this build ships for
/// it once unpacked, else for a pinned git checkout the checkout itself, where a Claude Code
/// plugin keeps its hooks. Inputs: the setup folder and the entry. Output: the folder, or
/// none when neither is present.
pub fn plugin_folder(setup: &Path, entry: &PackManifestEntry) -> Option<PathBuf> {
    if let Some(folder) = crate::setup::bundled::hook_folder(setup, &entry.id) {
        return Some(folder);
    }
    let PackSource::Git { repo, commit, .. } = &entry.source else {
        return None;
    };
    let folder = checkout_folder(setup, repo, commit);
    std::fs::symlink_metadata(&folder)
        .is_ok_and(|metadata| metadata.is_dir())
        .then_some(folder)
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
    let plan = server_plan(entry, Some(folder), &|name| default_program(setup, name))?;
    if plan.tool.is_some() {
        return Err(InstallError("pack_server_invalid"));
    }
    Ok(plan.spec)
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
