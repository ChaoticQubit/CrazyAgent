use super::runner::{ResearchAccount, ResearchHarness};
use crate::config::{
    EffectiveConfig, ModelAccess, ModelAuthentication, ModelConnection, ModelEndpoint,
};
use crate::protocol::CredentialRef;
use rusqlite::{Connection, OpenFlags};
use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

const ACCOUNT_DEFAULT_MODEL: &str = "native-default";
const ANTHROPIC_PROTOCOL: &str = "anthropic-messages";
const PROFILES: &str = "native-profiles";
const PROFILE_HOME: &str = "home";
const DATABASE_WAIT: Duration = Duration::from_secs(5);

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
/// picked from its assigned pool, rereading the engine's settings files and
/// sign-in records on every call so pool assignment and sign-in take effect
/// without a restart. A managed account is offered only while the engine
/// database records its profile as signed in and its engine-owned home is a
/// private canonical folder.
#[derive(Debug, Clone)]
pub struct SettingsResolver {
    global: PathBuf,
    project: PathBuf,
    projects_root: PathBuf,
    database: PathBuf,
}

impl SettingsResolver {
    /// Takes the global and project settings files, the canonical folder
    /// that holds one folder per project and the engine's `engine.db`, whose
    /// folder holds the managed profiles.
    pub fn new(
        global: &Path,
        project: &Path,
        projects_root: &Path,
        database: &Path,
    ) -> Result<Self, String> {
        let projects_root = projects_root
            .canonicalize()
            .map_err(|error| format!("projects folder: {error}"))?;
        if !projects_root.is_dir() {
            return Err("projects folder must be a directory".into());
        }
        let database = database
            .canonicalize()
            .map_err(|error| format!("engine database: {error}"))?;
        if !database.is_file() {
            return Err("engine database must be a file".into());
        }
        Ok(Self {
            global: global.to_path_buf(),
            project: project.to_path_buf(),
            projects_root,
            database,
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
        let homes = self.managed_homes(&config, project);
        let access = config.settings.model_access.unwrap_or_default();
        Some((
            folder,
            pick_account(&access, project, harness, |profile| {
                homes.get(profile).cloned()
            }),
        ))
    }

    /// Maps the profile reference of every signed-in managed connection in
    /// the pools assigned to `project` to its engine-owned home. Takes the
    /// effective settings and the project. Returns an empty map when the
    /// engine database or its sign-in records cannot be read.
    fn managed_homes(
        &self,
        config: &EffectiveConfig,
        project: &str,
    ) -> BTreeMap<CredentialRef, PathBuf> {
        let mut homes = BTreeMap::new();
        let (Some(access), Some(runtime)) = (
            config.settings.model_access.as_ref(),
            self.database.parent(),
        ) else {
            return homes;
        };
        let Ok(database) = Connection::open_with_flags(
            &self.database,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        ) else {
            return homes;
        };
        if database.busy_timeout(DATABASE_WAIT).is_err() {
            return homes;
        }
        let Ok(tx) = database.unchecked_transaction() else {
            return homes;
        };
        for pool in access
            .pools
            .iter()
            .filter(|pool| pool.projects.iter().any(|id| id.as_str() == project))
        {
            let Ok(signed) =
                crate::project_setup::signed_in_connections(&tx, config, Some(&pool.id))
            else {
                continue;
            };
            for connection in access
                .connections
                .iter()
                .filter(|connection| signed.contains(&connection.id))
            {
                if let ModelAuthentication::NativeManaged { profile_ref } = &connection.auth
                    && let Some(home) = profile_home(runtime, profile_ref)
                {
                    homes.insert(profile_ref.clone(), home);
                }
            }
        }
        drop(tx);
        assert!(homes.len() <= access.connections.len());
        homes
    }
}

/// The engine-owned home of a managed profile,
/// `<runtime>/native-profiles/<sha256 of the reference>/home`, when it is a
/// private canonical directory of this user. Takes the canonical engine
/// runtime folder and the profile reference. Returns the home, or `None`
/// for a missing, linked or shared one.
fn profile_home(runtime: &Path, profile: &CredentialRef) -> Option<PathBuf> {
    let home = runtime
        .join(PROFILES)
        .join(crate::memory::hash(profile.as_str().as_bytes()))
        .join(PROFILE_HOME);
    let metadata = std::fs::symlink_metadata(&home).ok()?;
    let private = metadata.is_dir()
        && metadata.uid() == rustix::process::geteuid().as_raw()
        && metadata.mode() & 0o7777 == 0o700;
    (private && home.canonicalize().ok()? == home).then_some(home)
}
