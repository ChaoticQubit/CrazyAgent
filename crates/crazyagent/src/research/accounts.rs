use super::runner::{ResearchAccount, ResearchHarness};
use crate::config::{ModelAccess, ModelAuthentication, ModelConnection, ModelEndpoint};
use crate::protocol::CredentialRef;
use std::path::{Path, PathBuf};

const ACCOUNT_DEFAULT_MODEL: &str = "native-default";
const ANTHROPIC_PROTOCOL: &str = "anthropic-messages";

/// The account a research job should run on, or why it has to wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountPick {
    NeedsPool,
    NoUsableAccount,
    Account {
        connection: CredentialRef,
        account: ResearchAccount,
    },
}

impl AccountPick {
    /// The plain status the UI shows while research cannot start.
    pub fn label(&self) -> &'static str {
        match self {
            Self::NeedsPool => "Needs a pool",
            Self::NoUsableAccount => "No account in this project's pool can run this research",
            Self::Account { .. } => "Ready",
        }
    }
}

/// Picks the first account `harness` can use from the pools explicitly
/// assigned to `project`, in pool and member order. `managed_home` maps a
/// managed profile reference to its engine-owned directory only while that
/// profile is signed in.
pub fn pick_account(
    access: &ModelAccess,
    project: &str,
    harness: &str,
    managed_home: impl Fn(&CredentialRef) -> Option<PathBuf>,
) -> AccountPick {
    let assigned: Vec<_> = access
        .pools
        .iter()
        .filter(|pool| pool.projects.iter().any(|id| id.as_str() == project))
        .collect();
    if assigned.is_empty() {
        return AccountPick::NeedsPool;
    }
    for pool in assigned {
        for member in &pool.members {
            let Some(connection) = access.connections.iter().find(|c| &c.id == member) else {
                continue;
            };
            if let Some(account) = usable(connection, harness, &managed_home) {
                return AccountPick::Account {
                    connection: connection.id.clone(),
                    account,
                };
            }
        }
    }
    AccountPick::NoUsableAccount
}

fn usable(
    connection: &ModelConnection,
    harness: &str,
    managed_home: &impl Fn(&CredentialRef) -> Option<PathBuf>,
) -> Option<ResearchAccount> {
    let harness = ResearchHarness::of(harness)?;
    let anthropic = connection.protocol.as_str() == ANTHROPIC_PROTOCOL;
    let model =
        || (connection.model.id != ACCOUNT_DEFAULT_MODEL).then(|| connection.model.id.clone());
    match (&connection.endpoint, &connection.auth) {
        (ModelEndpoint::Http { url }, ModelAuthentication::None {}) => match harness {
            ResearchHarness::Codex if !anthropic => Some(ResearchAccount::Endpoint {
                base_url: url.clone(),
                model: connection.model.id.clone(),
            }),
            ResearchHarness::ClaudeCode if anthropic => Some(ResearchAccount::AnthropicEndpoint {
                base_url: url.clone(),
                model: connection.model.id.clone(),
            }),
            _ => None,
        },
        (ModelEndpoint::Native {}, ModelAuthentication::NativeManaged { profile_ref })
            if ResearchHarness::of(connection.harness.as_str()) == Some(harness) =>
        {
            let home = managed_home(profile_ref)?;
            Some(match harness {
                ResearchHarness::Codex => ResearchAccount::ManagedCodex {
                    home,
                    model: model(),
                },
                ResearchHarness::ClaudeCode => ResearchAccount::ManagedClaude {
                    home,
                    model: model(),
                },
            })
        }
        _ => None,
    }
}

/// Resolves a project to its folder under `projects_root` and the account
/// picked from its assigned pool, rereading the engine's settings files on
/// every call so pool assignment takes effect without a restart. Managed
/// accounts are skipped until the engine hands over signed-in profile homes.
#[derive(Debug, Clone)]
pub struct SettingsResolver {
    global: PathBuf,
    project: PathBuf,
    projects_root: PathBuf,
}

impl SettingsResolver {
    /// Takes the global and project settings files and the canonical folder
    /// that holds one folder per project.
    pub fn new(global: &Path, project: &Path, projects_root: &Path) -> Result<Self, String> {
        let projects_root = projects_root
            .canonicalize()
            .map_err(|error| format!("projects folder: {error}"))?;
        if !projects_root.is_dir() {
            return Err("projects folder must be a directory".into());
        }
        Ok(Self {
            global: global.to_path_buf(),
            project: project.to_path_buf(),
            projects_root,
        })
    }

    /// The project's folder and the account `harness` would run on, or
    /// `None` when the project has no plain folder yet or the settings cannot
    /// be read.
    pub fn resolve(&self, project: &str, harness: &str) -> Option<(PathBuf, AccountPick)> {
        CredentialRef::new(project).ok()?;
        let folder = self.projects_root.join(project);
        let metadata = std::fs::symlink_metadata(&folder).ok()?;
        if !metadata.file_type().is_dir() || folder.canonicalize().ok()? != folder {
            return None;
        }
        let config = crate::config::resolve_config(&self.global, &self.project).ok()?;
        let access = config.settings.model_access.unwrap_or_default();
        Some((folder, pick_account(&access, project, harness, |_| None)))
    }
}
