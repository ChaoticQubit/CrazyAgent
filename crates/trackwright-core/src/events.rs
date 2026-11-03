use crate::model::{Actor, Check, Criterion, Event, Evidence, Link, ProjectKey, ShortId, Ticket};
use crate::runs;
use crate::store::{self, State, StoreError};
use crate::tests_store;
use rusqlite::{Row, Transaction, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;

pub const PROJECT_CREATE: &str = "project.create";
pub const TICKET_CREATE: &str = "ticket.create";
pub const TICKET_UPDATE: &str = "ticket.update";
pub const CRITERION_ADD: &str = "criterion.add";
pub const CRITERION_SET: &str = "criterion.set";
pub const LINK_ADD: &str = "link.add";
pub const LINK_REMOVE: &str = "link.remove";
pub const CRITERION_EDIT: &str = "criterion.edit";
pub const CHECK_ADD: &str = "check.add";
pub const EVIDENCE_RECORD: &str = "evidence.record";
pub const POLICY_SET: &str = "policy.set";
pub const VERSION_UPSERT: &str = "version.upsert";
pub const CYCLE_UPSERT: &str = "cycle.upsert";
pub const DEP_ADD: &str = "dep.add";
pub const DEP_REMOVE: &str = "dep.remove";

/// Every event op the core writes and replays.
pub const OPS: &[&str] = &[
    PROJECT_CREATE,
    TICKET_CREATE,
    TICKET_UPDATE,
    CRITERION_ADD,
    CRITERION_SET,
    LINK_ADD,
    LINK_REMOVE,
    CRITERION_EDIT,
    CHECK_ADD,
    EVIDENCE_RECORD,
    POLICY_SET,
    VERSION_UPSERT,
    CYCLE_UPSERT,
    DEP_ADD,
    DEP_REMOVE,
];

pub(crate) const EVENT_INSERT: &str = "INSERT INTO tk_events(ticket, project, at_ms, actor, op, \
     before_json, after_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
pub(crate) const EVENTS_OF_PROJECT: &str = "SELECT id, ticket, project, at_ms, actor, op, \
     before_json, after_json FROM tk_events WHERE project = ?1 ORDER BY id";
pub(crate) const EVENTS_OF_TICKET: &str = "SELECT id, ticket, project, at_ms, actor, op, \
     before_json, after_json FROM tk_events WHERE ticket = ?1 ORDER BY id";

/// One state change to record. Fields: the project and ticket it belongs to, the op, who
/// made it and when, and the changed row before and after as JSON.
pub(crate) struct Recorded<'a> {
    pub project: i64,
    pub ticket: Option<i64>,
    pub op: &'static str,
    pub actor: &'a Actor,
    pub at_ms: i64,
    pub before: Option<serde_json::Value>,
    pub after: Option<serde_json::Value>,
}

/// Appends one event in the caller's transaction, so the change and its event commit or
/// roll back together. Inputs: the transaction and the event. Output: Ok, or the SQLite
/// error that aborts the write.
pub(crate) fn record(tx: &Transaction<'_>, event: Recorded<'_>) -> Result<(), StoreError> {
    assert!(
        OPS.contains(&event.op) || tests_store::OPS.contains(&event.op),
        "every recorded op is listed"
    );
    assert!(event.before.is_some() || event.after.is_some());
    store::prepare(tx, EVENT_INSERT)?.execute(params![
        event.ticket,
        event.project,
        event.at_ms,
        event.actor.as_str(),
        event.op,
        event.before.as_ref().map(serde_json::Value::to_string),
        event.after.as_ref().map(serde_json::Value::to_string),
    ])?;
    Ok(())
}

/// The full JSON row an event stores. Input: a core row. Output: its JSON.
pub(crate) fn snapshot<T: Serialize>(row: &T) -> serde_json::Value {
    match serde_json::to_value(row) {
        Ok(value) => value,
        Err(error) => panic!("a core row always serializes: {error}"),
    }
}

/// One ticket's events, oldest first, its criteria and links included. Inputs: the
/// transaction and the ticket. Output: the events, or `ticket_unknown`.
pub fn history(tx: &Transaction<'_>, id: &ShortId) -> Result<Vec<Event>, StoreError> {
    let ticket = store::ticket(tx, id)?;
    read(tx, EVENTS_OF_TICKET, ticket.id)
}

/// Rebuilds a project's tickets, criteria, links, checks and evidence from its events alone,
/// in the order `store::state` reads the tables. Inputs: the transaction and the key.
/// Output: the state, or `project_unknown`, or `Corrupt` for an event this core cannot apply.
pub fn replay(tx: &Transaction<'_>, key: &ProjectKey) -> Result<State, StoreError> {
    let project = store::project(tx, key)?;
    let mut tickets: BTreeMap<i64, Ticket> = BTreeMap::new();
    let mut criteria: BTreeMap<(i64, u32), Criterion> = BTreeMap::new();
    let mut links: BTreeMap<(i64, String, String), Link> = BTreeMap::new();
    let mut checks: BTreeMap<i64, Check> = BTreeMap::new();
    let mut evidence: BTreeMap<i64, Evidence> = BTreeMap::new();
    for event in read(tx, EVENTS_OF_PROJECT, project.id)? {
        match event.op.as_str() {
            PROJECT_CREATE | POLICY_SET | VERSION_UPSERT | CYCLE_UPSERT | DEP_ADD | DEP_REMOVE => {}
            TICKET_CREATE | TICKET_UPDATE => {
                let ticket: Ticket = row(event.after)?;
                tickets.insert(ticket.id, ticket);
            }
            CRITERION_ADD | CRITERION_SET | CRITERION_EDIT => {
                let criterion: Criterion = row(event.after)?;
                criteria.insert((criterion.ticket, criterion.position), criterion);
            }
            LINK_ADD => {
                let link: Link = row(event.after)?;
                links.insert((link.ticket, link.kind.clone(), link.target.clone()), link);
            }
            CHECK_ADD => {
                let check: Check = row(event.after)?;
                checks.insert(check.id, check);
            }
            EVIDENCE_RECORD => {
                let recorded: Evidence = row(event.after)?;
                evidence.insert(recorded.id, recorded);
            }
            LINK_REMOVE => {
                let link: Link = row(event.before)?;
                links
                    .remove(&(link.ticket, link.kind, link.target))
                    .ok_or_else(|| {
                        StoreError::Corrupt("an event removes a link no event added".into())
                    })?;
            }
            op if tests_store::OPS.contains(&op) || runs::OPS.contains(&op) => {}
            _ => {
                return Err(StoreError::Corrupt(
                    "an event names an op this core does not know".into(),
                ));
            }
        }
    }
    let mut tickets: Vec<Ticket> = tickets.into_values().collect();
    tickets.sort_by_key(|ticket| ticket.short_id.number());
    Ok(State {
        tickets,
        criteria: criteria.into_values().collect(),
        links: links.into_values().collect(),
        checks: checks.into_values().collect(),
        evidence: evidence.into_values().collect(),
    })
}

fn read(tx: &Transaction<'_>, sql: &'static str, owner: i64) -> Result<Vec<Event>, StoreError> {
    Ok(store::prepare(tx, sql)?
        .query_map([owner], event_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn event_row(row: &Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        id: row.get(0)?,
        ticket: row.get(1)?,
        project: row.get(2)?,
        at_ms: row.get(3)?,
        actor: store::actor(row, 4)?,
        op: row.get(5)?,
        before: json(row, 6)?,
        after: json(row, 7)?,
    })
}

fn json(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<serde_json::Value>> {
    row.get::<_, Option<String>>(index)?
        .map(|text| serde_json::from_str(&text))
        .transpose()
        .map_err(|error| store::conversion(index, error))
}

fn row<T: DeserializeOwned>(value: Option<serde_json::Value>) -> Result<T, StoreError> {
    let value =
        value.ok_or_else(|| StoreError::Corrupt("an event lacks the row its op needs".into()))?;
    serde_json::from_value(value)
        .map_err(|_| StoreError::Corrupt("an event row does not match its op".into()))
}
