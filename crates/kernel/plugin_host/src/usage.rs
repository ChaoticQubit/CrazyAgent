use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

use crate::Refused;

/// The most runs one plugin may be held by at once.
pub const HOLDERS_MAX: usize = 128;

/// How a run ends when the owner turns off a plugin it holds and the setting is
/// [`TurnOff::StopRuns`].
pub const STOPPED_LINE: &str = "Stopped: plugin turned off";

/// What turning a plugin off does to runs already holding it, the owner setting
/// `plugins.when_turned_off`: refuse only new calls (the default), or stop those runs too.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOff {
    #[default]
    RefuseNewCalls,
    StopRuns,
}

/// A run holding a plugin, as the confirm before turning it off names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub run_id: String,
    pub label: String,
}

/// One change to a trusted plugin's plugin.toml, named by the SHA-256 the owner trusted and
/// the SHA-256 the file holds now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeNotice {
    pub plugin: String,
    pub trusted_sha256: String,
    pub current_sha256: String,
}

type Stop = Box<dyn FnOnce() + Send>;

struct Held {
    id: u64,
    holder: Holder,
    stop: Option<Stop>,
}

#[derive(Default)]
struct State {
    next: u64,
    held: BTreeMap<String, Vec<Held>>,
    notified: BTreeMap<String, String>,
    pending: BTreeMap<String, ChangeNotice>,
}

/// Which runs hold which plugin, and the change notices not yet taken. The engine owns one
/// and passes it in; the plugin host keeps no global state.
#[derive(Clone, Default)]
pub struct Usage {
    state: Arc<Mutex<State>>,
}

/// One run's hold on a plugin, released when it drops.
pub struct Lease {
    state: Arc<Mutex<State>>,
    plugin: String,
    id: u64,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Usage {
    /// Records that a run holds a plugin until the returned lease drops. Inputs: the plugin,
    /// the run and the stop the engine runs if the plugin is turned off with `stop_runs`.
    /// Output: the lease, or `plugin_holders_capacity` past [`HOLDERS_MAX`].
    pub fn hold(&self, plugin: &str, holder: Holder, stop: Stop) -> Result<Lease, Refused> {
        assert!(!plugin.is_empty());
        let mut state = lock(&self.state);
        let id = state.next;
        let held = state.held.entry(plugin.to_owned()).or_default();
        if held.len() >= HOLDERS_MAX {
            return Err(Refused {
                code: "plugin_holders_capacity",
                line: 1,
            });
        }
        held.push(Held {
            id,
            holder,
            stop: Some(stop),
        });
        assert!(held.len() <= HOLDERS_MAX);
        state.next = id.checked_add(1).expect("lease ids never run out");
        Ok(Lease {
            state: Arc::clone(&self.state),
            plugin: plugin.to_owned(),
            id,
        })
    }

    /// The runs holding a plugin, in the order they took it. Input: the plugin. Output:
    /// the holders, empty when none.
    pub fn users(&self, plugin: &str) -> Vec<Holder> {
        lock(&self.state)
            .held
            .get(plugin)
            .map(|held| held.iter().map(|held| held.holder.clone()).collect())
            .unwrap_or_default()
    }

    /// Runs the stop of every run holding a plugin that was not stopped before, outside the
    /// lock. The runs stay listed until their leases drop. Input: the plugin. Output: how
    /// many stops ran.
    pub fn stop_runs(&self, plugin: &str) -> usize {
        let stops: Vec<Stop> = lock(&self.state)
            .held
            .get_mut(plugin)
            .map(|held| {
                held.iter_mut()
                    .filter_map(|held| held.stop.take())
                    .collect()
            })
            .unwrap_or_default();
        let count = stops.len();
        assert!(count <= HOLDERS_MAX);
        for stop in stops {
            stop();
        }
        count
    }

    /// Keeps a notice for a change not noticed before; the same change again keeps none.
    /// Input: the notice. Output: whether it was new.
    pub(crate) fn note_change(&self, notice: ChangeNotice) -> bool {
        let mut state = lock(&self.state);
        if state.notified.get(&notice.plugin) == Some(&notice.current_sha256) {
            return false;
        }
        state
            .notified
            .insert(notice.plugin.clone(), notice.current_sha256.clone());
        state.pending.insert(notice.plugin.clone(), notice);
        true
    }

    /// Forgets the change noticed for a plugin that runs again, so its next change notifies
    /// even with a text seen before. A notice not yet taken stays. Input: the plugin.
    /// Output: none.
    pub(crate) fn settled(&self, plugin: &str) {
        lock(&self.state).notified.remove(plugin);
    }

    /// Takes the change notices kept since the last take, at most one per plugin, for the
    /// engine to emit. No input. Output: the notices, by plugin name.
    pub fn take_notices(&self) -> Vec<ChangeNotice> {
        let pending = std::mem::take(&mut lock(&self.state).pending);
        pending.into_values().collect()
    }
}

impl std::fmt::Debug for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = lock(&self.state);
        f.debug_struct("Usage")
            .field("plugins_held", &state.held.len())
            .field("notices", &state.pending.len())
            .finish()
    }
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease")
            .field("plugin", &self.plugin)
            .field("id", &self.id)
            .finish()
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = lock(&self.state);
        let Some(held) = state.held.get_mut(&self.plugin) else {
            return;
        };
        held.retain(|held| held.id != self.id);
        if held.is_empty() {
            state.held.remove(&self.plugin);
        }
    }
}
