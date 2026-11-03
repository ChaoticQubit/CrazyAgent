use crate::model::{
    Actor, CheckKind, Link, OpKind, ProjectKey, Refusal, ShortId, Ticket, TicketKind, TicketStatus,
    codes,
};
use crate::policy;
use crate::store::{self, StoreError};
use crate::text::{self, TextField};
use rusqlite::Transaction;

/// One link kind a host declares, for example `branch` or `change_set`. Fields: the stored
/// name and the label a person reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkKind {
    pub name: &'static str,
    pub label: &'static str,
}

/// What a host knows of one link target. Fields: whether the target still exists, the
/// label to show for it, and the host's own state text (standalone: `unknown`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkState {
    pub live: bool,
    pub label: String,
    pub state: String,
}

/// Whether a host finds a check satisfied. Fields: the answer and one line of detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceState {
    pub satisfied: bool,
    pub detail: String,
}

/// What a host sees of one ticket when it decides whether an actor may act on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketView {
    pub id: ShortId,
    pub title: String,
    pub kind: TicketKind,
    pub status: TicketStatus,
    pub assignee: Option<Actor>,
    pub created_by: Actor,
}

impl TicketView {
    /// The view of a stored ticket. Input: the ticket. Output: its view.
    pub fn of(ticket: &Ticket) -> Self {
        Self {
            id: ticket.short_id.clone(),
            title: ticket.title.clone(),
            kind: ticket.kind,
            status: ticket.status,
            assignee: ticket.assignee.clone(),
            created_by: ticket.created_by.clone(),
        }
    }
}

/// What a host sees of one check when it reports whether the check is satisfied. Fields:
/// the ticket, the criterion's position on it, the check's kind and its reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckView {
    pub ticket: ShortId,
    pub criterion: u32,
    pub kind: CheckKind,
    pub reference: String,
}

/// A change a host reports on one of its link targets, for example an accepted change
/// set. Fields: the link kind and the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEvent {
    pub kind: String,
    pub target: String,
}

/// Resolves the links a host declares. Every method gets the open transaction, so a host
/// reads its own tables in the same transaction as the core's write.
pub trait Links {
    /// The link kinds this host accepts. Output: the kinds; an empty list accepts any bounded
    /// kind, as the standalone host does.
    fn kinds(&self) -> &[LinkKind];

    /// Checks one target. Inputs: the transaction, the project, the kind and the target.
    /// Output: what the host knows of it, or a refusal that blocks the link.
    fn resolve(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        kind: &str,
        target: &str,
    ) -> Result<LinkState, Refusal>;
}

/// Reports whether checks are satisfied, from the host's own records.
pub trait Evidence {
    /// Checks one check. Inputs: the transaction, the project and the check. Output: its
    /// state, or a refusal.
    fn check(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        check: &CheckView,
    ) -> Result<EvidenceState, Refusal>;
}

/// Decides who may run which operation.
pub trait Authority {
    /// Checks one actor and operation. Inputs: the transaction, the project, the actor,
    /// the operation and the ticket it acts on, if any. Output: Ok, or the refusal.
    fn actor_may(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        actor: &Actor,
        op: OpKind,
        ticket: Option<&TicketView>,
    ) -> Result<(), Refusal>;

    /// Tells whether the host sets every actor from its own knowledge of the caller. Output:
    /// false when actors declare themselves, as in the standalone host, so policy binds
    /// honest agents only; a `policy_get` reply says which.
    fn actors_verified(&self) -> bool;

    /// Checks one actor recording evidence for one check, before anything is written. It
    /// has no default, so every host decides who may prove its checks. Inputs: the
    /// transaction, the project, the actor and the check. Output: Ok, or the refusal.
    fn evidence_may(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        actor: &Actor,
        check: &CheckView,
    ) -> Result<(), Refusal>;
}

/// Everything a host supplies: links, evidence and authority. Any type with the three is
/// a host.
pub trait Host: Links + Evidence + Authority {}

impl<T: Links + Evidence + Authority> Host for T {}

/// Links a ticket to a host target: the text is checked, the actor authorized, the kind
/// matched against the host's kinds and the target resolved by the host in the same
/// transaction before anything is written, so a refused link writes nothing. A closed
/// ticket still takes links. Inputs: the transaction, the host, the actor, the ticket, the
/// kind, the target and the time. Output: the link and what the host knows of its target.
pub fn link(
    tx: &Transaction<'_>,
    host: &impl Host,
    actor: &Actor,
    id: &ShortId,
    kind: &str,
    target: &str,
    now_ms: i64,
) -> Result<(Link, LinkState), StoreError> {
    text::check(TextField::LinkKind, kind)?;
    text::check(TextField::LinkTarget, target)?;
    let ticket = store::ticket(tx, id)?;
    policy::authorize(tx, host, actor, OpKind::Link, &ticket)?;
    let kinds = host.kinds();
    if !kinds.is_empty() && !kinds.iter().any(|declared| declared.name == kind) {
        return Err(store::refusal(
            codes::LINK_KIND_UNKNOWN,
            format!("this tracker links no {kind}"),
        ));
    }
    let state = host
        .resolve(tx, id.key(), kind, target)
        .map_err(StoreError::Refused)?;
    let link = store::add_link(tx, actor, id, kind, target, now_ms)?;
    assert_eq!(link.kind, kind);
    assert_eq!(link.target, target);
    Ok((link, state))
}

/// Removes a ticket's link after authorizing the actor. Inputs: the transaction, the host,
/// the actor, the ticket, the kind, the target and the time. Output: the removed link, or
/// `link_unknown` or the refusal.
pub fn unlink(
    tx: &Transaction<'_>,
    host: &impl Host,
    actor: &Actor,
    id: &ShortId,
    kind: &str,
    target: &str,
    now_ms: i64,
) -> Result<Link, StoreError> {
    let ticket = store::ticket(tx, id)?;
    policy::authorize(tx, host, actor, OpKind::Unlink, &ticket)?;
    let removed = store::remove_link(tx, actor, id, kind, target, now_ms)?;
    assert_eq!(removed.kind, kind);
    assert_eq!(removed.target, target);
    Ok(removed)
}
