use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock, PoisonError};

use bus_gateway::{Gateway, Grants, Refusals};
use crazyagent_plugin_agent_quit as agent_quit;
use rusqlite::Transaction;
use sdk::settings::{self as sdk_settings, Provide};
use sdk::{Capability, Refusal, Tier};

use crate::engine::EngineError;
use crate::protocol::Outcome;
use crate::quit_settings::{LeftRunning, OnQuit, QuitSettings};

#[cfg(test)]
mod tests;

/// The most first-seen (plugin, capability) refusal pairs one run keeps for the log.
pub(crate) const REFUSALS_PENDING_MAX: usize = 1_024;

/// Saves the owner's agents-on-quit choice through the bundled agent-quit plugin, the
/// generated SDK and the bus gateway's grant check. Inputs: the request transaction, the
/// signed revision and the settings. Output: the same outcome or error as a direct save,
/// or `plugin_call_refused`.
pub(crate) fn save_quit(
    tx: &Transaction<'_>,
    revision: u64,
    settings: &QuitSettings,
) -> Result<Outcome, EngineError> {
    save_quit_with(bundled_grants(), engine_refusals(), tx, revision, settings)
}

/// Saves the quit settings through the plugin with a given grant table and refusal sink.
/// Inputs: the grants, the sink, the request transaction, the signed revision and the
/// settings. Output: the provider's exact outcome or error, or `plugin_call_refused`.
pub(crate) fn save_quit_with(
    grants: &Grants,
    refusals: &EngineRefusals,
    tx: &Transaction<'_>,
    revision: u64,
    settings: &QuitSettings,
) -> Result<Outcome, EngineError> {
    let provider = QuitProvider {
        tx,
        revision,
        outcome: Cell::new(None),
        error: RefCell::new(None),
    };
    let gateway = Gateway::new(grants, &provider, refusals);
    let result = agent_quit::save(&gateway.bind(agent_quit::ID), (*settings).into());
    let outcome = provider.outcome.take();
    let error = provider.error.take();
    match result {
        Ok(()) => {
            assert!(error.is_none());
            Ok(outcome.expect("an applied save keeps its outcome"))
        }
        Err(Refusal::NotGranted { .. } | Refusal::TierTooHigh { .. }) => {
            assert!(
                outcome.is_none() && error.is_none(),
                "a refused call never ran"
            );
            Err(EngineError("plugin_call_refused".to_owned()))
        }
        Err(Refusal::Rejected { .. }) => {
            assert!(error.is_none());
            Ok(outcome.expect("a rejected save keeps its outcome"))
        }
        Err(Refusal::Failed { .. }) => {
            assert!(outcome.is_none());
            Err(error.expect("a failed save keeps its error"))
        }
    }
}

/// The bundled plugins' grant table: agent-quit at tier 2 holding `settings.write`, as its
/// `plugin.toml` asks. Output: the table, built once.
pub(crate) fn bundled_grants() -> &'static Grants {
    static GRANTS: OnceLock<Grants> = OnceLock::new();
    GRANTS.get_or_init(|| {
        Grants::new(&[(agent_quit::ID, Tier::T2, &["settings.write"])])
            .expect("the bundled grant table builds")
    })
}

/// The one process-wide refusal sink the bundled path reports to. Output: the sink.
fn engine_refusals() -> &'static EngineRefusals {
    static REFUSALS: OnceLock<EngineRefusals> = OnceLock::new();
    REFUSALS.get_or_init(EngineRefusals::new)
}

/// The engine's refusal sink: one diagnostic line per refused call, a per-plugin count,
/// and the (plugin, capability) pairs first refused in this run, bounded by
/// `REFUSALS_PENDING_MAX`. It records static ids only, never call inputs.
pub(crate) struct EngineRefusals {
    counts: Mutex<BTreeMap<&'static str, u64>>,
    pending: Mutex<BTreeSet<(&'static str, &'static str)>>,
}

impl EngineRefusals {
    /// An empty sink. Output: the sink.
    pub(crate) fn new() -> Self {
        Self {
            counts: Mutex::new(BTreeMap::new()),
            pending: Mutex::new(BTreeSet::new()),
        }
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
        if pending.len() < REFUSALS_PENDING_MAX {
            pending.insert((plugin, capability.name));
        }
        assert!(pending.len() <= REFUSALS_PENDING_MAX);
    }
}

/// The settings provider the gateway hands an admitted quit save to: it saves in the
/// request transaction and keeps the exact outcome or error for the caller.
struct QuitProvider<'t, 'c> {
    tx: &'t Transaction<'c>,
    revision: u64,
    outcome: Cell<Option<Outcome>>,
    error: RefCell<Option<EngineError>>,
}

impl Provide for QuitProvider<'_, '_> {
    fn save_quit(&self, settings: sdk_settings::QuitSettings) -> Result<(), Refusal> {
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
