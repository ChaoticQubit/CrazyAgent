use super::{engine_error, key_of, owner, unavailable};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::storage::versioned::{self, Item, Stale};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use trackwright_core::host::{
    Authority, CheckView, Evidence, EvidenceState, LinkKind, LinkState, Links, Paths, Sight,
    TicketView,
};
use trackwright_core::model::{
    Actor, AgentEditPolicy, ClosePolicy, CreatePolicy, CycleDays, ImportPolicy, OpKind, Policy,
    PolicyKey, ProjectKey, Refusal, RolloverPolicy, codes,
};
use trackwright_core::policy;
use trackwright_core::runs::{RunWriters, SourceRef};
use trackwright_core::tests_model::{
    CaseProtectedVisibility, RunRetention, SkippedCounts, TestEvidence, TestId,
};

const STORED: &str = "SELECT body FROM tracker_settings WHERE scope = ?1";
const STORE: &str = "INSERT INTO tracker_settings(scope, body) VALUES (?1, ?2)
    ON CONFLICT(scope) DO UPDATE SET body = excluded.body";
const ATTACHED: &str = "SELECT project FROM tracker_projects ORDER BY project";

/// Who may record evidence on a `manual` check: only the owner (the proposed default,
/// since a manual check is a person's judgment), or every actor the policy lets record
/// evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManualEvidence {
    #[default]
    Owner,
    Agents,
}

impl ManualEvidence {
    /// Every option, the default first.
    pub const ALL: [ManualEvidence; 2] = [ManualEvidence::Owner, ManualEvidence::Agents];
}

/// Who may record evidence on a `ci` check: the engine's own actors and the owner (the
/// proposed default, since a CI result comes from a run, not an agent's word), only the
/// owner, or every actor the policy lets record evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiEvidence {
    #[default]
    Engine,
    Owner,
    Agents,
}

impl CiEvidence {
    /// Every option, the default first.
    pub const ALL: [CiEvidence; 3] = [CiEvidence::Engine, CiEvidence::Owner, CiEvidence::Agents];
}

/// One project's ticket settings with every key set: the core policy keys and the
/// engine's own `manual_evidence` and `ci_evidence`. `Default` is the proposed default of
/// each key, with `run_writers` at the value the host locks it to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketSettings {
    pub policy: Policy,
    pub manual_evidence: ManualEvidence,
    pub ci_evidence: CiEvidence,
}

impl Default for TicketSettings {
    fn default() -> Self {
        Self {
            policy: Policy {
                run_writers: RunWriters::EngineOnly,
                ..Policy::default()
            },
            manual_evidence: ManualEvidence::default(),
            ci_evidence: CiEvidence::default(),
        }
    }
}

/// The keys one scope sets: the app values or one project's override. A key left None
/// takes the value of the scope below (project, then app, then the proposed default).
/// `run_writers` is no key of a scope: the host locks it to `engine_only`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TicketOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close: Option<ClosePolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create: Option<CreatePolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_edit: Option<AgentEditPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycle_days: Option<CycleDays>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rollover: Option<RolloverPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import: Option<ImportPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_evidence: Option<TestEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_counts: Option<SkippedCounts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub case_protected_visibility: Option<CaseProtectedVisibility>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_retention: Option<RunRetention>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual_evidence: Option<ManualEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_evidence: Option<CiEvidence>,
}

impl TicketOverride {
    /// Lays these values over the settings below them, keeping the locked `run_writers`
    /// from below. Input: the settings below. Output: the settings with every key this
    /// scope sets replaced.
    pub fn over(&self, below: &TicketSettings) -> TicketSettings {
        TicketSettings {
            policy: Policy {
                close: self.close.unwrap_or(below.policy.close),
                create: self.create.unwrap_or(below.policy.create),
                agent_edit: self.agent_edit.unwrap_or(below.policy.agent_edit),
                cycle_days: self.cycle_days.unwrap_or(below.policy.cycle_days),
                rollover: self.rollover.unwrap_or(below.policy.rollover),
                import: self.import.unwrap_or(below.policy.import),
                test_evidence: self.test_evidence.unwrap_or(below.policy.test_evidence),
                skipped_counts: self.skipped_counts.unwrap_or(below.policy.skipped_counts),
                case_protected_visibility: self
                    .case_protected_visibility
                    .unwrap_or(below.policy.case_protected_visibility),
                run_writers: below.policy.run_writers,
                run_retention: self.run_retention.unwrap_or(below.policy.run_retention),
            },
            manual_evidence: self.manual_evidence.unwrap_or(below.manual_evidence),
            ci_evidence: self.ci_evidence.unwrap_or(below.ci_evidence),
        }
    }
}

/// Where ticket values are set: the app values every project starts from, or one
/// project's override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    App,
    Project(ProjectId),
}

impl Scope {
    fn stored(&self) -> String {
        match self {
            Scope::App => "app".to_owned(),
            Scope::Project(project) => format!("project:{}", project.as_str()),
        }
    }

    fn item(&self) -> Result<Item, EngineError> {
        Item::new(&format!("tickets:{}", self.stored()))
    }
}

/// The values one scope stores and the version the owner's screen sends back to change
/// them (0 before the first write).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub values: TicketOverride,
    pub version: u64,
}

/// Reads what one scope stores. Inputs: the transaction and the scope. Output: the values
/// and their version, or `tracker_unavailable`.
pub fn stored(tx: &Transaction<'_>, scope: &Scope) -> Result<Stored, EngineError> {
    let body: Option<String> = tx
        .query_row(STORED, [scope.stored()], |row| row.get(0))
        .optional()
        .map_err(unavailable)?;
    let values = match body {
        Some(body) => serde_json::from_str(&body).map_err(unavailable)?,
        None => TicketOverride::default(),
    };
    let version = versioned::current(tx, &scope.item()?)?.version;
    Ok(Stored { values, version })
}

/// The settings a project runs with: its override, else the app values, else the proposed
/// default, key by key, with `run_writers` always `engine_only` (the host's lock). Inputs:
/// the transaction and the project. Output: the settings.
pub fn effective(tx: &Transaction<'_>, project: &ProjectId) -> Result<TicketSettings, EngineError> {
    let app = stored(tx, &Scope::App)?
        .values
        .over(&TicketSettings::default());
    let settings = stored(tx, &Scope::Project(project.clone()))?
        .values
        .over(&app);
    assert_eq!(
        super::host::locked(PolicyKey::RunWriters),
        Some(settings.policy.run_writers.as_str())
    );
    Ok(settings)
}

/// Saves the owner's values for one scope when the owner saw the current version, then
/// writes the effective core keys of every project the change reaches into the core. An
/// empty override is "Use app default" for every key. Inputs: the transaction, the scope,
/// the values, the version the owner's screen showed and the time. Output: the new
/// version, the stale refusal (nothing written), `project_not_attached`, or the core's
/// refusal of a policy write.
pub fn set(
    tx: &Transaction<'_>,
    scope: &Scope,
    values: &TicketOverride,
    expected: u64,
    now_ms: i64,
) -> Result<Result<u64, Stale>, EngineError> {
    if let Scope::Project(project) = scope
        && key_of(tx, project)?.is_none()
    {
        return Err(EngineError(super::host::PROJECT_NOT_ATTACHED.into()));
    }
    let version = match versioned::owner_write(tx, &scope.item()?, expected)? {
        Ok(version) => version,
        Err(stale) => return Ok(Err(stale)),
    };
    let body = serde_json::to_string(values).map_err(unavailable)?;
    let written = tx
        .execute(STORE, params![scope.stored(), body])
        .map_err(unavailable)?;
    assert_eq!(written, 1);
    match scope {
        Scope::App => sync_attached(tx, now_ms)?,
        Scope::Project(project) => sync(tx, project, now_ms)?,
    }
    assert_eq!(stored(tx, scope)?.version, version);
    Ok(Ok(version))
}

/// The host `sync` writes the core's policy keys through. The engine host refuses the
/// core's `policy_set` for every caller, so the ticket settings are the one writer of
/// `tk_policy`; only this module, writing the settings the owner saved, passes it. It
/// records no evidence, opens no file, holds the engine host's policy locks and answers
/// links and evidence as the engine host does.
struct SettingsWriter;

impl Links for SettingsWriter {
    fn kinds(&self) -> &[LinkKind] {
        &super::host::KINDS
    }

    fn resolve(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        kind: &str,
        target: &str,
    ) -> Result<LinkState, Refusal> {
        super::Host.resolve(tx, project, kind, target)
    }
}

impl Evidence for SettingsWriter {
    fn check(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        check: &CheckView,
    ) -> Result<EvidenceState, Refusal> {
        super::Host.check(tx, project, check)
    }

    fn tracks_source(&self) -> bool {
        super::Host.tracks_source()
    }

    fn current_source(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        ticket: &TicketView,
    ) -> Result<Option<SourceRef>, Refusal> {
        super::Host.current_source(tx, project, ticket)
    }
}

impl Paths for SettingsWriter {
    /// Refuses every file: the settings writer only writes policy keys and opens none.
    /// Inputs: the transaction, key, actor and path (unused) and the operation. Output:
    /// `export_path_refused` for an export, `import_path_refused` otherwise.
    fn resolve_path(
        &self,
        _: &Transaction<'_>,
        _: &ProjectKey,
        _: &Actor,
        op: OpKind,
        _: &Path,
    ) -> Result<PathBuf, Refusal> {
        let code = match op {
            OpKind::Export => codes::EXPORT_PATH_REFUSED,
            _ => codes::IMPORT_PATH_REFUSED,
        };
        Err(Refusal::new(
            code,
            "the ticket settings writer opens no file",
        ))
    }
}

impl Authority for SettingsWriter {
    fn actor_may(
        &self,
        _: &Transaction<'_>,
        _: &ProjectKey,
        _: &Actor,
        _: OpKind,
        _: Option<&TicketView>,
    ) -> Result<(), Refusal> {
        Ok(())
    }

    fn actors_verified(&self) -> bool {
        true
    }

    fn evidence_may(
        &self,
        _: &Transaction<'_>,
        _: &ProjectKey,
        _: &Actor,
        _: &CheckView,
    ) -> Result<(), Refusal> {
        Err(Refusal::host(
            super::host::HOST_FAILED,
            "the ticket settings writer records no evidence",
        ))
    }

    fn policy_locked(&self, setting: PolicyKey) -> Option<&'static str> {
        super::host::locked(setting)
    }

    fn run_sight(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        actor: &Actor,
        run: &TestId,
    ) -> Result<Sight, Refusal> {
        super::Host.run_sight(tx, project, actor, run)
    }
}

/// Writes the effective core policy keys of every attached project into the core, as
/// `sync` does for one; a project already current writes nothing. Inputs: the transaction
/// and the time. Output: Ok once every attached project is current.
pub(crate) fn sync_attached(tx: &Transaction<'_>, now_ms: i64) -> Result<(), EngineError> {
    let projects = tx
        .prepare(ATTACHED)
        .and_then(|mut statement| {
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(unavailable)?;
    for project in projects {
        let project = ProjectId::new(project).map_err(unavailable)?;
        sync(tx, &project, now_ms)?;
    }
    Ok(())
}

/// Writes a project's effective core policy keys into the core's `tk_policy` as the owner,
/// one key at a time and only the keys that differ, so the core's own decisions (the
/// evidence close, `next`) read the values the engine enforces. Inputs: the transaction,
/// an attached project and the time. Output: Ok once the core holds the effective policy.
pub(crate) fn sync(
    tx: &Transaction<'_>,
    project: &ProjectId,
    now_ms: i64,
) -> Result<(), EngineError> {
    let key = key_of(tx, project)?
        .ok_or_else(|| EngineError(super::host::PROJECT_NOT_ATTACHED.into()))?;
    let wanted = effective(tx, project)?.policy;
    let current = policy::load(tx, &key).map_err(engine_error)?;
    if current == wanted {
        return Ok(());
    }
    let wanted_values = serde_json::to_value(wanted).map_err(unavailable)?;
    let current_values = serde_json::to_value(current).map_err(unavailable)?;
    for entry in policy::describe(tx, &key).map_err(engine_error)? {
        let name = entry.key.as_str();
        assert!(wanted_values.get(name).is_some());
        if wanted_values[name] == current_values[name] {
            continue;
        }
        let word = match &wanted_values[name] {
            serde_json::Value::String(word) => word.clone(),
            number => number.to_string(),
        };
        policy::set(
            tx,
            &SettingsWriter,
            &owner(),
            &key,
            entry.key,
            &word,
            entry.version,
            now_ms,
        )
        .map_err(engine_error)?;
    }
    assert_eq!(policy::load(tx, &key).map_err(engine_error)?, wanted);
    Ok(())
}
