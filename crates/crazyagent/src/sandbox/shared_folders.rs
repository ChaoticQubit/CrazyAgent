use super::ProjectSandbox;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The most top-level folders one project may share by name under Chosen folders.
pub const CHOSEN_MAX: usize = 16;
/// The most secret files or folders the owner may share back with a project's agents.
pub const SECRETS_MAX: usize = 16;
/// The most paths git leaves untracked that one launch keeps out under Chosen folders or
/// None; a project with more refuses the launch with [`TOO_MANY`] instead of sharing them.
pub const LOCAL_PATHS_MAX: usize = 256;
/// The most untracked top-level folders one launch makes writable under Read-write.
pub const SHARED_WRITES_MAX: usize = 32;
/// The most top-level folders a project's view offers for Chosen folders.
pub const LOCAL_FOLDERS_MAX: usize = 64;
const NAME_BYTES_MAX: usize = 255;
const SECRET_PATH_BYTES_MAX: usize = 512;
const ENGINE_NAMES: [&str; 3] = [".git", ".crazyagent", "graphify-out"];
/// What the engine keeps directly in a project folder made before the projects home, whose
/// state folder is the project folder itself ([`crate::projects_home::state_folder`]).
const LEGACY_STATE_NAMES: [&str; 4] = ["context", "worktrees", "research", "brief.md"];
/// The engine folder name a shared folder is never written under ([`ProjectSandbox::allow_shared_write`]).
const CONTEXT_NAME: &str = "context";

/// The refusal code for a launch whose untracked paths git could not list.
pub const UNLISTED: &str = "shared_folders_unlisted";
/// The refusal code for a launch with more untracked paths than [`LOCAL_PATHS_MAX`] or more
/// untracked top-level folders than [`SHARED_WRITES_MAX`].
pub const TOO_MANY: &str = "shared_folders_too_many";
/// The refusal code for a shared folder the sandbox would not grant.
pub const REFUSED: &str = "shared_folders_refused";
/// The refusal code for a choice other than All and Read-only off macOS, where only Seatbelt
/// can keep folders out or open them for writing.
pub const NEEDS_MACOS: &str = "shared_folders_need_macos";
/// The refusal code for a policy whose shared folders choice is out of shape.
pub const INVALID: &str = "agent_policy_invalid";

/// What the owner sees about the Linux and Windows gap, the same in the desktop app.
pub const OFF_MACOS_LINE: &str = "On Linux and Windows agents read the whole project folder, secret files included, and only All with Read-only can start them.";

/// Secret file and folder names a project's agents never read in the project folder unless
/// the owner shares them back (DEC-147.9), as Seatbelt regex fragments in lower case, matched
/// in any letter case on any path part: `.env*`, credential stores, key and certificate files,
/// key stores, cloud and cluster credentials, Terraform state and variables, age keys and
/// `secrets.*`.
pub const SECRET_NAMES: &[&str] = &[
    r"\.env.*",
    r"\.git-credentials",
    r"\.netrc",
    r"\.npmrc",
    r"\.pypirc",
    r"\.pgpass",
    r"\.htpasswd",
    r"\.dockercfg",
    r"\.s3cfg",
    r"\.yarnrc\.yml",
    r"pip\.conf",
    r"kubeconfig",
    r"\.ssh",
    r"\.aws",
    r"\.gnupg",
    r"\.kube",
    r"\.docker/config\.json",
    r"\.config/gcloud",
    r"id_(rsa|dsa|ecdsa|ed25519).*",
    r"credentials(\..*)?",
    r"secrets?(\..*)?",
    r".*service(-|_)?account.*\.json",
    r".*\.(pem|key|p12|pfx|jks|keystore|ppk|kdbx|keychain|keychain-db|tfstate|age|crt|cer)",
    r".*\.tfstate\..*",
    r".*\.tfvars(\..*)?",
];

/// Env templates the secret deny gives back for reading, in any letter case, since they name
/// variables and hold no values: `.env.example`, `.env.sample`, `.env.template`, `.env.dist`.
/// A link with such a name to a real secret stays closed: the sandbox checks its target.
pub const SECRET_TEMPLATES: &str = r"\.env\.(example|sample|template|dist)";

/// Which of the project folder's local folders a project's agents see (DEC-147.9): All, the
/// default, shares every one, gitignored ones included; Chosen folders shares only the
/// top-level folders the owner named; None shares none, so an agent works in its worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharedScope {
    #[default]
    All,
    Chosen,
    None,
}

impl SharedScope {
    /// Every choice, the default first.
    pub const ALL: [Self; 3] = [Self::All, Self::Chosen, Self::None];

    /// The choice's stable id, as stored and sent to clients.
    pub fn id(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Chosen => "chosen",
            Self::None => "none",
        }
    }

    /// The name shown to the owner, the same in the desktop app.
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All local folders",
            Self::Chosen => "Chosen folders",
            Self::None => "None",
        }
    }

    /// What the choice gives agents, the same in the desktop app.
    pub fn help(self) -> &'static str {
        match self {
            Self::All => {
                "Agents see every folder in the project folder, the ones git ignores included, such as data and programs."
            }
            Self::Chosen => "Agents see only the top-level folders you pick.",
            Self::None => "Agents see only their own worktree.",
        }
    }
}

/// Whether a project's agents may change its shared local folders (DEC-147.9): Read-only, the
/// default, or Read-write for the shared top-level folders git does not track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharedAccess {
    #[default]
    ReadOnly,
    ReadWrite,
}

impl SharedAccess {
    /// Every choice, the default first.
    pub const ALL: [Self; 2] = [Self::ReadOnly, Self::ReadWrite];

    /// The choice's stable id, as stored and sent to clients.
    pub fn id(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::ReadWrite => "read_write",
        }
    }

    /// The name shown to the owner, the same in the desktop app.
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "Read-only",
            Self::ReadWrite => "Read-write",
        }
    }

    /// What the choice lets agents do, the same in the desktop app.
    pub fn help(self) -> &'static str {
        match self {
            Self::ReadOnly => "Agents read shared folders and never change them.",
            Self::ReadWrite => {
                "Agents may also change files in shared top-level folders git does not track, but not in hidden folders such as .venv or .vscode. Folders inside stay in place: agents can't move or delete them. Tracked files change only in their worktree."
            }
        }
    }
}

/// One project's shared local folders choice (DEC-147.9): which folders, how, the top-level
/// folders picked for Chosen folders and the secret files or folders the owner shared back.
/// A policy saved before the setting existed reads All and Read-only with nothing picked.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedFolders {
    #[serde(default)]
    pub scope: SharedScope,
    #[serde(default)]
    pub access: SharedAccess,
    #[serde(default)]
    pub chosen: Vec<String>,
    #[serde(default)]
    pub secrets: Vec<String>,
}

impl SharedFolders {
    /// Checks a choice a client sent: at most [`CHOSEN_MAX`] picked folders, each one plain
    /// top-level name, and at most [`SECRETS_MAX`] shared-back secrets, each a plain path
    /// inside the project, none twice, never `.git` or `.crazyagent`. Takes the choice.
    /// Returns [`INVALID`] for any other.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.chosen.len() > CHOSEN_MAX || self.secrets.len() > SECRETS_MAX {
            return Err(INVALID);
        }
        let names_ok = self.chosen.iter().all(|name| plain_name(name));
        let paths_ok = self.secrets.iter().all(|path| plain_path(path));
        if !names_ok || !paths_ok || repeats(&self.chosen) || repeats(&self.secrets) {
            return Err(INVALID);
        }
        Ok(())
    }

    /// Whether this choice reaches no further than All with Read-only, the only one the Linux
    /// and Windows boundaries can start. Takes the choice. Returns the answer.
    pub fn plain_reach(&self) -> bool {
        self.scope == SharedScope::All && self.access == SharedAccess::ReadOnly
    }

    /// Whether the owner's top-level folder `top` is shared under this choice. Takes the
    /// folder's name. Returns the answer.
    fn shares(&self, top: &str) -> bool {
        match self.scope {
            SharedScope::All => true,
            SharedScope::Chosen => self.chosen.iter().any(|name| name == top),
            SharedScope::None => false,
        }
    }
}

/// What one launch's sandbox gets from a shared folders choice: the untracked project paths
/// it keeps out, the untracked top-level folders it may write and the shared-back secrets it
/// may read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub kept_out: Vec<PathBuf>,
    pub writes: Vec<PathBuf>,
    pub revealed: Vec<PathBuf>,
}

/// Works out what a launch's sandbox gets from `shared` in the project folder `root`: the
/// shared-back secrets always, and, unless the choice is All with Read-only, every untracked
/// path git lists (outside `.git` and `.crazyagent`) whose top-level folder is not shared to
/// keep out, and under Read-write every shared top-level folder git does not track at all to
/// write. Takes the project folder, canonical, and the choice. Returns the plan, or
/// [`UNLISTED`] when git cannot list the paths and [`TOO_MANY`] past the bounds.
pub fn plan(root: &Path, shared: &SharedFolders) -> Result<Plan, &'static str> {
    assert!(shared.validate().is_ok());
    let revealed: Vec<PathBuf> = shared
        .secrets
        .iter()
        .map(|path| {
            path.split('/')
                .fold(root.to_path_buf(), |at, part| at.join(part))
        })
        .collect();
    if shared.plain_reach() {
        return Ok(Plan {
            revealed,
            ..Plan::default()
        });
    }
    let listed = untracked(root).ok_or(UNLISTED)?;
    let engine = engine_names(root);
    let mut kept_out = Vec::new();
    let mut writes = Vec::new();
    for path in &listed {
        let top = path.split('/').next().unwrap_or_default();
        if top.is_empty() || engine.contains(&top) {
            continue;
        }
        if !shared.shares(top) {
            kept_out.push(root.join(path.trim_end_matches('/')));
            continue;
        }
        let whole_folder = path.strip_suffix('/') == Some(top);
        let folder = root.join(top);
        let plain_folder = std::fs::symlink_metadata(&folder)
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink());
        if shared.access == SharedAccess::ReadWrite
            && whole_folder
            && plain_folder
            && !never_written(root, top)
        {
            writes.push(folder);
        }
    }
    if kept_out.len() > LOCAL_PATHS_MAX || writes.len() > SHARED_WRITES_MAX {
        return Err(TOO_MANY);
    }
    assert!(writes.iter().all(|folder| folder.parent() == Some(root)));
    Ok(Plan {
        kept_out,
        writes,
        revealed,
    })
}

/// Gives one confined launch's sandbox the shared folders choice for the project folder
/// `root`, which it must already read: the secret names kept closed there outside its own
/// writable roots with the owner's shared-back secrets open again, then [`plan`]'s keep-outs
/// and writable folders. A Full access launch runs with no profile, so its caller skips this.
/// Takes the sandbox, the canonical project folder and the choice. Returns the sandbox, or a
/// plain refusal code: [`NEEDS_MACOS`] for a wider choice off macOS, [`plan`]'s codes, or
/// [`REFUSED`] for a grant the sandbox would not hold.
pub fn apply(
    sandbox: ProjectSandbox,
    root: &Path,
    shared: &SharedFolders,
) -> Result<ProjectSandbox, &'static str> {
    if cfg!(not(target_os = "macos")) && !shared.plain_reach() {
        return Err(NEEDS_MACOS);
    }
    let plan = plan(root, shared)?;
    let mut sandbox = sandbox
        .keep_project_secrets(root, &plan.revealed)
        .map_err(|_| REFUSED)?;
    for path in &plan.kept_out {
        sandbox = sandbox.keep_out(path).map_err(|_| REFUSED)?;
    }
    for folder in &plan.writes {
        sandbox = sandbox.allow_shared_write(folder).map_err(|_| REFUSED)?;
    }
    Ok(sandbox)
}

/// The top-level folders of the project folder the owner may pick for Chosen folders: real
/// folders, not links, with plain names, never the engine's own ([`engine_names`]), sorted,
/// at most [`LOCAL_FOLDERS_MAX`]. Takes the project folder. Returns the names, none when it
/// cannot be read.
pub fn local_folders(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let engine = engine_names(root);
    let mut names: Vec<String> = entries
        .flatten()
        .take(4 * LOCAL_FOLDERS_MAX)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| plain_name(name) && !engine.contains(&name.as_str()))
        .collect();
    names.sort();
    names.truncate(LOCAL_FOLDERS_MAX);
    assert!(names.len() <= LOCAL_FOLDERS_MAX);
    names
}

/// The "Shared project folders" section every harness's agents get in their project context
/// (DEC-141, DEC-147.9): where the project folder is, which of its local folders they may
/// read or change, that secret files stay closed, and that toolchains are the owner's,
/// read-only and never installed (F26). Takes the project folder, none when unknown, the
/// access level and the choice. Returns at most [`SECTION_BYTES_MAX`] bytes of markdown.
pub fn context_section(
    folder: Option<&Path>,
    level: super::policy::AgentAccess,
    shared: &SharedFolders,
) -> String {
    let mut text = String::from("\n## Shared project folders\n\n");
    let place = folder
        .map(|folder| folder.display().to_string())
        .filter(|place| place.len() <= SECRET_PATH_BYTES_MAX * 2)
        .unwrap_or_else(|| "the project folder".to_owned());
    if level == super::policy::AgentAccess::Full {
        text.push_str(&format!(
            "You run with the owner's permissions: {place} and all of its folders are open to you. Never read or copy secret files there (.env files, keys, certificates, credential files) unless the task asks for one.\n"
        ));
    } else {
        match shared.scope {
            SharedScope::All => text.push_str(&format!(
                "{place} holds folders your worktree does not have, such as data, programs and the folders git ignores. Read them there.\n"
            )),
            SharedScope::Chosen => {
                let mut names = String::new();
                for name in &shared.chosen {
                    if names.len() + name.len() + 4 > CHOSEN_LIST_BYTES_MAX {
                        names.push_str(", ...");
                        break;
                    }
                    if !names.is_empty() {
                        names.push_str(", ");
                    }
                    names.push_str(&format!("`{name}`"));
                }
                if names.is_empty() {
                    names.push_str("none yet");
                }
                text.push_str(&format!(
                    "Of the folders in {place} that your worktree does not have, only these are shared with you: {names}. Read them there.\n"
                ));
            }
            SharedScope::None => text.push_str(&format!(
                "The folders in {place} that your worktree does not have are not shared with you. Work in your worktree.\n"
            )),
        }
        if shared.scope != SharedScope::None {
            text.push_str(match shared.access {
                SharedAccess::ReadOnly => "They are read-only to you.\n",
                SharedAccess::ReadWrite => "You may change files in the shared top-level folders git does not track, never in hidden ones (.venv, .vscode) or graphify-out, and you can't move or delete folders there; tracked files change only in your worktree.\n",
            });
        }
        text.push_str("Secret files there (.env files, keys, certificates, credential files) stay closed unless the owner shared them.\n");
    }
    text.push_str("Toolchains: you use the owner's Rust, Node.js and Python read-only. Never install or download a toolchain or a language version (such as rustup toolchain install, uv python install or nvm install); when one is missing, say so in your reply.\n");
    assert!(text.len() <= SECTION_BYTES_MAX);
    text
}

/// The most bytes of [`context_section`].
pub const SECTION_BYTES_MAX: usize = 3072;
const CHOSEN_LIST_BYTES_MAX: usize = 512;

/// Whether the top-level folder `name` of the project folder `root` stays read-only under
/// Read-write (L4 r1 M1): a hidden folder, which the owner's own tools load and run unconfined
/// (`.venv`, `.vscode`, `.claude`, `.codex`), the engine's `context` and every engine name.
/// Takes the project folder and the name. Returns true when agents never write it.
pub(crate) fn never_written(root: &Path, name: &str) -> bool {
    name.starts_with('.')
        || name.eq_ignore_ascii_case(CONTEXT_NAME)
        || engine_names(root)
            .iter()
            .any(|engine| engine.eq_ignore_ascii_case(name))
}

/// The top-level names in the project folder `root` that belong to the engine: `.git`,
/// `.crazyagent` and `graphify-out`, and in a project made before the projects home, the
/// state it keeps there.
/// Takes the project folder. Returns the names.
fn engine_names(root: &Path) -> Vec<&'static str> {
    let mut names = ENGINE_NAMES.to_vec();
    if crate::projects_home::state_folder(root) == root {
        names.extend(LEGACY_STATE_NAMES);
    }
    names
}

fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_BYTES_MAX
        && name != "."
        && name != ".."
        && !ENGINE_NAMES
            .iter()
            .any(|engine| engine.eq_ignore_ascii_case(name))
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '"' | '\\' | '\''))
}

fn plain_path(path: &str) -> bool {
    path.len() <= SECRET_PATH_BYTES_MAX && path.split('/').all(plain_name)
}

fn repeats(items: &[String]) -> bool {
    items
        .iter()
        .enumerate()
        .any(|(index, item)| items[..index].contains(item))
}

#[cfg(unix)]
fn untracked(root: &Path) -> Option<Vec<String>> {
    crate::onboarding::draft::untracked(root)
}

#[cfg(not(unix))]
fn untracked(_root: &Path) -> Option<Vec<String>> {
    None
}

#[cfg(test)]
mod tests;
