use super::runner::{ResearchAccount, ResearchHarness};
use crate::config::{
    AccountPool, ModelAccess, ModelAuthentication, ModelConnection, ModelEndpoint,
};
use crate::engine::owner::HostStamp;
use crate::engine::owner::native_auth::NativeAuthFact;
use crate::protocol::CredentialRef;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::path::{Path, PathBuf};
use std::time::Duration;

const ACCOUNT_DEFAULT_MODEL: &str = "native-default";
const ANTHROPIC_PROTOCOL: &str = "anthropic-messages";
const DATABASE_WAIT: Duration = Duration::from_secs(5);

/// The custody and sign-in code for a managed profile another launch holds.
pub(crate) const PROFILE_BUSY: &str = "native_profile_in_use";

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
/// assigned to `project`, in pool and member order. `signed_in` reports
/// whether a managed connection is signed in for this engine run.
pub fn pick_account(
    access: &ModelAccess,
    project: &str,
    harness: &str,
    signed_in: impl Fn(&ModelConnection) -> bool,
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
        if let Some((connection, account)) = pool_account(access, pool, harness, &signed_in) {
            return AccountPick::Account {
                connection,
                account: Box::new(account),
            };
        }
    }
    AccountPick::NoUsableAccount
}

/// Picks the first member of `pool` that `harness` can use, in member order,
/// with the same rules as [`pick_account`]. Takes the model access, the pool,
/// the harness id and the signed-in check for managed connections. Returns the
/// connection id and its account, or `None` when no member fits.
pub fn pool_account(
    access: &ModelAccess,
    pool: &AccountPool,
    harness: &str,
    signed_in: &impl Fn(&ModelConnection) -> bool,
) -> Option<(CredentialRef, ResearchAccount)> {
    pool.members.iter().find_map(|member| {
        let connection = access.connections.iter().find(|c| &c.id == member)?;
        usable(connection, harness, signed_in).map(|account| (connection.id.clone(), account))
    })
}

fn usable(
    connection: &ModelConnection,
    harness: &str,
    signed_in: &impl Fn(&ModelConnection) -> bool,
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
        (ModelEndpoint::Native {}, ModelAuthentication::NativeManaged { .. })
            if ResearchHarness::of(connection.harness.as_str()) == Some(harness)
                && signed_in(connection) =>
        {
            Some(match harness {
                ResearchHarness::Codex => ResearchAccount::ManagedCodex {
                    connection: connection.clone(),
                    model: model(),
                },
                ResearchHarness::ClaudeCode => ResearchAccount::ManagedClaude {
                    connection: connection.clone(),
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
/// without a restart. A managed account is offered only while it passes the
/// sign-in check every master and team turn uses for this engine run.
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
        let access = config.settings.model_access.unwrap_or_default();
        let database = self.engine();
        let pick = pick_account(&access, project, harness, |connection| {
            database
                .as_ref()
                .is_some_and(|database| match managed_sign_in(database, connection) {
                    Ok(_) => true,
                    Err(code) => code == PROFILE_BUSY,
                })
        });
        Some((folder, pick))
    }

    /// Opens the engine database for one resolve, or `None` when it cannot be
    /// opened, so managed accounts are then treated as signed out.
    fn engine(&self) -> Option<Connection> {
        let database = Connection::open_with_flags(
            &self.database,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .ok()?;
        database.busy_timeout(DATABASE_WAIT).ok()?;
        Some(database)
    }
}

/// Proves a managed connection is signed in for the running engine with the
/// same check every master and team turn uses. Takes the engine database and
/// the connection. Returns the active engine host and the sign-in, or a plain
/// code such as `research_engine_not_ready`, `master_authentication_stale` or
/// `native_profile_in_use`.
pub(crate) fn managed_sign_in(
    database: &Connection,
    connection: &ModelConnection,
) -> Result<(HostStamp, NativeAuthFact), String> {
    let tx = database
        .unchecked_transaction()
        .map_err(|error| error.to_string())?;
    let host = tx
        .query_row(
            "SELECT db_instance,engine_epoch FROM owner_meta WHERE singleton=1 AND schema_version=1 AND runtime_state='active'",
            [],
            |row| {
                Ok(HostStamp {
                    db_instance: row.get(0)?,
                    engine_epoch: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(|error| error.to_string())?
        .ok_or("research_engine_not_ready")?;
    let fact = crate::storage::native_sign_in(&tx, &host, connection, None)
        .map_err(|problem| problem.0)?;
    drop(tx);
    assert_eq!(fact.binding.engine_epoch, host.engine_epoch);
    assert_eq!(fact.binding.connection, *connection);
    Ok((host, fact))
}
