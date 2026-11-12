use crate::events::{self, Recorded};
use crate::model::{
    Actor, CRITERIA_PER_TICKET_MAX, Check, Criterion, CriterionState, Evidence,
    LINKS_PER_TICKET_MAX, Link, Priority, Project, ProjectKey, Refusal, ShortId,
    TICKETS_PER_PROJECT_MAX, Ticket, TicketKind, TicketStatus, codes,
};
use crate::ops;
use crate::requests;
use crate::schema;
use crate::text::{self, TextField};
use rusqlite::types::Type;
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, Statement, Transaction, TransactionBehavior,
    params,
};
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

macro_rules! ticket_select {
    () => {
        "SELECT t.id, t.project, p.key, t.number, t.title, t.body, t.kind, t.status, \
         t.priority, t.version_id, t.cycle_id, t.parent_id, t.assignee, t.created_by, \
         t.created_ms, t.updated_ms, t.closed_ms, t.closed_by, t.close_reason, t.version \
         FROM tk_tickets t JOIN tk_projects p ON p.id = t.project"
    };
}

macro_rules! criterion_select {
    () => {
        "SELECT c.id, c.ticket, c.position, c.text, c.state, c.version FROM tk_criteria c"
    };
}

macro_rules! link_select {
    () => {
        "SELECT l.ticket, l.kind, l.target, l.created_by, l.created_ms FROM tk_links l"
    };
}

const PROJECT_BY_KEY: &str =
    "SELECT id, key, name, created_ms, version FROM tk_projects WHERE key = ?1";
const PROJECT_INSERT: &str =
    "INSERT INTO tk_projects(key, name, created_ms, version) VALUES (?1, ?2, ?3, 1)";
const TICKET_BY_ID: &str = concat!(ticket_select!(), " WHERE t.id = ?1");
const TICKET_BY_NUMBER: &str = concat!(ticket_select!(), " WHERE p.key = ?1 AND t.number = ?2");
const TICKETS_OF_PROJECT: &str =
    concat!(ticket_select!(), " WHERE t.project = ?1 ORDER BY t.number");
const TICKET_COUNT: &str =
    "SELECT count(*), coalesce(max(number), 0) FROM tk_tickets WHERE project = ?1";
const TICKET_INSERT: &str = "INSERT INTO tk_tickets(project, number, title, body, kind, status, \
     priority, created_by, created_ms, updated_ms, version, parent_id, version_id, cycle_id) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, 1, ?10, ?11, ?12)";
const TICKET_UPDATE: &str = "UPDATE tk_tickets SET title = ?3, body = ?4, kind = ?5, status = ?6, \
     priority = ?7, assignee = ?8, updated_ms = ?9, closed_ms = ?10, closed_by = ?11, \
     close_reason = ?12, parent_id = ?13, version_id = ?14, cycle_id = ?15, \
     version = version + 1 WHERE id = ?1 AND version = ?2";
const CRITERION_BY_ID: &str = concat!(criterion_select!(), " WHERE c.id = ?1");
pub(crate) const CRITERION_BY_POSITION: &str = concat!(
    criterion_select!(),
    " WHERE c.ticket = ?1 AND c.position = ?2"
);
const CRITERIA_OF_PROJECT: &str = concat!(
    criterion_select!(),
    " JOIN tk_tickets t ON t.id = c.ticket WHERE t.project = ?1 ORDER BY c.ticket, c.position"
);
const CRITERION_COUNT: &str =
    "SELECT count(*), coalesce(max(position), 0) FROM tk_criteria WHERE ticket = ?1";
const CRITERION_INSERT: &str =
    "INSERT INTO tk_criteria(ticket, position, text, state, version) VALUES (?1, ?2, ?3, ?4, 1)";
const CRITERION_STATE_UPDATE: &str =
    "UPDATE tk_criteria SET state = ?3, version = version + 1 WHERE id = ?1 AND version = ?2";
const LINK_GET: &str = concat!(
    link_select!(),
    " WHERE l.ticket = ?1 AND l.kind = ?2 AND l.target = ?3"
);
const LINKS_OF_PROJECT: &str = concat!(
    link_select!(),
    " JOIN tk_tickets t ON t.id = l.ticket WHERE t.project = ?1 \
     ORDER BY l.ticket, l.kind, l.target"
);
const LINK_COUNT: &str = "SELECT count(*) FROM tk_links WHERE ticket = ?1";
const LINK_INSERT: &str = "INSERT INTO tk_links(ticket, kind, target, created_by, created_ms) VALUES (?1, ?2, ?3, ?4, ?5)";
const LINK_DELETE: &str = "DELETE FROM tk_links WHERE ticket = ?1 AND kind = ?2 AND target = ?3";

const STATEMENTS: &[&str] = &[
    schema::VERSION_READ,
    schema::VERSION_WRITE,
    PROJECT_BY_KEY,
    PROJECT_INSERT,
    TICKET_BY_ID,
    TICKET_BY_NUMBER,
    TICKETS_OF_PROJECT,
    TICKET_COUNT,
    TICKET_INSERT,
    TICKET_UPDATE,
    CRITERION_BY_ID,
    CRITERION_BY_POSITION,
    CRITERIA_OF_PROJECT,
    CRITERION_COUNT,
    CRITERION_INSERT,
    CRITERION_STATE_UPDATE,
    LINK_GET,
    LINKS_OF_PROJECT,
    LINK_COUNT,
    LINK_INSERT,
    LINK_DELETE,
    events::EVENT_INSERT,
    events::EVENTS_OF_PROJECT,
    events::EVENTS_OF_TICKET,
    requests::REQUEST_GET,
    requests::REQUEST_INSERT,
    requests::REQUESTS_EXPIRE,
    requests::REQUESTS_COUNT,
    requests::REQUESTS_TRIM,
    ops::WRITES_RECENT,
    ops::OPEN_CREATED,
    ops::CRITERIA_TALLY,
    ops::LIST_PAGE,
    ops::CRITERIA_FROM,
    ops::CHECKS_OF,
    ops::EVIDENCE_LATEST,
    ops::LINKS_FROM,
    ops::EVENTS_RECENT,
    ops::EVENTS_FROM,
    ops::EVENTS_COUNT,
    crate::criteria::CRITERIA_OF_TICKET,
    crate::criteria::CRITERION_TEXT_UPDATE,
    crate::criteria::CHECKS_OF_CRITERION,
    crate::criteria::CHECK_INSERT,
    crate::criteria::CHECKS_OF_PROJECT,
    crate::criteria::EVIDENCE_INSERT,
    crate::criteria::EVIDENCE_LATEST,
    crate::criteria::EVIDENCE_OF_PROJECT,
    crate::criteria::TICKETS_LINKED_OPEN,
    crate::policy::POLICY_OF_PROJECT,
    crate::policy::POLICY_INSERT,
    crate::policy::POLICY_UPDATE,
    crate::export::OPEN_TICKETS_OF_PROJECT,
    crate::export::CYCLES_OF_PROJECT,
    crate::export::LINK_TARGETS_OF_TICKET,
    crate::import::VERSIONS_OF_PROJECT,
    crate::import::VERSION_BY_ID,
    crate::import::VERSION_BY_NAME,
    crate::import::VERSION_INSERT,
    crate::import::VERSION_UPDATE,
    crate::import::TICKET_TITLES_OF_PROJECT,
    crate::import::TICKET_VERSION_SET,
    crate::plan::VERSION_BY_NAME,
    crate::plan::VERSION_TALLY,
    crate::plan::VERSION_INSERT,
    crate::plan::VERSION_UPDATE,
    crate::plan::CYCLE_BY_NUMBER,
    crate::plan::CYCLE_LATEST,
    crate::plan::CYCLE_NEXT_OPEN,
    crate::plan::CYCLE_FIRST_UPCOMING,
    crate::plan::CYCLE_COUNT,
    crate::plan::CYCLE_ACTIVE,
    crate::plan::CYCLE_INSERT,
    crate::plan::CYCLE_STATUS_SET,
    crate::plan::CYCLE_OPEN_TICKETS,
    crate::plan::PARENT_OF,
    crate::plan::DEP_GET,
    crate::plan::DEP_COUNT,
    crate::plan::DEP_REACHES,
    crate::plan::DEP_INSERT,
    crate::plan::DEP_DELETE,
    crate::ready::READY_PAGE,
    crate::ready::VERSIONS_PAGE,
    crate::ready::UNPLANNED_TALLY,
    crate::ready::CYCLES_PAGE,
    crate::ready::PLACEMENT_NAMES,
    crate::ready::VERSION_NAME,
    crate::ready::BLOCKERS_OPEN,
];

/// Every SQL statement the core runs outside its migrations. A test prepares each against a
/// database holding only the core schema, so the core names no host table. Output: the list.
pub fn statements() -> &'static [&'static str] {
    STATEMENTS
}

/// Why a store call failed: a plain refusal to show the caller, a SQLite error, or a stored
/// row or event the core cannot read back.
#[derive(Debug)]
pub enum StoreError {
    Refused(Refusal),
    Sqlite(rusqlite::Error),
    Corrupt(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Refused(refusal) => write!(f, "{refusal}"),
            StoreError::Sqlite(error) => write!(f, "{}: {error}", codes::STORE_FAILED),
            StoreError::Corrupt(detail) => write!(f, "{}: {detail}", codes::STORE_FAILED),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        StoreError::Sqlite(error)
    }
}

impl From<Refusal> for StoreError {
    fn from(refusal: Refusal) -> Self {
        StoreError::Refused(refusal)
    }
}

/// One project's tickets, criteria, links, checks and evidence, each in a fixed order:
/// tickets by number, criteria by ticket and position, links by ticket, kind and target,
/// checks and evidence in the order they were added.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    pub tickets: Vec<Ticket>,
    pub criteria: Vec<Criterion>,
    pub links: Vec<Link>,
    pub checks: Vec<Check>,
    pub evidence: Vec<Evidence>,
}

/// A new ticket's fields. The core sets its number, actor and times.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTicket {
    pub title: String,
    pub body: String,
    pub kind: TicketKind,
    pub status: TicketStatus,
    pub priority: Priority,
}

/// The fields one ticket write changes; None keeps a field. For the optional fields,
/// `Some(None)` clears it. A move to a closed status records who closed it and when; a move
/// back to an open status clears both and the close reason.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TicketChange {
    pub title: Option<String>,
    pub body: Option<String>,
    pub kind: Option<TicketKind>,
    pub status: Option<TicketStatus>,
    pub priority: Option<Priority>,
    pub assignee: Option<Option<Actor>>,
    pub close_reason: Option<Option<String>>,
    pub placement: Placement,
}

/// Where a write puts a ticket in the plan: its parent ticket, product version and cycle,
/// each by internal id. None keeps a field and `Some(None)` clears it. Only the core builds
/// one, after checking every id belongs to the ticket's project, so ids never leave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Placement {
    pub(crate) parent_id: Option<Option<i64>>,
    pub(crate) version_id: Option<Option<i64>>,
    pub(crate) cycle_id: Option<Option<i64>>,
}

impl Placement {
    /// Tells whether the placement changes nothing. Output: true when every field is kept.
    pub fn is_empty(&self) -> bool {
        self.parent_id.is_none() && self.version_id.is_none() && self.cycle_id.is_none()
    }
}

/// The standalone tracker database: one SQLite file private to its user, migrated on open.
#[derive(Debug)]
pub struct Store {
    connection: Connection,
}

impl Store {
    /// Opens or creates the standalone database and migrates it. Creates a missing folder
    /// 0700 and a missing file 0600. Refuses a symlinked folder or file, a file that is not
    /// a plain file, and on Unix a folder or file that other users may open. Input: the file
    /// path. Output: the store, or the refusal.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if path.file_name().is_none() {
            return Err(path_refused("the tracker path names no file"));
        }
        let folder = match path.parent() {
            Some(folder) if !folder.as_os_str().is_empty() => folder,
            _ => Path::new("."),
        };
        private_folder(folder)?;
        private_file(path)?;
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NOFOLLOW
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        connection.pragma_update(None, "foreign_keys", true)?;
        let mut store = Self { connection };
        let tx = store.transaction()?;
        schema::migrate(&tx)?;
        tx.commit()?;
        Ok(store)
    }

    /// Begins an immediate transaction, so a write never waits to upgrade a read lock.
    /// Output: the transaction every core call takes.
    pub fn transaction(&mut self) -> Result<Transaction<'_>, StoreError> {
        Ok(self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?)
    }

    /// Gives up the store for its connection, already migrated, with foreign keys on and the
    /// busy timeout set, so a session can own it like a host's connection. Output: the
    /// connection.
    pub fn into_connection(self) -> Connection {
        self.connection
    }
}

/// Creates a project and its event. Inputs: the transaction, the actor, the key, the name
/// and the time. Output: the project, or `project_exists` or a text refusal.
pub fn create_project(
    tx: &Transaction<'_>,
    actor: &Actor,
    key: &ProjectKey,
    name: &str,
    now_ms: i64,
) -> Result<Project, StoreError> {
    text::check(TextField::ProjectName, name)?;
    write(tx, || {
        if project_row(tx, key)?.is_some() {
            return Err(refusal(
                codes::PROJECT_EXISTS,
                format!("project {key} exists"),
            ));
        }
        prepare(tx, PROJECT_INSERT)?.execute(params![key.as_str(), name, now_ms])?;
        let created = project(tx, key)?;
        events::record(
            tx,
            Recorded {
                project: created.id,
                ticket: None,
                op: events::PROJECT_CREATE,
                actor,
                at_ms: now_ms,
                before: None,
                after: Some(events::snapshot(&created)),
            },
        )?;
        Ok(created)
    })
}

/// Reads a project by key. Inputs: the transaction and the key. Output: the project, or
/// `project_unknown`.
pub fn project(tx: &Transaction<'_>, key: &ProjectKey) -> Result<Project, StoreError> {
    project_row(tx, key)?
        .ok_or_else(|| refusal(codes::PROJECT_UNKNOWN, format!("no project {key}")))
}

/// Creates a ticket with the project's next number and its event. Inputs: the transaction,
/// the actor, the project key, the fields and the time. Output: the ticket, or a refusal
/// for bad text, a closed status, an unknown project or a full project.
pub fn create_ticket(
    tx: &Transaction<'_>,
    actor: &Actor,
    key: &ProjectKey,
    new: &NewTicket,
    now_ms: i64,
) -> Result<Ticket, StoreError> {
    create_ticket_placed(tx, actor, key, new, Placement::default(), now_ms)
}

/// Creates a ticket already placed under a parent, in a product version and in a cycle,
/// with one event at version 1. The core checks every placement id belongs to the project
/// before it calls this. Inputs: as `create_ticket`, plus the placement. Output: as
/// `create_ticket`.
pub(crate) fn create_ticket_placed(
    tx: &Transaction<'_>,
    actor: &Actor,
    key: &ProjectKey,
    new: &NewTicket,
    placement: Placement,
    now_ms: i64,
) -> Result<Ticket, StoreError> {
    text::check(TextField::Title, &new.title)?;
    text::check(TextField::Body, &new.body)?;
    if new.status.is_closed() {
        return Err(refusal(
            codes::STATUS_CLOSED_AT_CREATE,
            "a ticket starts open; close it with a later write",
        ));
    }
    write(tx, || {
        let owner = project(tx, key)?;
        let (count, last): (i64, i64) = prepare(tx, TICKET_COUNT)?
            .query_row([owner.id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        if count >= TICKETS_PER_PROJECT_MAX as i64 {
            return Err(refusal(
                codes::TICKETS_FULL,
                format!("project {key} holds {TICKETS_PER_PROJECT_MAX} tickets"),
            ));
        }
        let number = last + 1;
        assert!((1..=i64::from(u32::MAX)).contains(&number));
        prepare(tx, TICKET_INSERT)?.execute(params![
            owner.id,
            number,
            new.title,
            new.body,
            new.kind.as_str(),
            new.status.as_str(),
            new.priority.get(),
            actor.as_str(),
            now_ms,
            placement.parent_id.flatten(),
            placement.version_id.flatten(),
            placement.cycle_id.flatten(),
        ])?;
        let created = ticket_by_id(tx, tx.last_insert_rowid())?;
        assert_eq!(i64::from(created.short_id.number()), number);
        assert_eq!(created.parent_id, placement.parent_id.flatten());
        assert_eq!(created.cycle_id, placement.cycle_id.flatten());
        events::record(
            tx,
            Recorded {
                project: created.project,
                ticket: Some(created.id),
                op: events::TICKET_CREATE,
                actor,
                at_ms: now_ms,
                before: None,
                after: Some(events::snapshot(&created)),
            },
        )?;
        Ok(created)
    })
}

/// Reads a ticket by its short id. Inputs: the transaction and the id. Output: the ticket,
/// or `ticket_unknown`.
pub fn ticket(tx: &Transaction<'_>, id: &ShortId) -> Result<Ticket, StoreError> {
    prepare(tx, TICKET_BY_NUMBER)?
        .query_row(params![id.key().as_str(), id.number()], ticket_row)
        .optional()?
        .ok_or_else(|| refusal(codes::TICKET_UNKNOWN, format!("no ticket {id}")))
}

/// Changes a ticket at the version the caller read, and records the change. A change that
/// leaves every field as it is writes nothing. Inputs: the transaction, the actor, the id,
/// the version, the change and the time. Output: the stored ticket, or `stale_version`,
/// `close_reason_open` or a text refusal.
pub fn update_ticket(
    tx: &Transaction<'_>,
    actor: &Actor,
    id: &ShortId,
    version: i64,
    change: &TicketChange,
    now_ms: i64,
) -> Result<Ticket, StoreError> {
    if let Some(title) = &change.title {
        text::check(TextField::Title, title)?;
    }
    if let Some(body) = &change.body {
        text::check(TextField::Body, body)?;
    }
    if let Some(Some(reason)) = &change.close_reason {
        text::check(TextField::CloseReason, reason)?;
    }
    write(tx, || {
        let before = ticket(tx, id)?;
        current(&id.to_string(), before.version, version)?;
        let after = changed(&before, change, actor, now_ms)?;
        if after == before {
            return Ok(before);
        }
        let written = prepare(tx, TICKET_UPDATE)?.execute(params![
            before.id,
            before.version,
            after.title,
            after.body,
            after.kind.as_str(),
            after.status.as_str(),
            after.priority.get(),
            after.assignee.as_ref().map(Actor::as_str),
            now_ms,
            after.closed_ms,
            after.closed_by.as_ref().map(Actor::as_str),
            after.close_reason,
            after.parent_id,
            after.version_id,
            after.cycle_id,
        ])?;
        assert_eq!(
            written, 1,
            "the version read in this transaction still matches"
        );
        let stored = ticket_by_id(tx, before.id)?;
        assert_eq!(stored.version, before.version + 1);
        events::record(
            tx,
            Recorded {
                project: stored.project,
                ticket: Some(stored.id),
                op: events::TICKET_UPDATE,
                actor,
                at_ms: now_ms,
                before: Some(events::snapshot(&before)),
                after: Some(events::snapshot(&stored)),
            },
        )?;
        Ok(stored)
    })
}

fn changed(
    before: &Ticket,
    change: &TicketChange,
    actor: &Actor,
    now_ms: i64,
) -> Result<Ticket, StoreError> {
    let mut after = before.clone();
    if let Some(title) = &change.title {
        after.title.clone_from(title);
    }
    if let Some(body) = &change.body {
        after.body.clone_from(body);
    }
    if let Some(kind) = change.kind {
        after.kind = kind;
    }
    if let Some(status) = change.status {
        after.status = status;
    }
    if let Some(priority) = change.priority {
        after.priority = priority;
    }
    if let Some(assignee) = &change.assignee {
        after.assignee.clone_from(assignee);
    }
    if let Some(reason) = &change.close_reason {
        after.close_reason.clone_from(reason);
    }
    if let Some(parent) = change.placement.parent_id {
        assert_ne!(parent, Some(before.id), "a ticket is never its own parent");
        after.parent_id = parent;
    }
    if let Some(version) = change.placement.version_id {
        after.version_id = version;
    }
    if let Some(cycle) = change.placement.cycle_id {
        after.cycle_id = cycle;
    }
    if after.status.is_closed() {
        if !before.status.is_closed() {
            after.closed_ms = Some(now_ms);
            after.closed_by = Some(actor.clone());
        }
    } else {
        if matches!(change.close_reason, Some(Some(_))) {
            return Err(refusal(
                codes::CLOSE_REASON_OPEN,
                "only a closed ticket has a close reason",
            ));
        }
        after.closed_ms = None;
        after.closed_by = None;
        after.close_reason = None;
    }
    if after != *before {
        after.updated_ms = now_ms;
        after.version = before.version + 1;
    }
    Ok(after)
}

/// Adds an open criterion at the ticket's next position and records it. Inputs: the
/// transaction, the actor, the ticket, the text and the time. Output: the criterion, or
/// `criteria_full` or a text refusal.
pub fn add_criterion(
    tx: &Transaction<'_>,
    actor: &Actor,
    id: &ShortId,
    criterion: &str,
    now_ms: i64,
) -> Result<Criterion, StoreError> {
    text::check(TextField::Criterion, criterion)?;
    write(tx, || {
        let owner = ticket(tx, id)?;
        let (count, last): (i64, i64) = prepare(tx, CRITERION_COUNT)?
            .query_row([owner.id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        if count >= CRITERIA_PER_TICKET_MAX as i64 {
            return Err(refusal(
                codes::CRITERIA_FULL,
                format!("{id} holds {CRITERIA_PER_TICKET_MAX} criteria"),
            ));
        }
        prepare(tx, CRITERION_INSERT)?.execute(params![
            owner.id,
            last + 1,
            criterion,
            CriterionState::Open.as_str(),
        ])?;
        let added = criterion_by_id(tx, tx.last_insert_rowid())?;
        events::record(
            tx,
            Recorded {
                project: owner.project,
                ticket: Some(owner.id),
                op: events::CRITERION_ADD,
                actor,
                at_ms: now_ms,
                before: None,
                after: Some(events::snapshot(&added)),
            },
        )?;
        Ok(added)
    })
}

/// Sets a criterion's state at the version the caller read, and records it. Setting the
/// state it already has writes nothing. Inputs: the transaction, the actor, the ticket, the
/// position, the version, the state and the time. Output: the criterion, or
/// `criterion_unknown` or `stale_version`.
pub fn set_criterion(
    tx: &Transaction<'_>,
    actor: &Actor,
    id: &ShortId,
    position: u32,
    version: i64,
    state: CriterionState,
    now_ms: i64,
) -> Result<Criterion, StoreError> {
    write(tx, || {
        let owner = ticket(tx, id)?;
        let before = prepare(tx, CRITERION_BY_POSITION)?
            .query_row(params![owner.id, position], criterion_row)
            .optional()?
            .ok_or_else(|| {
                refusal(
                    codes::CRITERION_UNKNOWN,
                    format!("{id} has no criterion {position}"),
                )
            })?;
        current(
            &format!("{id} criterion {position}"),
            before.version,
            version,
        )?;
        if before.state == state {
            return Ok(before);
        }
        let written = prepare(tx, CRITERION_STATE_UPDATE)?.execute(params![
            before.id,
            before.version,
            state.as_str(),
        ])?;
        assert_eq!(
            written, 1,
            "the version read in this transaction still matches"
        );
        let after = criterion_by_id(tx, before.id)?;
        events::record(
            tx,
            Recorded {
                project: owner.project,
                ticket: Some(owner.id),
                op: events::CRITERION_SET,
                actor,
                at_ms: now_ms,
                before: Some(events::snapshot(&before)),
                after: Some(events::snapshot(&after)),
            },
        )?;
        Ok(after)
    })
}

/// Links a ticket to a target of a kind and records it. Inputs: the transaction, the
/// actor, the ticket, the kind, the target and the time. Output: the link, or
/// `link_exists`, `links_full` or a text refusal.
pub fn add_link(
    tx: &Transaction<'_>,
    actor: &Actor,
    id: &ShortId,
    kind: &str,
    target: &str,
    now_ms: i64,
) -> Result<Link, StoreError> {
    text::check(TextField::LinkKind, kind)?;
    text::check(TextField::LinkTarget, target)?;
    write(tx, || {
        let owner = ticket(tx, id)?;
        if link_row(tx, owner.id, kind, target)?.is_some() {
            return Err(refusal(
                codes::LINK_EXISTS,
                format!("{id} already links this {kind}"),
            ));
        }
        let count: i64 = prepare(tx, LINK_COUNT)?.query_row([owner.id], |row| row.get(0))?;
        if count >= LINKS_PER_TICKET_MAX as i64 {
            return Err(refusal(
                codes::LINKS_FULL,
                format!("{id} holds {LINKS_PER_TICKET_MAX} links"),
            ));
        }
        prepare(tx, LINK_INSERT)?.execute(params![
            owner.id,
            kind,
            target,
            actor.as_str(),
            now_ms
        ])?;
        let added = Link {
            ticket: owner.id,
            kind: kind.to_owned(),
            target: target.to_owned(),
            created_by: actor.clone(),
            created_ms: now_ms,
        };
        events::record(
            tx,
            Recorded {
                project: owner.project,
                ticket: Some(owner.id),
                op: events::LINK_ADD,
                actor,
                at_ms: now_ms,
                before: None,
                after: Some(events::snapshot(&added)),
            },
        )?;
        Ok(added)
    })
}

/// Removes a ticket's link and records it. Inputs: the transaction, the actor, the ticket,
/// the kind, the target and the time. Output: the removed link, or `link_unknown`.
pub fn remove_link(
    tx: &Transaction<'_>,
    actor: &Actor,
    id: &ShortId,
    kind: &str,
    target: &str,
    now_ms: i64,
) -> Result<Link, StoreError> {
    write(tx, || {
        let owner = ticket(tx, id)?;
        let removed = link_row(tx, owner.id, kind, target)?
            .ok_or_else(|| refusal(codes::LINK_UNKNOWN, format!("{id} has no such {kind} link")))?;
        let written = prepare(tx, LINK_DELETE)?.execute(params![owner.id, kind, target])?;
        assert_eq!(
            written, 1,
            "the link read in this transaction is still there"
        );
        events::record(
            tx,
            Recorded {
                project: owner.project,
                ticket: Some(owner.id),
                op: events::LINK_REMOVE,
                actor,
                at_ms: now_ms,
                before: Some(events::snapshot(&removed)),
                after: None,
            },
        )?;
        Ok(removed)
    })
}

/// Reads one project's stored tickets, criteria, links, checks and evidence. Inputs: the
/// transaction and the key. Output: the state, or `project_unknown`.
pub fn state(tx: &Transaction<'_>, key: &ProjectKey) -> Result<State, StoreError> {
    let owner = project(tx, key)?;
    let tickets = prepare(tx, TICKETS_OF_PROJECT)?
        .query_map([owner.id], ticket_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let criteria = prepare(tx, CRITERIA_OF_PROJECT)?
        .query_map([owner.id], criterion_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let links = prepare(tx, LINKS_OF_PROJECT)?
        .query_map([owner.id], link_row_of)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let checks = crate::criteria::project_checks(tx, owner.id)?;
    let evidence = crate::criteria::project_evidence(tx, owner.id)?;
    Ok(State {
        tickets,
        criteria,
        links,
        checks,
        evidence,
    })
}

/// Prepares one of the core's listed statements. Plain `prepare`, because the core builds
/// rusqlite without its statement cache feature. Inputs: the transaction and the statement.
/// Output: the prepared statement.
pub(crate) fn prepare<'t>(
    tx: &'t Transaction<'_>,
    sql: &'static str,
) -> rusqlite::Result<Statement<'t>> {
    assert!(STATEMENTS.contains(&sql), "every core statement is listed");
    tx.prepare(sql)
}

/// Builds the refusal error for one code. Inputs: the code and the message. Output: the
/// error.
pub(crate) fn refusal(code: &'static str, message: impl Into<String>) -> StoreError {
    StoreError::Refused(Refusal::new(code, message))
}

/// Runs one write inside a savepoint, so a refusal or error anywhere in it leaves no row and
/// no event; writes nest. Inputs: the transaction and the write. Output: the write's result.
pub(crate) fn write<T>(
    tx: &Transaction<'_>,
    body: impl FnOnce() -> Result<T, StoreError>,
) -> Result<T, StoreError> {
    tx.execute_batch("SAVEPOINT tk_write")?;
    match body() {
        Ok(value) => {
            tx.execute_batch("RELEASE tk_write")?;
            Ok(value)
        }
        Err(error) => {
            if let Err(rollback) = tx.execute_batch("ROLLBACK TO tk_write; RELEASE tk_write") {
                assert!(
                    tx.is_autocommit(),
                    "a failed write rolls back to its savepoint: {rollback}"
                );
            }
            Err(error)
        }
    }
}

/// Refuses a write made at a version other than the stored one. Inputs: what is written,
/// the stored version and the caller's. Output: Ok, or `stale_version`.
pub(crate) fn current(what: &str, stored: i64, expected: i64) -> Result<(), StoreError> {
    if stored == expected {
        return Ok(());
    }
    Err(refusal(
        codes::STALE_VERSION,
        format!("{what} is at version {stored}, not {expected}; show it and retry"),
    ))
}

fn project_row(tx: &Transaction<'_>, key: &ProjectKey) -> Result<Option<Project>, StoreError> {
    Ok(prepare(tx, PROJECT_BY_KEY)?
        .query_row([key.as_str()], |row| {
            Ok(Project {
                id: row.get(0)?,
                key: ProjectKey::new(&row.get::<_, String>(1)?)
                    .map_err(|refused| conversion(1, refused))?,
                name: row.get(2)?,
                created_ms: row.get(3)?,
                version: row.get(4)?,
            })
        })
        .optional()?)
}

/// Reads a ticket by its row id. Inputs: the transaction and the id. Output: the ticket.
pub(crate) fn ticket_by_id(tx: &Transaction<'_>, id: i64) -> Result<Ticket, StoreError> {
    Ok(prepare(tx, TICKET_BY_ID)?.query_row([id], ticket_row)?)
}

fn criterion_by_id(tx: &Transaction<'_>, id: i64) -> Result<Criterion, StoreError> {
    Ok(prepare(tx, CRITERION_BY_ID)?.query_row([id], criterion_row)?)
}

fn link_row(
    tx: &Transaction<'_>,
    ticket: i64,
    kind: &str,
    target: &str,
) -> Result<Option<Link>, StoreError> {
    Ok(prepare(tx, LINK_GET)?
        .query_row(params![ticket, kind, target], link_row_of)
        .optional()?)
}

fn ticket_row(row: &Row<'_>) -> rusqlite::Result<Ticket> {
    let key =
        ProjectKey::new(&row.get::<_, String>(2)?).map_err(|refused| conversion(2, refused))?;
    let short_id = ShortId::new(key, row.get(3)?).map_err(|refused| conversion(3, refused))?;
    Ok(Ticket {
        id: row.get(0)?,
        project: row.get(1)?,
        short_id,
        title: row.get(4)?,
        body: row.get(5)?,
        kind: named(row, 6, TicketKind::parse)?,
        status: named(row, 7, TicketStatus::parse)?,
        priority: Priority::new(row.get(8)?).map_err(|refused| conversion(8, refused))?,
        version_id: row.get(9)?,
        cycle_id: row.get(10)?,
        parent_id: row.get(11)?,
        assignee: optional_actor(row, 12)?,
        created_by: actor(row, 13)?,
        created_ms: row.get(14)?,
        updated_ms: row.get(15)?,
        closed_ms: row.get(16)?,
        closed_by: optional_actor(row, 17)?,
        close_reason: row.get(18)?,
        version: row.get(19)?,
    })
}

/// Reads a criterion from a row of `criterion_select!`'s columns. Input: the row. Output:
/// the criterion.
pub(crate) fn criterion_row(row: &Row<'_>) -> rusqlite::Result<Criterion> {
    Ok(Criterion {
        id: row.get(0)?,
        ticket: row.get(1)?,
        position: row.get(2)?,
        text: row.get(3)?,
        state: named(row, 4, CriterionState::parse)?,
        version: row.get(5)?,
    })
}

fn link_row_of(row: &Row<'_>) -> rusqlite::Result<Link> {
    Ok(Link {
        ticket: row.get(0)?,
        kind: row.get(1)?,
        target: row.get(2)?,
        created_by: actor(row, 3)?,
        created_ms: row.get(4)?,
    })
}

/// Reads a stored actor, the core's own `host:` actors included. Inputs: the row and the
/// column. Output: the actor, or a conversion error for a bad shape.
pub(crate) fn actor(row: &Row<'_>, index: usize) -> rusqlite::Result<Actor> {
    Actor::stored(&row.get::<_, String>(index)?).map_err(|refused| conversion(index, refused))
}

fn optional_actor(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<Actor>> {
    row.get::<_, Option<String>>(index)?
        .map(|text| Actor::stored(&text))
        .transpose()
        .map_err(|refused| conversion(index, refused))
}

/// Reads a stored name of a closed set. Inputs: the row, the column and the set's parser.
/// Output: the value, or a conversion error for a name this core does not know.
pub(crate) fn named<T>(
    row: &Row<'_>,
    index: usize,
    parse: fn(&str) -> Option<T>,
) -> rusqlite::Result<T> {
    let stored: String = row.get(index)?;
    parse(&stored).ok_or_else(|| {
        conversion(
            index,
            Refusal::new(
                codes::STORE_FAILED,
                "a stored name is not one this core knows",
            ),
        )
    })
}

/// Wraps a refusal met while reading a column as SQLite's conversion error. Inputs: the
/// column and the refusal. Output: the error.
pub(crate) fn conversion(
    index: usize,
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(error))
}

fn path_refused(message: &str) -> StoreError {
    refusal(codes::STORE_PATH_REFUSED, message)
}

fn io_failed(what: &str, error: &io::Error) -> StoreError {
    refusal(
        codes::STORE_FAILED,
        format!("the tracker {what} cannot be opened: {}", error.kind()),
    )
}

fn private_folder(folder: &Path) -> Result<(), StoreError> {
    match fs::symlink_metadata(folder) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_folder(folder).map_err(|error| io_failed("folder", &error))?;
        }
        Err(error) => return Err(io_failed("folder", &error)),
    }
    let metadata = fs::symlink_metadata(folder).map_err(|error| io_failed("folder", &error))?;
    if !metadata.file_type().is_dir() {
        return Err(path_refused(
            "the tracker folder is a symlink or not a folder",
        ));
    }
    private(&metadata, "folder")
}

fn private_file(path: &Path) -> Result<(), StoreError> {
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_file(path).map_err(|error| io_failed("database", &error))?;
        }
        Err(error) => return Err(io_failed("database", &error)),
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| io_failed("database", &error))?;
    if !metadata.file_type().is_file() {
        return Err(path_refused(
            "the tracker database is a symlink or not a plain file",
        ));
    }
    private(&metadata, "database")
}

#[cfg(unix)]
fn create_folder(folder: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(folder)
}

#[cfg(not(unix))]
fn create_folder(folder: &Path) -> io::Result<()> {
    fs::create_dir_all(folder)
}

#[cfg(unix)]
fn create_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map(drop)
}

#[cfg(not(unix))]
fn create_file(path: &Path) -> io::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(drop)
}

#[cfg(unix)]
fn private(metadata: &fs::Metadata, what: &str) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o077 == 0 {
        return Ok(());
    }
    Err(refusal(
        codes::STORE_NOT_PRIVATE,
        format!("other users may open the tracker {what}; make it private to you"),
    ))
}

#[cfg(not(unix))]
fn private(_metadata: &fs::Metadata, _what: &str) -> Result<(), StoreError> {
    Ok(())
}
