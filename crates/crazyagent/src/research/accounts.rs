use super::runner::{ResearchAccount, ResearchHarness};
use crate::config::{
    AccountPool, ModelAccess, ModelAuthentication, ModelConnection, ModelEndpoint,
};
use crate::engine::owner::HostStamp;
use crate::engine::owner::native_auth::NativeAuthFact;
use crate::protocol::CredentialRef;
use crate::scheduling::{Candidate, Choice};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::path::{Path, PathBuf};
use std::time::Duration;

const ACCOUNT_DEFAULT_MODEL: &str = "native-default";
const DATABASE_WAIT: Duration = Duration::from_secs(5);

/// The custody and sign-in code for a managed profile another launch holds.
pub(crate) const PROFILE_BUSY: &str = "native_profile_in_use";

/// Whether a pick probes the model provider of each loopback endpoint member it would pick.
/// Read-only runs (Edit with AI, the mapping step, the overview and the roadmap check) pick
/// with `Reachable` and pass over a member whose provider refuses every connect; research
/// jobs and the service tick pick with `Unprobed` and make no connect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    Unprobed,
    Reachable,
}

/// The account a research job should run on, or why it has to wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountPick {
    NeedsPool,
    NoUsableAccount,
    Paused {
        until_ms: i64,
    },
    Unreached {
        line: String,
    },
    Account {
        connection: CredentialRef,
        account: Box<ResearchAccount>,
    },
}

impl AccountPick {
    /// The plain status the UI shows while research cannot start.
    pub fn label(&self) -> &'static str {
        match self {
            Self::NeedsPool => "Needs a pool",
            Self::NoUsableAccount => "No account in this project's pool can run this research",
            Self::Paused { .. } => {
                "Every account in this project's pool that can run this research hit its rate limit"
            }
            Self::Unreached { .. } => {
                "No account in this project's pool can reach its model provider"
            }
            Self::Account { .. } => "Ready",
        }
    }

    /// Folds another member's or harness's wait into this one when nothing can run now: the
    /// first down provider's line wins, since a run there failed at once before failover too;
    /// else the earlier pause end; else this wait. Input: the next wait, not an account.
    /// Output: the kept wait.
    pub(crate) fn or_wait(self, next: AccountPick) -> AccountPick {
        assert!(!matches!(self, Self::Account { .. }) && !matches!(next, Self::Account { .. }));
        match (self, next) {
            (kept @ Self::Unreached { .. }, _) | (_, kept @ Self::Unreached { .. }) => kept,
            (Self::Paused { until_ms }, Self::Paused { until_ms: end }) => Self::Paused {
                until_ms: until_ms.min(end),
            },
            (kept @ Self::Paused { .. }, _) | (_, kept @ Self::Paused { .. }) => kept,
            (kept, _) => kept,
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
    pick_unpaused(access, project, harness, signed_in, |_| None)
}

/// Picks like [`pick_account`] but skips accounts paused after a rate or usage limit, the
/// pause master and team turns share. `paused_until` gives the end of an account's pause
/// in ms since the epoch while it is paused. Returns [`AccountPick::Paused`] with the
/// earliest end when every account `harness` could use is paused.
pub fn pick_unpaused(
    access: &ModelAccess,
    project: &str,
    harness: &str,
    signed_in: impl Fn(&ModelConnection) -> bool,
    paused_until: impl Fn(&CredentialRef) -> Option<i64>,
) -> AccountPick {
    pick_scheduled(
        access,
        project,
        harness,
        signed_in,
        paused_until,
        &Choice::default(),
    )
}

/// Picks like [`pick_unpaused`] in the order the owner's schedule gives (DEC-096 item
/// 8): an account the schedule leaves out is skipped, and one held until a known reset
/// counts as paused until then. Takes the same inputs plus the scheduling choice.
pub fn pick_scheduled(
    access: &ModelAccess,
    project: &str,
    harness: &str,
    signed_in: impl Fn(&ModelConnection) -> bool,
    paused_until: impl Fn(&CredentialRef) -> Option<i64>,
    choice: &Choice,
) -> AccountPick {
    scheduled_pick(
        access,
        project,
        harness,
        &signed_in,
        &paused_until,
        choice,
        Reach::Unprobed,
    )
}

/// [`pick_scheduled`] with the reach rule `reach`. Takes the same inputs plus the rule.
/// Returns the pick, or [`AccountPick::Unreached`] with the first down provider's line when
/// `reach` probes and every member that could run has a provider that refuses connects.
fn scheduled_pick(
    access: &ModelAccess,
    project: &str,
    harness: &str,
    signed_in: &impl Fn(&ModelConnection) -> bool,
    paused_until: &impl Fn(&CredentialRef) -> Option<i64>,
    choice: &Choice,
    reach: Reach,
) -> AccountPick {
    let assigned: Vec<_> = access
        .pools
        .iter()
        .filter(|pool| pool.projects.iter().any(|id| id.as_str() == project))
        .collect();
    if assigned.is_empty() {
        return AccountPick::NeedsPool;
    }
    members_pick(
        access,
        assigned.iter().flat_map(|pool| pool.members.iter()),
        harness,
        signed_in,
        paused_until,
        choice,
        reach,
    )
}

/// Picks the first of `members` that `harness` can use and that is not paused, in the
/// schedule's order. Takes the model access, the members in pool order, the harness id,
/// the signed-in check for managed connections, the pause lookup and the scheduling
/// choice and the reach rule. Returns the account, else the kept wait of
/// [`AccountPick::or_wait`], else [`AccountPick::NoUsableAccount`].
fn members_pick<'a>(
    access: &ModelAccess,
    members: impl IntoIterator<Item = &'a CredentialRef>,
    harness: &str,
    signed_in: &impl Fn(&ModelConnection) -> bool,
    paused_until: &impl Fn(&CredentialRef) -> Option<i64>,
    choice: &Choice,
    _reach: Reach,
) -> AccountPick {
    let mut fitting = Vec::new();
    for member in members {
        let Some(connection) = access.connections.iter().find(|c| &c.id == member) else {
            continue;
        };
        if let Some(account) = usable(connection, harness, signed_in) {
            fitting.push((connection, account));
        }
    }
    let candidates: Vec<Candidate> = fitting
        .iter()
        .map(|(connection, _)| Candidate {
            connection: connection.id.clone(),
            load: 0,
            current: false,
        })
        .collect();
    let order = choice.order(&candidates);
    let mut waiting = AccountPick::NoUsableAccount;
    for (_, why) in &order.excluded {
        if let Some(until_ms) = why.until_ms() {
            waiting = waiting.or_wait(AccountPick::Paused { until_ms });
        }
    }
    for index in order.ready {
        let (connection, account) = &fitting[index];
        match paused_until(&connection.id) {
            None => {
                return AccountPick::Account {
                    connection: connection.id.clone(),
                    account: Box::new(account.clone()),
                };
            }
            Some(until_ms) => waiting = waiting.or_wait(AccountPick::Paused { until_ms }),
        }
    }
    waiting
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

/// Tells which research account one pool member gives a harness, by the one account rule
/// every harness shares (DEC-141 PA-8): an endpoint whose wire format the harness speaks,
/// with its key reference when it has one, or a signed-in profile of that harness. Takes the
/// connection, the harness id and the signed-in check. Returns the account, or none.
fn usable(
    connection: &ModelConnection,
    harness: &str,
    signed_in: &impl Fn(&ModelConnection) -> bool,
) -> Option<ResearchAccount> {
    let harness = ResearchHarness::of(harness)?;
    if !crate::harnesses::account_fits(harness.id(), connection) {
        return None;
    }
    let model =
        || (connection.model.id != ACCOUNT_DEFAULT_MODEL).then(|| connection.model.id.clone());
    match (&connection.endpoint, &connection.auth) {
        (ModelEndpoint::Http { url }, auth) => {
            let base_url = crate::harnesses::endpoint_base_url(harness.id(), url).to_owned();
            let model = connection.model.id.clone();
            let key = match auth {
                ModelAuthentication::ApiKey { credential_ref } => Some(credential_ref.clone()),
                _ => None,
            };
            Some(match harness {
                ResearchHarness::Codex => ResearchAccount::Endpoint {
                    base_url,
                    model,
                    key,
                },
                ResearchHarness::ClaudeCode => ResearchAccount::AnthropicEndpoint {
                    base_url,
                    model,
                    key,
                },
            })
        }
        (ModelEndpoint::Native {}, ModelAuthentication::NativeManaged { .. })
            if signed_in(connection) =>
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

/// Resolves a project to the folder the engine recorded when it was set up,
/// or else its folder under `projects_root`, and the account picked from its
/// assigned pool, rereading the engine's settings files,
/// sign-in records and account pauses on every call so pool assignment, sign-in and
/// pauses take effect without a restart. A managed account is offered only while it
/// passes the sign-in check every master and team turn uses for this engine run, and no
/// account while it is paused after a rate or usage limit.
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
    /// be read. Probes no provider: [`Self::resolve_with`] under [`Reach::Unprobed`].
    pub fn resolve(&self, project: &str, harness: &str) -> Option<(PathBuf, AccountPick)> {
        self.resolve_with(project, harness, Reach::Unprobed)
    }

    /// [`Self::resolve`] under the reach rule `reach`. Takes the project id, the harness id
    /// and the rule. Returns the folder and the pick, which under [`Reach::Reachable`] skips
    /// a member whose loopback provider refuses connects, or `None` as [`Self::resolve`].
    pub fn resolve_with(
        &self,
        project: &str,
        harness: &str,
        reach: Reach,
    ) -> Option<(PathBuf, AccountPick)> {
        CredentialRef::new(project).ok()?;
        let database = self.engine();
        let folder = database
            .as_ref()
            .and_then(|database| stored_folder(database, project))
            .unwrap_or_else(|| self.projects_root.join(project));
        let metadata = std::fs::symlink_metadata(&folder).ok()?;
        if !metadata.file_type().is_dir() || folder.canonicalize().ok()? != folder {
            return None;
        }
        let config = crate::config::resolve_config(&self.global, &self.project).ok()?;
        let access = config.settings.model_access.unwrap_or_default();
        let pools: Vec<&CredentialRef> = access
            .pools
            .iter()
            .filter(|pool| pool.projects.iter().any(|id| id.as_str() == project))
            .map(|pool| &pool.id)
            .collect();
        let choice = match database.as_ref() {
            Some(database) => Choice::read(
                database,
                &access,
                Some(project),
                &pools,
                crate::team::now_ms(),
            )
            .ok()?,
            None => Choice::default(),
        };
        let pick = scheduled_pick(
            &access,
            project,
            harness,
            &|connection| {
                database.as_ref().is_some_and(|database| {
                    match managed_sign_in(database, connection) {
                        Ok(_) => true,
                        Err(code) => code == PROFILE_BUSY,
                    }
                })
            },
            &|connection| {
                database
                    .as_ref()
                    .and_then(|database| {
                        crate::team::held_until(database, project, connection).ok()
                    })
                    .flatten()
            },
            &choice,
            reach,
        );
        Some((folder, pick))
    }

    /// The account an agent outside the research queue, such as onboarding's
    /// mapping agent, runs on for `project` from `pool`: the pool's first member
    /// the first of `harnesses` can use, else the next harness's, with the same
    /// sign-in check, account pauses and project holds as [`Self::resolve`].
    /// `harnesses` are the installed ones, Codex first. Rereads the settings on
    /// every call. Returns [`AccountPick::NeedsPool`] for an unknown pool or
    /// unreadable settings, [`AccountPick::Paused`] with the earliest end when
    /// only paused or held members fit, else [`AccountPick::NoUsableAccount`]
    /// when none fits. Probes no provider.
    pub fn pool_pick(
        &self,
        project: &str,
        pool: &CredentialRef,
        harnesses: &[ResearchHarness],
    ) -> AccountPick {
        self.pool_pick_with(project, pool, harnesses, Reach::Unprobed)
    }

    /// [`Self::pool_pick`] under the reach rule `reach`. Takes the project id, the pool, the
    /// installed harnesses and the rule. Returns the pick, which under [`Reach::Reachable`]
    /// skips a member whose loopback provider refuses connects and is
    /// [`AccountPick::Unreached`] when only such members could run.
    pub fn pool_pick_with(
        &self,
        project: &str,
        pool: &CredentialRef,
        harnesses: &[ResearchHarness],
        reach: Reach,
    ) -> AccountPick {
        let Ok(config) = crate::config::resolve_config(&self.global, &self.project) else {
            return AccountPick::NeedsPool;
        };
        let access = config.settings.model_access.unwrap_or_default();
        let Some(chosen) = access.pools.iter().find(|candidate| &candidate.id == pool) else {
            return AccountPick::NeedsPool;
        };
        let database = self.engine();
        let choice = match database.as_ref() {
            Some(database) => {
                match Choice::read(database, &access, None, &[pool], crate::team::now_ms()) {
                    Ok(choice) => choice,
                    Err(_) => return AccountPick::NeedsPool,
                }
            }
            None => Choice::default(),
        };
        let signed_in = |connection: &ModelConnection| {
            database
                .as_ref()
                .is_some_and(|database| match managed_sign_in(database, connection) {
                    Ok(_) => true,
                    Err(code) => code == PROFILE_BUSY,
                })
        };
        let paused_until = |connection: &CredentialRef| {
            database
                .as_ref()
                .and_then(|database| crate::team::held_until(database, project, connection).ok())
                .flatten()
        };
        let mut waiting = AccountPick::NoUsableAccount;
        for harness in harnesses {
            match members_pick(
                &access,
                &chosen.members,
                harness.id(),
                &signed_in,
                &paused_until,
                &choice,
                reach,
            ) {
                pick @ AccountPick::Account { .. } => {
                    assert!(
                        matches!(&pick, AccountPick::Account { account, .. } if account.harness() == *harness)
                    );
                    return pick;
                }
                pick => waiting = waiting.or_wait(pick),
            }
        }
        waiting
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

/// The folder the engine recorded for a set-up project, such as its folder
/// in the projects home. Takes the engine database and the project id.
/// Returns `None` when the project has no setup record or it cannot be read.
fn stored_folder(database: &Connection, project: &str) -> Option<PathBuf> {
    let id = crate::protocol::ProjectId::new(project).ok()?;
    let tx = database.unchecked_transaction().ok()?;
    let stored = crate::project_setup::load_setup(&tx, &id).ok()??;
    Some(stored.project_folder())
}

/// Proves a managed connection is signed in for the running engine with the
/// same check every master and team turn uses, and that the provider did not
/// reject that sign-in, as the master's account pick requires. Takes the engine
/// database and the connection. Returns the active engine host and the sign-in,
/// or a plain code such as `research_engine_not_ready`,
/// `master_authentication_stale`, `master_sign_in_rejected` or `native_profile_in_use`.
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
    if crate::model_gateway::sign_in_rejected(&tx, connection).map_err(|problem| problem.0)? {
        return Err("master_sign_in_rejected".into());
    }
    drop(tx);
    assert_eq!(fact.binding.engine_epoch, host.engine_epoch);
    assert_eq!(fact.binding.connection, *connection);
    Ok((host, fact))
}

#[cfg(test)]
#[path = "accounts_tests.rs"]
mod accounts_tests;
