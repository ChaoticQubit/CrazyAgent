use super::{Actor, CheckRefusal, immediate, owner_only, project_known, unavailable};
use crate::engine::owner::failure;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};
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

/// What a test a passing run no longer reports against its inventory baseline does at the
/// gate: nothing; a gap, kept in the note under `warn` and refused under `require`; or a
/// refusal under `warn` too. A passing run whose integrity block is missing counts as one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovedTests {
    Off,
    #[default]
    Flag,
    Refuse,
}

/// Every option of `parallel`: how many check runs go at once.
pub const PARALLEL_OPTIONS: [u8; 4] = [1, 2, 3, 4];
/// Every option of `keep_failed_snapshots`: how many failed runs keep their snapshot folder.
pub const KEEP_FAILED_OPTIONS: [u8; 6] = [0, 1, 2, 3, 4, 5];
/// Every option of `agent_requests`: how many runs one agent may ask for per hour, 0 for off.
pub const AGENT_REQUESTS_OPTIONS: [u8; 4] = [0, 2, 6, 12];

/// The checks settings in effect for a project. The default is the proposal for each key:
/// gate `warn`, one run at once, two failed snapshots kept, six agent requests per hour,
/// inventory `last_known`, skip markers `flag`, protected change `refuse`, removed tests
/// `flag`.
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
    pub removed_tests: RemovedTests,
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
    pub removed_tests: Option<RemovedTests>,
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
            removed_tests: RemovedTests::default(),
        }
    }
}

impl GateMode {
    /// Every option, in display order.
    pub const ALL: [Self; 3] = [Self::Off, Self::Warn, Self::Require];
}

impl InventoryMode {
    /// Every option, in display order.
    pub const ALL: [Self; 3] = [Self::LastKnown, Self::RunBase, Self::Off];
}

impl SkipMarkers {
    /// Every option, in display order.
    pub const ALL: [Self; 3] = [Self::Flag, Self::Refuse, Self::Off];
}

impl ProtectedChange {
    /// Every option, in display order.
    pub const ALL: [Self; 2] = [Self::Refuse, Self::Flag];
}

impl RemovedTests {
    /// Every option, in display order.
    pub const ALL: [Self; 3] = [Self::Off, Self::Flag, Self::Refuse];
}

impl ChecksSettings {
    /// Resolves the settings in effect: each key from the project's override, else the app
    /// default, else the proposal. Inputs: the app layer and the project layer, both
    /// validated. Output: the settings.
    pub fn resolve(app: &ChecksChoice, project: &ChecksChoice) -> Self {
        let proposal = Self::default();
        let resolved = Self {
            gate: project.gate.or(app.gate).unwrap_or(proposal.gate),
            parallel: project
                .parallel
                .or(app.parallel)
                .unwrap_or(proposal.parallel),
            keep_failed_snapshots: project
                .keep_failed_snapshots
                .or(app.keep_failed_snapshots)
                .unwrap_or(proposal.keep_failed_snapshots),
            agent_requests: project
                .agent_requests
                .or(app.agent_requests)
                .unwrap_or(proposal.agent_requests),
            inventory: project
                .inventory
                .or(app.inventory)
                .unwrap_or(proposal.inventory),
            skip_markers: project
                .skip_markers
                .or(app.skip_markers)
                .unwrap_or(proposal.skip_markers),
            protected_change: project
                .protected_change
                .or(app.protected_change)
                .unwrap_or(proposal.protected_change),
            removed_tests: project
                .removed_tests
                .or(app.removed_tests)
                .unwrap_or(proposal.removed_tests),
        };
        assert!(PARALLEL_OPTIONS.contains(&resolved.parallel));
        assert!(KEEP_FAILED_OPTIONS.contains(&resolved.keep_failed_snapshots));
        assert!(AGENT_REQUESTS_OPTIONS.contains(&resolved.agent_requests));
        resolved
    }
}

impl ChecksChoice {
    /// Checks each numeric key is one of its options. Output: Ok, or
    /// `checks_setting_invalid`.
    pub fn validate(&self) -> Result<(), CheckRefusal> {
        let listed = |value: Option<u8>, options: &[u8]| value.is_none_or(|v| options.contains(&v));
        if !listed(self.parallel, &PARALLEL_OPTIONS)
            || !listed(self.keep_failed_snapshots, &KEEP_FAILED_OPTIONS)
            || !listed(self.agent_requests, &AGENT_REQUESTS_OPTIONS)
        {
            return Err(failure("checks_setting_invalid"));
        }
        Ok(())
    }
}

impl Scope {
    fn key(&self) -> String {
        match self {
            Self::App => "app".into(),
            Self::Project(project) => format!("project:{}", project.as_str()),
        }
    }
}

/// Every checks settings key with all its options and the proposed default, in display
/// order. Output: the keys.
pub fn choices() -> Vec<SettingChoice> {
    fn value(option: impl Serialize) -> serde_json::Value {
        serde_json::to_value(option).expect("a checks option serializes")
    }
    fn listed<T: Serialize>(
        key: &'static str,
        options: impl IntoIterator<Item = T>,
        default: T,
    ) -> SettingChoice {
        SettingChoice {
            key,
            options: options.into_iter().map(value).collect(),
            default: value(default),
        }
    }
    let proposal = ChecksSettings::default();
    let keys = vec![
        listed("gate", GateMode::ALL, proposal.gate),
        listed("parallel", PARALLEL_OPTIONS, proposal.parallel),
        listed(
            "keep_failed_snapshots",
            KEEP_FAILED_OPTIONS,
            proposal.keep_failed_snapshots,
        ),
        listed(
            "agent_requests",
            AGENT_REQUESTS_OPTIONS,
            proposal.agent_requests,
        ),
        listed("inventory", InventoryMode::ALL, proposal.inventory),
        listed("skip_markers", SkipMarkers::ALL, proposal.skip_markers),
        listed(
            "protected_change",
            ProtectedChange::ALL,
            proposal.protected_change,
        ),
        listed("removed_tests", RemovedTests::ALL, proposal.removed_tests),
    ];
    assert!(keys.iter().all(|key| key.options.contains(&key.default)));
    keys
}

/// Reads one saved layer. Inputs: the engine connection or a transaction on it, and the
/// scope. Output: the layer and its version, 0 and an empty layer when never saved, or
/// `checks_unavailable`.
pub fn read(db: &Connection, scope: &Scope) -> Result<(ChecksChoice, u64), CheckRefusal> {
    let row: Option<(String, i64)> = db
        .query_row(
            "SELECT body,version FROM check_settings WHERE scope=?1",
            [scope.key()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((body, version)) = row else {
        return Ok((ChecksChoice::default(), 0));
    };
    let choice: ChecksChoice = serde_json::from_str(&body).map_err(unavailable)?;
    choice.validate().map_err(unavailable)?;
    let version = u64::try_from(version).map_err(unavailable)?;
    assert!(version > 0);
    Ok((choice, version))
}

/// Saves one layer whole when it is still at the version the owner read. Inputs: the engine
/// connection, the scope, the layer, that version and the actor. Output: the new version, or
/// `checks_owner_only`, `checks_setting_invalid`, `checks_project_missing`,
/// `checks_settings_stale` or `checks_unavailable`.
pub fn write(
    db: &Connection,
    scope: &Scope,
    choice: &ChecksChoice,
    version: u64,
    actor: Actor,
) -> Result<u64, CheckRefusal> {
    owner_only(actor)?;
    choice.validate()?;
    let tx = immediate(db)?;
    if let Scope::Project(project) = scope {
        project_known(&tx, project)?;
    }
    let (_, saved) = read(&tx, scope)?;
    if saved != version {
        return Err(failure("checks_settings_stale"));
    }
    let next = version
        .checked_add(1)
        .ok_or_else(|| failure("checks_settings_stale"))?;
    tx.execute(
        "INSERT INTO check_settings(scope,body,version) VALUES (?1,?2,?3)
         ON CONFLICT(scope) DO UPDATE SET body=excluded.body, version=excluded.version",
        params![
            scope.key(),
            serde_json::to_string(choice).map_err(unavailable)?,
            i64::try_from(next).map_err(unavailable)?
        ],
    )
    .map_err(unavailable)?;
    tx.commit().map_err(unavailable)?;
    assert!(next > saved);
    Ok(next)
}

/// The settings in effect for one project. Inputs: the engine connection or a transaction
/// on it, and the project. Output: the resolved settings, or `checks_unavailable`.
pub fn effective(db: &Connection, project: &ProjectId) -> Result<ChecksSettings, CheckRefusal> {
    let (app, _) = read(db, &Scope::App)?;
    let (own, _) = read(db, &Scope::Project(project.clone()))?;
    Ok(ChecksSettings::resolve(&app, &own))
}
