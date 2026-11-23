use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock, PoisonError};

use bus_gateway::{Gateway, Grants, Refusals};
use crazyagent_plugin_agent_quit as agent_quit;
use kernel_plugin_host::generated::permissions::{BUNDLED_GRANTS, PERMISSIONS};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use sdk::settings::{self as sdk_settings, Provide};
use sdk::{Capability, Refusal};
use sdk_host::{Bound, Providers};
use serde::{Deserialize, Serialize};

use crate::engine::EngineError;
use crate::engine::owner::{HostStamp, failure};
use crate::protocol::{Cursor, Outcome};
use crate::quit_settings::{LeftRunning, OnQuit, QuitSettings};
use crate::state_log::Aggregate;
use crate::state_log::fence::Fence;
use kernel_storage::state_log::Draft;

#[cfg(test)]
mod tests;

/// The kind of the event that logs a plugin's first refused call of a run, on the
/// `plugins` aggregate.
pub const KIND_CALL_REFUSED: &str = "CallRefused";

/// The schema of a `CallRefused` body.
const CALL_REFUSED_SCHEMA: u16 = 1;

/// A `CallRefused` body: the refused plugin, capability and permission, static ids only,
/// never a call input.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CallRefused {
    plugin: String,
    capability: String,
    needs: String,
}

/// Whether a body is a `CallRefused` body: exactly its three keys, a bundled plugin id and a
/// capability with its permission from the generated `sdk::CAPABILITIES`, so it never holds free text.
/// Input: the body bytes. Output: true when it is.
pub(crate) fn call_refused_closed(body: &[u8]) -> bool {
    let Ok(refused) = serde_json::from_slice::<CallRefused>(body) else {
        return false;
    };
    BUNDLED_GRANTS
        .iter()
        .any(|grant| grant.plugin == refused.plugin)
        && sdk::CAPABILITIES.iter().any(|capability| {
            capability.name == refused.capability && capability.needs == refused.needs
        })
}

/// One queued first refusal: plugin id, capability name and the permission it needs.
type Queued = (&'static str, &'static str, &'static str);

/// What one drain committed: the `CallRefused` events it appended and the state revision
/// its own transaction carries, so a receipt can name it without a read after the commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Drained {
    pub(crate) appended: usize,
    pub(crate) revision: Cursor,
}

/// Appends each first-seen refusal queued in a sink as one `CallRefused` event, in one
/// engine-fenced transaction of its own, so the event outlives the refused request's
/// rollback; an empty queue opens no transaction. Inputs: the connection, the engine host
/// and the sink. Output: what the drain committed, `None` for an empty queue, or the
/// refusal with the queue kept.
pub(crate) fn drain_refusals(
    connection: &mut Connection,
    host: &HostStamp,
    refusals: &EngineRefusals,
) -> Result<Option<Drained>, EngineError> {
    let queued = refusals.take_queued();
    if queued.is_empty() {
        return Ok(None);
    }
    let appended = append_refusals(connection, host, &queued);
    if appended.is_err() {
        refusals.requeue(queued);
    }
    appended.map(Some)
}

/// Appends queued refusals as `CallRefused` events on the `plugins` aggregate and commits.
/// Inputs: the connection, the engine host and the refusals. Output: their number and the
/// revision the transaction committed, or the fence's, the log's or SQLite's refusal.
fn append_refusals(
    connection: &mut Connection,
    host: &HostStamp,
    queued: &[Queued],
) -> Result<Drained, EngineError> {
    assert!(!queued.is_empty() && queued.len() <= REFUSALS_PENDING_MAX);
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| failure("plugin_refusal_log_begin"))?;
    let revision = {
        let mut appender = crate::state_log::own(&tx, Fence::Engine(host.clone()))?;
        let at_ms = u64::try_from(crate::team::now_ms()).unwrap_or(0);
        for (plugin, capability, needs) in queued {
            let body = serde_json::to_vec(&CallRefused {
                plugin: (*plugin).to_owned(),
                capability: (*capability).to_owned(),
                needs: (*needs).to_owned(),
            })
            .map_err(|_| failure("plugin_refusal_log_body"))?;
            assert!(
                call_refused_closed(&body),
                "the engine names only its own ids"
            );
            appender.append(&Draft {
                aggregate: Aggregate::Plugins.into(),
                kind: KIND_CALL_REFUSED,
                schema: CALL_REFUSED_SCHEMA,
                at_ms,
                body: &body,
            })?;
        }
        appender.revision()
    };
    tx.commit()
        .map_err(|_| failure("plugin_refusal_log_commit"))?;
    Ok(Drained {
        appended: queued.len(),
        revision: Cursor::new(revision),
    })
}

/// The most first-seen (plugin, capability) refusal pairs one run keeps for the log.
pub(crate) const REFUSALS_PENDING_MAX: usize = 1_024;

/// A bundled plugin's quit-save entry: what the plugin crate exports for the engine to
/// call with the host bound to it. Inputs: the host and the settings. Output: the
/// plugin's result, which the engine reads as input, never as an invariant.
pub(crate) type Entry = fn(&dyn sdk::Host, sdk_settings::QuitSettings) -> Result<(), Refusal>;

/// Saves the owner's agents-on-quit choice through the bundled agent-quit plugin, the
/// generated SDK and the bus gateway's grant check. Inputs: the request transaction, the
/// signed revision and the settings. Output: the same outcome or error as a direct save,
/// or `plugin_call_refused`.
pub(crate) fn save_quit(
    tx: &Transaction<'_>,
    revision: u64,
    settings: &QuitSettings,
) -> Result<Outcome, EngineError> {
    save_quit_with(
        agent_quit::save,
        bundled_grants(),
        engine_refusals(),
        tx,
        revision,
        settings,
    )
}

/// Saves the quit settings through a plugin entry with a given grant table and refusal
/// sink. The plugin's result is input: a refusal the gateway recorded in this call is
/// `plugin_call_refused`, one admitted host call whose result the plugin returns is that
/// result, and every other shape is `plugin_call_invalid`. Inputs: the entry, the grants,
/// the sink, the request transaction, the signed revision and the settings.
pub(crate) fn save_quit_with(
    entry: Entry,
    grants: &Grants,
    refusals: &EngineRefusals,
    tx: &Transaction<'_>,
    revision: u64,
    settings: &QuitSettings,
) -> Result<Outcome, EngineError> {
    let provider = QuitProvider {
        tx,
        revision,
        calls: Cell::new(0),
        outcome: Cell::new(None),
        error: RefCell::new(None),
    };
    let record = CallRecord {
        sink: refusals,
        refused: Cell::new(false),
    };
    let gateway = Gateway::new(grants, &record);
    let providers = Providers {
        net: &crate::security_caps::Absent,
        process: &crate::sdk_install::InstallCaps::UNREGISTERED,
        secrets: &crate::security_caps::Absent,
        settings: &provider,
        service: &crate::security_caps::Absent,
        update: &crate::security_caps::Absent,
    };
    let host = Bound::new(gateway.bind(BUNDLED_GRANTS[0].plugin), &providers);
    let result = entry(&host, (*settings).into());
    if record.refused.get() {
        return Err(EngineError("plugin_call_refused".to_owned()));
    }
    let invalid = || Err(EngineError("plugin_call_invalid".to_owned()));
    if provider.calls.get() != 1 {
        return invalid();
    }
    match (result, provider.outcome.take(), provider.error.take()) {
        (Ok(()), Some(outcome), None) if !matches!(outcome, Outcome::Rejected { .. }) => {
            Ok(outcome)
        }
        (Err(Refusal::Rejected { code }), Some(Outcome::Rejected { code: kept }), None)
            if code == kept =>
        {
            Ok(Outcome::Rejected { code })
        }
        (Err(Refusal::Failed { code }), None, Some(error)) if code == error.0 => Err(error),
        _ => invalid(),
    }
}

/// The refusal sink one call sees: it forwards each refusal to the engine sink and keeps
/// whether the gateway refused anything in this call, so "refused" is the gateway's own
/// record, never the variant the plugin returned.
struct CallRecord<'s> {
    sink: &'s EngineRefusals,
    refused: Cell<bool>,
}

impl Refusals for CallRecord<'_> {
    fn refused(&self, plugin: &'static str, capability: &Capability, refusal: &Refusal) {
        self.refused.set(true);
        self.sink.refused(plugin, capability, refusal);
    }
}

/// The bundled plugins' grant table, built once from the generated `BUNDLED_GRANTS` and
/// checked against the generated `PERMISSIONS`. Output: the table.
pub(crate) fn bundled_grants() -> &'static Grants {
    static GRANTS: OnceLock<Grants> = OnceLock::new();
    GRANTS.get_or_init(|| {
        Grants::from_table(&BUNDLED_GRANTS, &PERMISSIONS).expect("the bundled grant table builds")
    })
}

/// The one process-wide refusal sink the bundled path reports to. Output: the sink.
pub(crate) fn engine_refusals() -> &'static EngineRefusals {
    static REFUSALS: OnceLock<EngineRefusals> = OnceLock::new();
    REFUSALS.get_or_init(EngineRefusals::new)
}

/// The engine's refusal sink: one diagnostic line per refused call, a per-plugin count,
/// the (plugin, capability) pairs first refused in this run, bounded by
/// `REFUSALS_PENDING_MAX`, and the queue of those not yet logged. Static ids only.
pub(crate) struct EngineRefusals {
    counts: Mutex<BTreeMap<&'static str, u64>>,
    pending: Mutex<BTreeSet<(&'static str, &'static str)>>,
    queue: Mutex<Vec<Queued>>,
}

impl EngineRefusals {
    /// An empty sink. Output: the sink.
    pub(crate) fn new() -> Self {
        Self {
            counts: Mutex::new(BTreeMap::new()),
            pending: Mutex::new(BTreeSet::new()),
            queue: Mutex::new(Vec::new()),
        }
    }

    /// Takes the first refusals queued since the last take. Output: them, in order.
    fn take_queued(&self) -> Vec<Queued> {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        std::mem::take(&mut *queue)
    }

    /// Puts refusals whose events were not written back at the front of the queue.
    /// Input: the refusals. Output: none.
    fn requeue(&self, queued: Vec<Queued>) {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        let later = std::mem::replace(&mut *queue, queued);
        queue.extend(later);
        assert!(queue.len() <= REFUSALS_PENDING_MAX);
    }

    /// How many calls of one plugin were refused. Input: the plugin id. Output: the count.
    #[cfg(test)]
    pub(crate) fn count(&self, plugin: &str) -> u64 {
        let counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        counts.get(plugin).copied().unwrap_or(0)
    }

    /// How many first-seen refusal pairs are kept. Output: the number.
    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        let pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        pending.len()
    }
}

impl Refusals for EngineRefusals {
    fn refused(&self, plugin: &'static str, capability: &Capability, refusal: &Refusal) {
        let needs = match refusal {
            Refusal::NotGranted { needs, .. } => needs,
            _ => capability.needs,
        };
        eprintln!(
            "plugin_call_refused plugin={plugin} capability={} needs={needs}",
            capability.name
        );
        let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        let count = counts.entry(plugin).or_insert(0);
        *count = count.saturating_add(1);
        drop(counts);
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        if pending.len() < REFUSALS_PENDING_MAX && pending.insert((plugin, capability.name)) {
            let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
            queue.push((plugin, capability.name, needs));
            assert!(queue.len() <= pending.len());
        }
        assert!(pending.len() <= REFUSALS_PENDING_MAX);
    }
}

/// The settings provider the gateway hands an admitted quit save to: it saves in the
/// request transaction once, counts every call, and keeps the exact outcome or error for
/// the caller.
struct QuitProvider<'t, 'c> {
    tx: &'t Transaction<'c>,
    revision: u64,
    calls: Cell<u32>,
    outcome: Cell<Option<Outcome>>,
    error: RefCell<Option<EngineError>>,
}

impl Provide for QuitProvider<'_, '_> {
    fn save_quit(
        &self,
        _caller: &'static str,
        settings: sdk_settings::QuitSettings,
    ) -> Result<(), Refusal> {
        let calls = self.calls.get().saturating_add(1);
        self.calls.set(calls);
        if calls > 1 {
            return Err(Refusal::Failed {
                code: "plugin_call_invalid".to_owned(),
            });
        }
        match crate::settings_log::save_quit_at(self.tx, self.revision, &settings.into()) {
            Ok(Outcome::Applied) => {
                self.outcome.set(Some(Outcome::Applied));
                Ok(())
            }
            Ok(Outcome::Rejected { code }) => {
                self.outcome
                    .set(Some(Outcome::Rejected { code: code.clone() }));
                Err(Refusal::Rejected { code })
            }
            Ok(other) => {
                self.outcome.set(Some(other));
                Ok(())
            }
            Err(error) => {
                let code = error.0.clone();
                *self.error.borrow_mut() = Some(error);
                Err(Refusal::Failed { code })
            }
        }
    }
}

impl From<QuitSettings> for sdk_settings::QuitSettings {
    fn from(settings: QuitSettings) -> Self {
        Self {
            on_quit: match settings.on_quit {
                OnQuit::KeepRunning => sdk_settings::OnQuit::KeepRunning,
                OnQuit::Stop => sdk_settings::OnQuit::Stop,
            },
            left_running: match settings.left_running {
                LeftRunning::AfterHours => sdk_settings::LeftRunning::AfterHours,
                LeftRunning::Never => sdk_settings::LeftRunning::Never,
            },
            hours: settings.hours,
        }
    }
}

impl From<sdk_settings::QuitSettings> for QuitSettings {
    fn from(settings: sdk_settings::QuitSettings) -> Self {
        Self {
            on_quit: match settings.on_quit {
                sdk_settings::OnQuit::KeepRunning => OnQuit::KeepRunning,
                sdk_settings::OnQuit::Stop => OnQuit::Stop,
            },
            left_running: match settings.left_running {
                sdk_settings::LeftRunning::AfterHours => LeftRunning::AfterHours,
                sdk_settings::LeftRunning::Never => LeftRunning::Never,
            },
            hours: settings.hours,
        }
    }
}
