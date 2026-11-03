use crate::events::{self, Recorded};
use crate::host::{Authority, Host, TicketView};
use crate::model::{
    Actor, AgentEditPolicy, ClosePolicy, CreatePolicy, CycleDays, ImportPolicy, OpKind, Policy,
    PolicyKey, ProjectKey, Refusal, RolloverPolicy, ShortId, Ticket, TicketStatus, codes,
};
use crate::ops::{self, Candidate, Cx, Failure};
use crate::store::{self, StoreError, TicketChange};
use crate::tests_model::{
    CaseProtectedVisibility, RunRetention, RunWriters, SkippedCounts, TestEvidence,
};
use rusqlite::{Transaction, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) const POLICY_OF_PROJECT: &str =
    "SELECT key, value_json, version FROM tk_policy WHERE project = ?1 ORDER BY key";
pub(crate) const POLICY_INSERT: &str =
    "INSERT INTO tk_policy(project, key, value_json, version) VALUES (?1, ?2, ?3, 1)";
pub(crate) const POLICY_UPDATE: &str = "UPDATE tk_policy SET value_json = ?3, \
     version = version + 1 WHERE project = ?1 AND key = ?2 AND version = ?4";

/// The identity every host gives its owner.
pub const OWNER: &str = "owner";
/// The prefix of a host's trusted automation, for example `engine:landing`.
pub const ENGINE_PREFIX: &str = "engine:";
/// The most bytes of a policy value word.
pub const POLICY_WORD_BYTES_MAX: usize = 32;
/// The smallest and largest custom `cycle_days`.
pub const CYCLE_DAYS_CUSTOM: CustomRange = CustomRange { min: 1, max: 90 };

const _: () = assert!(PolicyKey::ALL.len() == 11);
const _: () = assert!(CYCLE_DAYS_CUSTOM.min < CYCLE_DAYS_CUSTOM.max);

/// What an actor is to policy: the owner, the host's trusted automation (`engine:<source>`),
/// or an agent, which is every other actor, masters included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Owner,
    Engine,
    Agent,
}

/// The bounds of a setting's custom value. Fields: the smallest and largest value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CustomRange {
    pub min: u8,
    pub max: u8,
}

/// One policy setting as `policy_get` shows it. Fields: the key, the value in force, the
/// proposed default, every option offered, the custom range when one is allowed, and the
/// stored version (0 while the project keeps the default).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Setting {
    pub key: PolicyKey,
    pub value: Value,
    pub default: Value,
    pub options: Vec<Value>,
    pub custom: Option<CustomRange>,
    pub version: i64,
}

/// Tells what an actor is to policy. Input: a caller's actor, never the core's own `host:`
/// actor. Output: the role.
pub fn role(actor: &Actor) -> Role {
    assert!(
        !actor.is_host(),
        "the core's own writes are never authorized"
    );
    let found = if actor.as_str() == OWNER {
        Role::Owner
    } else if actor.as_str().starts_with(ENGINE_PREFIX) {
        Role::Engine
    } else {
        Role::Agent
    };
    assert!(found != Role::Owner || actor.as_str() == OWNER);
    found
}

/// Reads a project's policy, the proposed default for every key it never set. Inputs: the
/// transaction and the project key. Output: the policy, or `project_unknown`.
pub fn load(tx: &Transaction<'_>, key: &ProjectKey) -> Result<Policy, StoreError> {
    let project = store::project(tx, key)?;
    Ok(stored(tx, project.id)?.0)
}

/// Every setting of a project's policy with its options, for `policy_get`. Inputs: the
/// transaction and the project key. Output: one setting per key, in key order.
pub fn describe(tx: &Transaction<'_>, key: &ProjectKey) -> Result<Vec<Setting>, StoreError> {
    let project = store::project(tx, key)?;
    let (policy, versions) = stored(tx, project.id)?;
    let current = as_object(&policy);
    let default = as_object(&Policy::default());
    let settings: Vec<Setting> = PolicyKey::ALL
        .iter()
        .map(|setting| Setting {
            key: *setting,
            value: current[setting.as_str()].clone(),
            default: default[setting.as_str()].clone(),
            options: options(*setting),
            custom: (*setting == PolicyKey::CycleDays).then_some(CYCLE_DAYS_CUSTOM),
            version: versions.get(setting).copied().unwrap_or(0),
        })
        .collect();
    assert_eq!(settings.len(), PolicyKey::ALL.len());
    assert!(settings.iter().all(|entry| !entry.value.is_null()));
    Ok(settings)
}

/// Sets one policy key at the version the caller read, and records it. Only the owner sets
/// policy. Setting the value a key already stores writes nothing; a key never set is stored
/// even at its default, at version 1. Inputs: the transaction, the host's authority, the
/// actor, the project key, the setting, its new value as a word (an option's name, or a
/// number of days for `cycle_days`), the version (0 for a key never set) and the time.
/// A key the host locks takes only its locked value, and `test_evidence = current_source`
/// needs a host that tracks each ticket's source. Output: the policy now in force, or
/// `owner_only`, `policy_locked`, `option_needs_host`, `policy_value_invalid` or
/// `stale_version`.
#[allow(clippy::too_many_arguments)]
pub fn set(
    tx: &Transaction<'_>,
    host: &impl Host,
    actor: &Actor,
    key: &ProjectKey,
    setting: PolicyKey,
    value: &str,
    version: i64,
    now_ms: i64,
) -> Result<Policy, StoreError> {
    host.actor_may(tx, key, actor, OpKind::PolicySet, None)
        .map_err(StoreError::Refused)?;
    host_permits(host, setting, value)?;
    store::write(tx, || {
        let project = store::project(tx, key)?;
        let (before, versions) = stored(tx, project.id)?;
        let stored_version = versions.get(&setting).copied().unwrap_or(0);
        store::current(&format!("policy {setting}"), stored_version, version)?;
        let mut object = as_object(&before);
        let old = object[setting.as_str()].clone();
        object[setting.as_str()] = word_value(setting, value).ok_or_else(|| invalid(setting))?;
        let after: Policy = serde_json::from_value(object).map_err(|_| invalid(setting))?;
        let new = as_object(&after)[setting.as_str()].clone();
        if stored_version > 0 && new == old {
            return Ok(before);
        }
        let text = new.to_string();
        let written = if stored_version == 0 {
            store::prepare(tx, POLICY_INSERT)?.execute(params![
                project.id,
                setting.as_str(),
                text
            ])?
        } else {
            store::prepare(tx, POLICY_UPDATE)?.execute(params![
                project.id,
                setting.as_str(),
                text,
                stored_version
            ])?
        };
        assert_eq!(
            written, 1,
            "the policy version read in this write still matches"
        );
        events::record(
            tx,
            Recorded {
                project: project.id,
                ticket: None,
                op: events::POLICY_SET,
                actor,
                at_ms: now_ms,
                before: (stored_version > 0)
                    .then(|| json!({"key": setting, "value": old, "version": stored_version})),
                after: Some(json!({"key": setting, "value": new, "version": stored_version + 1})),
            },
        )?;
        assert_eq!(load(tx, key)?, after);
        Ok(after)
    })
}

/// Decides whether a policy lets an actor run an operation. The owner runs all; reads, test
/// ops, `import_preview` and `export` are open to all, the test store deciding protected
/// cases and run writers. `close` and `reopen` follow `close`, `create` follows `create`,
/// `policy_set` is the owner's; engines make other writes; agents follow `agent_edit` on
/// tickets they did not create and may run planning writes only under `agent_edit = any`.
/// Inputs: the policy, the actor, the operation and its ticket. Output: Ok, or the refusal.
pub fn permits(
    policy: &Policy,
    actor: &Actor,
    op: OpKind,
    ticket: Option<&TicketView>,
) -> Result<(), Refusal> {
    if actor.is_host() {
        return Err(Refusal::new(
            codes::ACTOR_INVALID,
            "the host: prefix is reserved for the tracker's own writes",
        ));
    }
    let role = role(actor);
    if role == Role::Owner {
        return Ok(());
    }
    match op {
        OpKind::Ready
        | OpKind::List
        | OpKind::Show
        | OpKind::History
        | OpKind::Roadmap
        | OpKind::PolicyGet
        | OpKind::Export
        | OpKind::Schema
        | OpKind::ImportPreview => Ok(()),
        OpKind::PolicySet => Err(Refusal::new(
            codes::OWNER_ONLY,
            "only the owner sets policy",
        )),
        OpKind::Close | OpKind::Reopen => close_permits(policy.close, op),
        OpKind::Create => create_permits(policy.create, role),
        OpKind::VersionUpsert | OpKind::CycleStart | OpKind::CycleClose | OpKind::ImportApply => {
            planning_permits(policy.agent_edit, role, op)
        }
        OpKind::Edit
        | OpKind::Depend
        | OpKind::Undepend
        | OpKind::Link
        | OpKind::Unlink
        | OpKind::Move
        | OpKind::Claim
        | OpKind::Release
        | OpKind::CriterionAdd
        | OpKind::CriterionEdit
        | OpKind::CheckAdd
        | OpKind::EvidenceRecord
        | OpKind::CriterionSet => edit_permits(policy.agent_edit, role, actor, op, ticket),
        OpKind::CaseList
        | OpKind::CaseShow
        | OpKind::SetShow
        | OpKind::PlanList
        | OpKind::PlanShow
        | OpKind::RunList
        | OpKind::RunShow
        | OpKind::Coverage
        | OpKind::CaseCreate
        | OpKind::CaseEdit
        | OpKind::CaseLink
        | OpKind::CaseUnlink
        | OpKind::SetUpsert
        | OpKind::SetAdd
        | OpKind::SetRemove
        | OpKind::PlanUpsert
        | OpKind::PlanAdd
        | OpKind::PlanRemove
        | OpKind::RunStart
        | OpKind::RunRecord
        | OpKind::RunFinish
        | OpKind::RunCancel => {
            assert!(op.is_test_read() || op.is_test_write());
            Ok(())
        }
    }
}

/// The status a new ticket starts in: an agent's ticket starts in triage under
/// `create = triage`; every other ticket starts where its creator asked. Inputs: the
/// policy, the creator and the open status asked for. Output: the starting status.
pub fn start_status(policy: &Policy, actor: &Actor, requested: TicketStatus) -> TicketStatus {
    assert!(!requested.is_closed(), "a ticket starts open");
    let status = if policy.create == CreatePolicy::Triage && role(actor) == Role::Agent {
        TicketStatus::Triage
    } else {
        requested
    };
    assert!(!status.is_closed());
    status
}

/// Closes an open ticket by hand as done or cancelled, as the host's authority and the
/// `close` policy allow. Inputs: the transaction, the host's authority, the actor, the
/// ticket, the version read, the closed status, the reason and the time. Output: the closed
/// ticket, or `status_invalid`, `ticket_closed`, `stale_version` or the policy's refusal.
#[allow(clippy::too_many_arguments)]
pub fn close(
    tx: &Transaction<'_>,
    host: &impl Authority,
    actor: &Actor,
    id: &ShortId,
    version: i64,
    status: TicketStatus,
    reason: Option<&str>,
    now_ms: i64,
) -> Result<Ticket, StoreError> {
    if !status.is_closed() {
        return Err(store::refusal(
            codes::STATUS_INVALID,
            "a ticket closes as done or cancelled",
        ));
    }
    let ticket = store::ticket(tx, id)?;
    authorize(tx, host, actor, OpKind::Close, &ticket)?;
    if ticket.status.is_closed() {
        return Err(store::refusal(
            codes::TICKET_CLOSED,
            format!("{id} is already closed"),
        ));
    }
    let change = TicketChange {
        status: Some(status),
        close_reason: Some(reason.map(str::to_owned)),
        ..TicketChange::default()
    };
    let closed = store::update_ticket(tx, actor, id, version, &change, now_ms)?;
    assert_eq!(closed.status, status);
    assert_eq!(closed.closed_by.as_ref(), Some(actor));
    Ok(closed)
}

/// Reopens a closed ticket into an open status, as the host's authority and the `close`
/// policy allow. A reopened ticket is not re-evaluated until its criteria next change.
/// Inputs: the transaction, the host's authority, the actor, the ticket, the version read,
/// the open status and the time. Output: the reopened ticket, or `status_invalid`,
/// `ticket_open`, `stale_version` or the policy's refusal.
pub fn reopen(
    tx: &Transaction<'_>,
    host: &impl Authority,
    actor: &Actor,
    id: &ShortId,
    version: i64,
    status: TicketStatus,
    now_ms: i64,
) -> Result<Ticket, StoreError> {
    if status.is_closed() {
        return Err(store::refusal(
            codes::STATUS_INVALID,
            "a ticket reopens into an open status",
        ));
    }
    let ticket = store::ticket(tx, id)?;
    authorize(tx, host, actor, OpKind::Reopen, &ticket)?;
    if !ticket.status.is_closed() {
        return Err(store::refusal(codes::TICKET_OPEN, format!("{id} is open")));
    }
    let change = TicketChange {
        status: Some(status),
        close_reason: Some(None),
        ..TicketChange::default()
    };
    let reopened = store::update_ticket(tx, actor, id, version, &change, now_ms)?;
    assert_eq!(reopened.status, status);
    assert!(reopened.closed_by.is_none());
    Ok(reopened)
}

/// Asks the host whether an actor may run an operation on a stored ticket. Inputs: the
/// transaction, the host's authority, the actor, the operation and the ticket. Output: Ok,
/// or the host's refusal.
pub(crate) fn authorize(
    tx: &Transaction<'_>,
    host: &impl Authority,
    actor: &Actor,
    op: OpKind,
    ticket: &Ticket,
) -> Result<(), StoreError> {
    assert!(op.is_write(), "reads need no ticket authority");
    host.actor_may(
        tx,
        ticket.short_id.key(),
        actor,
        op,
        Some(&TicketView::of(ticket)),
    )
    .map_err(StoreError::Refused)
}

/// Checks a new value against what the host allows, before anything is written: a key the
/// host locks takes only its locked value, and `test_evidence = current_source` needs a host
/// that tracks each ticket's source. Inputs: the host, the setting and the value word.
/// Output: Ok, or `policy_locked` or `option_needs_host`.
fn host_permits(host: &impl Host, setting: PolicyKey, value: &str) -> Result<(), StoreError> {
    if let Some(locked) = host.policy_locked(setting) {
        assert!(!locked.is_empty(), "a host locks a key to a value word");
        assert!(locked.len() <= POLICY_WORD_BYTES_MAX);
        if value != locked {
            return Err(store::refusal(
                codes::POLICY_LOCKED,
                format!("the host locks {setting} to {locked}"),
            ));
        }
    }
    let current_source = TestEvidence::CurrentSource.as_str();
    if setting == PolicyKey::TestEvidence && value == current_source && !host.tracks_source() {
        return Err(store::refusal(
            codes::OPTION_NEEDS_HOST,
            format!("{setting} {current_source} needs a host that knows each ticket's source"),
        ));
    }
    Ok(())
}

fn close_permits(option: ClosePolicy, op: OpKind) -> Result<(), Refusal> {
    match option {
        ClosePolicy::Agents => Ok(()),
        ClosePolicy::Evidence => Err(Refusal::new(
            codes::CLOSE_NEEDS_EVIDENCE,
            format!("policy close is evidence: only the owner may {op} by hand"),
        )),
        ClosePolicy::Owner => Err(Refusal::new(
            codes::CLOSE_OWNER_ONLY,
            format!("policy close is owner: only the owner may {op}"),
        )),
    }
}

fn create_permits(option: CreatePolicy, role: Role) -> Result<(), Refusal> {
    assert!(role != Role::Owner);
    if role == Role::Agent && option == CreatePolicy::OwnerOnly {
        return Err(Refusal::new(
            codes::CREATE_OWNER_ONLY,
            "policy create is owner_only: only the owner creates tickets",
        ));
    }
    Ok(())
}

fn planning_permits(option: AgentEditPolicy, role: Role, op: OpKind) -> Result<(), Refusal> {
    assert!(role != Role::Owner);
    if role == Role::Engine || option == AgentEditPolicy::Any {
        return Ok(());
    }
    Err(Refusal::new(
        codes::AGENT_EDIT_REFUSED,
        format!("policy agent_edit is {option}: an agent may not run {op}"),
    ))
}

fn edit_permits(
    option: AgentEditPolicy,
    role: Role,
    actor: &Actor,
    op: OpKind,
    ticket: Option<&TicketView>,
) -> Result<(), Refusal> {
    assert!(role != Role::Owner);
    if role == Role::Engine || option == AgentEditPolicy::Any {
        return Ok(());
    }
    if ticket.is_some_and(|view| view.created_by == *actor) {
        return Ok(());
    }
    let progress = matches!(
        op,
        OpKind::Move
            | OpKind::Claim
            | OpKind::Release
            | OpKind::CriterionAdd
            | OpKind::CriterionEdit
            | OpKind::CheckAdd
            | OpKind::EvidenceRecord
            | OpKind::CriterionSet
    );
    if progress && option == AgentEditPolicy::StatusAndCriteria {
        return Ok(());
    }
    Err(Refusal::new(
        codes::AGENT_EDIT_REFUSED,
        format!("policy agent_edit is {option}: an agent may not {op} others' tickets"),
    ))
}

fn stored(
    tx: &Transaction<'_>,
    project: i64,
) -> Result<(Policy, BTreeMap<PolicyKey, i64>), StoreError> {
    let rows = store::prepare(tx, POLICY_OF_PROJECT)?
        .query_map([project], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert!(
        rows.len() <= PolicyKey::ALL.len(),
        "one row per key at most"
    );
    let mut object = as_object(&Policy::default());
    let mut versions = BTreeMap::new();
    for (name, text, version) in rows {
        let setting = PolicyKey::parse(&name)
            .ok_or_else(|| StoreError::Corrupt(format!("a stored policy key {name} is unknown")))?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|_| StoreError::Corrupt(format!("the stored policy {setting} is not JSON")))?;
        object[setting.as_str()] = value;
        assert!(version >= 1);
        versions.insert(setting, version);
    }
    let policy: Policy = serde_json::from_value(object).map_err(|_| {
        StoreError::Corrupt("a stored policy value is not one of its options".to_owned())
    })?;
    Ok((policy, versions))
}

fn word_value(setting: PolicyKey, word: &str) -> Option<Value> {
    if word.is_empty() || word.len() > POLICY_WORD_BYTES_MAX {
        return None;
    }
    if setting == PolicyKey::CycleDays {
        if !word.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        return word.parse::<u8>().ok().map(|days| json!(days));
    }
    Some(json!(word))
}

fn as_object(policy: &Policy) -> Value {
    let object = json!(policy);
    assert!(object.is_object());
    assert!(
        PolicyKey::ALL
            .iter()
            .all(|setting| object.get(setting.as_str()).is_some()),
        "every policy key is a field of Policy"
    );
    object
}

fn options(setting: PolicyKey) -> Vec<Value> {
    fn names<T: Copy + Serialize>(all: &[T]) -> Vec<Value> {
        all.iter().map(|option| json!(option)).collect()
    }
    let options = match setting {
        PolicyKey::Close => names(ClosePolicy::ALL),
        PolicyKey::Create => names(CreatePolicy::ALL),
        PolicyKey::AgentEdit => names(AgentEditPolicy::ALL),
        PolicyKey::CycleDays => CycleDays::PRESETS.iter().map(|days| json!(days)).collect(),
        PolicyKey::Rollover => names(RolloverPolicy::ALL),
        PolicyKey::Import => names(ImportPolicy::ALL),
        PolicyKey::TestEvidence => names(TestEvidence::ALL),
        PolicyKey::SkippedCounts => names(SkippedCounts::ALL),
        PolicyKey::CaseProtectedVisibility => names(CaseProtectedVisibility::ALL),
        PolicyKey::RunWriters => names(RunWriters::ALL),
        PolicyKey::RunRetention => names(RunRetention::ALL),
    };
    assert!(options.len() >= 2, "every setting offers a choice");
    options
}

fn invalid(setting: PolicyKey) -> StoreError {
    let offered: Vec<String> = options(setting).iter().map(Value::to_string).collect();
    let custom = if setting == PolicyKey::CycleDays {
        format!(
            ", or {} to {}",
            CYCLE_DAYS_CUSTOM.min, CYCLE_DAYS_CUSTOM.max
        )
    } else {
        String::new()
    };
    store::refusal(
        codes::POLICY_VALUE_INVALID,
        format!("{setting} takes one of {}{custom}", offered.join(", ")),
    )
}

/// One policy setting as a reply shows it. Input: the setting. Output: its key, value,
/// proposed default, options, custom range (null when none) and stored version.
fn setting_json(setting: &Setting) -> Value {
    assert!(setting.options.len() >= 2);
    json!({
        "key": setting.key.as_str(),
        "value": setting.value,
        "default": setting.default,
        "options": setting.options,
        "custom": setting.custom.map(|range| json!({"min": range.min, "max": range.max})),
        "version": setting.version,
    })
}

/// Runs `policy_get` for dispatch. Input: the context. Output: the project and every
/// setting of its policy, in key order.
pub(crate) fn policy_get<H: Host>(cx: &Cx<'_, '_, H>) -> Result<ops::Outcome, Failure> {
    let settings = describe(cx.tx, cx.project())?;
    assert_eq!(settings.len(), PolicyKey::ALL.len());
    let shown: Vec<Value> = settings.iter().map(setting_json).collect();
    Ok(ops::Outcome {
        data: json!({"project": cx.project().as_str(), "settings": shown}),
        next: Vec::new(),
        cursor: None,
    })
}

/// Runs `policy_set` for dispatch; a key never set is at version 0, so `version` may be
/// left out until it was set once. Input: the context. Output: the setting as stored.
pub(crate) fn policy_set<H: Host>(cx: &Cx<'_, '_, H>) -> Result<ops::Outcome, Failure> {
    let setting = cx.args.need::<PolicyKey>("key");
    let value = cx.args.need::<&str>("value");
    let version = cx.args.get::<i64>("version").unwrap_or(0);
    let read = describe(cx.tx, cx.project())?
        .into_iter()
        .find(|entry| entry.key == setting)
        .expect("describe lists every policy key");
    if read.version != version {
        return Err(Failure::new(
            codes::STALE_VERSION,
            format!(
                "policy {setting} is at version {}, not {version}; read the policy and retry",
                read.version
            ),
        )
        .with_data(json!({"key": setting.as_str(), "version": read.version}))
        .with_next(vec![Candidate::project(
            OpKind::PolicyGet,
            cx.project(),
            "read the policy's current versions, then retry",
        )]));
    }
    set(
        cx.tx,
        cx.host,
        cx.actor,
        cx.project(),
        setting,
        value,
        version,
        cx.at_ms,
    )?;
    let stored = describe(cx.tx, cx.project())?
        .into_iter()
        .find(|entry| entry.key == setting)
        .expect("describe lists every policy key");
    assert!(stored.version >= version);
    Ok(ops::Outcome {
        data: json!({"setting": setting_json(&stored)}),
        next: Vec::new(),
        cursor: None,
    })
}

/// Runs `close` for dispatch: done when no status is named. A closed ticket proposes
/// `reopen`, and a version other than the one read proposes `show`, as other ticket writes
/// do. Input: the context. Output: the ticket as closed.
pub(crate) fn close_ticket<H: Host>(cx: &Cx<'_, '_, H>) -> Result<ops::Outcome, Failure> {
    let ticket = cx.ticket();
    if ticket.status.is_closed() {
        return Err(Failure::new(
            codes::TICKET_CLOSED,
            format!("{} is already closed", ticket.short_id),
        )
        .with_next(vec![Candidate::ticket(
            OpKind::Reopen,
            ticket,
            "reopen it to work on it again",
        )]));
    }
    let version = ops::fresh(cx, ticket)?;
    let status = cx
        .args
        .get::<TicketStatus>("status")
        .unwrap_or(TicketStatus::Done);
    let closed = close(
        cx.tx,
        cx.host,
        cx.actor,
        &ticket.short_id,
        version,
        status,
        cx.args.get::<&str>("reason"),
        cx.at_ms,
    )?;
    assert_eq!(closed.status, status);
    assert!(closed.status.is_closed());
    ops::written(cx, &closed)
}

/// Runs `reopen` for dispatch: todo when no status is named. An open ticket proposes
/// `close`, and a version other than the one read proposes `show`. Input: the context.
/// Output: the ticket as reopened.
pub(crate) fn reopen_ticket<H: Host>(cx: &Cx<'_, '_, H>) -> Result<ops::Outcome, Failure> {
    let ticket = cx.ticket();
    if !ticket.status.is_closed() {
        return Err(
            Failure::new(codes::TICKET_OPEN, format!("{} is open", ticket.short_id)).with_next(
                vec![Candidate::ticket(
                    OpKind::Close,
                    ticket,
                    "close it as done or cancelled",
                )],
            ),
        );
    }
    let version = ops::fresh(cx, ticket)?;
    let status = cx
        .args
        .get::<TicketStatus>("status")
        .unwrap_or(TicketStatus::Todo);
    let reopened = reopen(
        cx.tx,
        cx.host,
        cx.actor,
        &ticket.short_id,
        version,
        status,
        cx.at_ms,
    )?;
    assert_eq!(reopened.status, status);
    assert!(!reopened.status.is_closed());
    ops::written(cx, &reopened)
}
