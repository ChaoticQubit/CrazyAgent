use super::error;
use crate::engine::EngineError;
use crate::engine::owner::{Admission, HostStamp};
use crate::storage::opening::Opening;
use crate::storage::{check_owner_admission, check_owner_host};
use rusqlite::{OptionalExtension, Transaction};
use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, PoisonError};

#[cfg(test)]
mod tests;

/// The most engine hosts one process keeps started; past it the oldest is dropped, so its
/// writers are refused, never let through.
pub(crate) const HOSTS_MAX: usize = 256;

static HOSTS: Mutex<Hosts> = Mutex::new(Hosts::new());

/// The proof a writer brings to append to the log, one per writer class (v2.5 plan section
/// 3.2): an owner request's admission, the engine host stamp of a writer inside the engine
/// process, or the storage open's proof for the open and its schema steps, which write
/// before any host starts (v2.5 L6).
#[derive(Clone)]
pub enum Fence {
    Owner(Admission),
    Engine(HostStamp),
    Opening(Opening),
}

/// The engine hosts started in this process and not stopped, oldest first, at most
/// [`HOSTS_MAX`], each as its `db_instance` and `engine_epoch`.
pub(crate) struct Hosts {
    hosts: VecDeque<(String, String)>,
}

impl Hosts {
    /// No host started. Output: the empty set.
    pub(crate) const fn new() -> Self {
        Self {
            hosts: VecDeque::new(),
        }
    }

    /// Starts a host as the newest, moving it there when it is already started, and drops
    /// the oldest past [`HOSTS_MAX`]. Input: the host. Output: none.
    pub(crate) fn start(&mut self, host: &HostStamp) {
        self.stop(host);
        if self.hosts.len() == HOSTS_MAX {
            self.hosts.pop_front();
        }
        self.hosts
            .push_back((host.db_instance.clone(), host.engine_epoch.clone()));
    }

    /// Stops a host, so its writers are refused. Input: the host. Output: none.
    pub(crate) fn stop(&mut self, host: &HostStamp) {
        self.hosts.retain(|(db_instance, engine_epoch)| {
            *db_instance != host.db_instance || *engine_epoch != host.engine_epoch
        });
    }

    /// Whether a host is started. Inputs: its `db_instance` and `engine_epoch`. Output: true
    /// when it is.
    pub(crate) fn holds(&self, db_instance: &str, engine_epoch: &str) -> bool {
        self.hosts
            .iter()
            .any(|(instance, epoch)| instance == db_instance && epoch == engine_epoch)
    }

    /// Whether any host of a store is started, whatever its epoch. Input: the store's
    /// `db_instance`. Output: true when one is.
    pub(crate) fn runs(&self, db_instance: &str) -> bool {
        self.hosts.iter().any(|(instance, _)| instance == db_instance)
    }
}

/// Records that this process runs the engine host, so the writers it fences can append.
/// The storage actor calls it once its owner runtime starts. Input: the host. Output: none.
pub fn host_started(host: &HostStamp) {
    hosts().start(host);
}

/// Records that this process no longer runs the engine host, refusing its writers. The
/// storage actor calls it when its owner runtime stops or closes. Input: the host. Output:
/// none.
pub fn host_stopped(host: &HostStamp) {
    hosts().stop(host);
}

/// Whether this process runs the engine host. Input: the host. Output: true when it does.
pub fn hosted(host: &HostStamp) -> bool {
    hosts().holds(&host.db_instance, &host.engine_epoch)
}

/// Checks a writer's fence inside its transaction. The host must run in this process, so a
/// writer outside the engine process is refused; then the store must agree: an owner
/// request's admission through `check_owner_admission`, an engine writer's stamp through
/// `check_owner_host`, each refusing a stale `engine_epoch`. An opening writer is the
/// reverse: no host of its store may run. Inputs: the transaction and the fence. Output:
/// Ok, or `state_log_fence_out_of_process`, `owner_runtime_stale`,
/// `owner_grant_stale_or_revoked` or `state_log_opening_host_running`.
pub fn check(tx: &Transaction<'_>, fence: &Fence) -> Result<(), EngineError> {
    let (db_instance, engine_epoch) = match fence {
        Fence::Owner(admission) => (&admission.db_instance, &admission.engine_epoch),
        Fence::Engine(host) => (&host.db_instance, &host.engine_epoch),
        Fence::Opening(_) => return check_opening(tx),
    };
    if !hosts().holds(db_instance, engine_epoch) {
        return Err(error("state_log_fence_out_of_process"));
    }
    match fence {
        Fence::Owner(admission) => check_owner_admission(tx, admission),
        Fence::Engine(host) => check_owner_host(tx, host),
        Fence::Opening(_) => unreachable!("an opening writer is checked above"),
    }
}

/// Checks an opening writer: once a host of its store runs in this process, every writer
/// must bring that host's fence, so the opening class is refused. A store with no owner
/// tables yet has had no host. The check is per store, not per process, since a test
/// process runs many stores. Input: the transaction. Output: Ok, or
/// `state_log_opening_host_running`.
fn check_opening(tx: &Transaction<'_>) -> Result<(), EngineError> {
    let owned: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='owner_meta')",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if !owned {
        return Ok(());
    }
    let instance: Option<String> = tx
        .query_row(
            "SELECT db_instance FROM owner_meta WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    assert!(
        instance.as_ref().is_none_or(|instance| instance.len() == 64),
        "owner_meta keeps a 64-character instance"
    );
    if instance.is_some_and(|instance| hosts().runs(&instance)) {
        return Err(error("state_log_opening_host_running"));
    }
    Ok(())
}

/// The process's started hosts, taken over from a thread that panicked while holding them,
/// since every change leaves the set whole. Output: the guard.
fn hosts() -> MutexGuard<'static, Hosts> {
    HOSTS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Seeds a store with the owner tables storage.rs creates, holding one fixed active host and
/// one active tui owner, and starts that host in this process. Input: the store. Output:
/// none.
#[cfg(test)]
pub(crate) fn seed(db: &rusqlite::Connection) {
    let host = test_host();
    let owner = test_owner();
    db.execute_batch(&format!(
        "CREATE TABLE owner_meta (
            singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
            schema_version INTEGER NOT NULL CHECK(schema_version = 1),
            db_instance TEXT NOT NULL CHECK(length(db_instance) = 64),
            engine_epoch TEXT NOT NULL CHECK(length(engine_epoch) = 64),
            runtime_state TEXT NOT NULL CHECK(runtime_state IN ('offline', 'active')));
        CREATE TABLE owner_identities (
            owner_id TEXT NOT NULL PRIMARY KEY CHECK(length(owner_id) = 64),
            client_kind TEXT NOT NULL UNIQUE CHECK(client_kind IN ('tui', 'desktop')),
            owner_epoch INTEGER NOT NULL CHECK(owner_epoch >= 1),
            status TEXT NOT NULL CHECK(status IN ('active', 'revoked')));
        INSERT INTO owner_meta VALUES (1,1,'{}','{}','active');
        INSERT INTO owner_identities VALUES ('{}','tui',{},'active');",
        host.db_instance, host.engine_epoch, owner.owner_id, owner.owner_epoch
    ))
    .unwrap();
    host_started(&host);
}

/// The fixed host [`seed`] writes. Output: the stamp.
#[cfg(test)]
pub(crate) fn test_host() -> HostStamp {
    HostStamp {
        db_instance: "a".repeat(64),
        engine_epoch: "b".repeat(64),
    }
}

/// The admission of the owner [`seed`] writes, on its host. Output: the admission.
#[cfg(test)]
pub(crate) fn test_owner() -> Admission {
    let host = test_host();
    Admission {
        db_instance: host.db_instance,
        engine_epoch: host.engine_epoch,
        owner_id: "c".repeat(64),
        owner_epoch: 1,
    }
}

/// The engine fence of the host [`seed`] writes. Output: the fence.
#[cfg(test)]
pub(crate) fn engine() -> Fence {
    Fence::Engine(test_host())
}
