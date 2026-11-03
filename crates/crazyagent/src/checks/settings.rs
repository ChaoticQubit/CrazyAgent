use super::{Actor, CheckRefusal};
use crate::protocol::ProjectId;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// What the owner's Accept asks of checks: nothing; Accept proceeds and the change set and
/// ticket record any gap; or Accept refuses unless every required check passed on the
/// reviewed head under the current profile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateMode {
    Off,
    #[default]
    Warn,
    Require,
}

/// How a run's executed test names are compared: with the last passed run of the same check
/// on the base commit, also by running the check on the base, or not at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InventoryMode {
    #[default]
    LastKnown,
    RunBase,
    Off,
}

/// What an added skip marker in a change set does: flags the run, refuses it, or nothing.
/// It also covers a test the baseline run executed that this run skips, which is seen
/// only when `inventory` is not `off` and a matching baseline run exists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipMarkers {
    #[default]
    Flag,
    Refuse,
    Off,
}

/// What a change set touching a protected path does: it cannot pass the gate, or it is
/// flagged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedChange {
    #[default]
    Refuse,
    Flag,
}

/// Every option of `parallel`: how many check runs go at once.
pub const PARALLEL_OPTIONS: [u8; 4] = [1, 2, 3, 4];
/// Every option of `keep_failed_snapshots`: how many failed runs keep their snapshot folder.
pub const KEEP_FAILED_OPTIONS: [u8; 6] = [0, 1, 2, 3, 4, 5];
/// Every option of `agent_requests`: how many runs one agent may ask for per hour, 0 for off.
pub const AGENT_REQUESTS_OPTIONS: [u8; 4] = [0, 2, 6, 12];

/// The checks settings in effect for a project. The default is the proposal for each key:
/// gate `warn`, one run at once, two failed snapshots kept, six agent requests per hour,
/// inventory `last_known`, skip markers `flag`, protected change `refuse`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChecksSettings {
    pub gate: GateMode,
    pub parallel: u8,
    pub keep_failed_snapshots: u8,
    pub agent_requests: u8,
    pub inventory: InventoryMode,
    pub skip_markers: SkipMarkers,
    pub protected_change: ProtectedChange,
}

/// One layer of checks settings the owner saved: the app default, or a project's override.
/// A key left out takes the layer below it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChecksChoice {
    pub gate: Option<GateMode>,
    pub parallel: Option<u8>,
    pub keep_failed_snapshots: Option<u8>,
    pub agent_requests: Option<u8>,
    pub inventory: Option<InventoryMode>,
    pub skip_markers: Option<SkipMarkers>,
    pub protected_change: Option<ProtectedChange>,
}

/// Which saved layer a read or write names: the app default, or one project's override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    App,
    Project(ProjectId),
}

/// One settings key as a settings page lists it: the key, every option, and the proposed
/// default among them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SettingChoice {
    pub key: &'static str,
    pub options: Vec<serde_json::Value>,
    pub default: serde_json::Value,
}

impl Default for ChecksSettings {
    fn default() -> Self {
        Self {
            gate: GateMode::default(),
            parallel: 1,
            keep_failed_snapshots: 2,
            agent_requests: 6,
            inventory: InventoryMode::default(),
            skip_markers: SkipMarkers::default(),
            protected_change: ProtectedChange::default(),
        }
    }
}

impl ChecksSettings {
    /// Resolves the settings in effect: each key from the project's override, else the app
    /// default, else the proposal. Inputs: the app layer and the project layer, both
    /// validated. Output: the settings.
    pub fn resolve(app: &ChecksChoice, project: &ChecksChoice) -> Self {
        let _ = (app, project);
        todo!()
    }
}

impl ChecksChoice {
    /// Checks each numeric key is one of its options. Output: Ok, or
    /// `checks_setting_invalid`.
    pub fn validate(&self) -> Result<(), CheckRefusal> {
        todo!()
    }
}

/// Every checks settings key with all its options and the proposed default, in display
/// order. Output: the keys.
pub fn choices() -> Vec<SettingChoice> {
    todo!()
}

/// Reads one saved layer. Inputs: the engine connection and the scope. Output: the layer and
/// its version, 0 and an empty layer when never saved, or `checks_unavailable`.
pub fn read(db: &Connection, scope: &Scope) -> Result<(ChecksChoice, u64), CheckRefusal> {
    let _ = (db, scope);
    todo!()
}

/// Saves one layer whole when it is still at the version the owner read. Inputs: the engine
/// connection, the scope, the layer, that version and the actor. Output: the new version, or
/// `checks_owner_only`, `checks_setting_invalid`, `checks_settings_stale` or
/// `checks_unavailable`.
pub fn write(
    db: &Connection,
    scope: &Scope,
    choice: &ChecksChoice,
    version: u64,
    actor: Actor,
) -> Result<u64, CheckRefusal> {
    let _ = (db, scope, choice, version, actor);
    todo!()
}

/// The settings in effect for one project. Inputs: the engine connection and the project.
/// Output: the resolved settings, or `checks_unavailable`.
pub fn effective(db: &Connection, project: &ProjectId) -> Result<ChecksSettings, CheckRefusal> {
    let _ = (db, project);
    todo!()
}
