use crate::events::{self, Recorded};
use crate::host::{CheckView, Host, HostEvent};
use crate::model::{
    Actor, CHECKS_PER_CRITERION_MAX, CRITERIA_PER_TICKET_MAX, Check, CheckKind, ClosePolicy,
    Criterion, CriterionState, Evidence, EvidenceResult, OpKind, ShortId, TICKETS_PER_PROJECT_MAX,
    Ticket, TicketStatus, codes,
};
use crate::ops::{self, Candidate, Cx, Failure};
use crate::policy::{self, Role};
use crate::store::{self, StoreError, TicketChange};
use crate::text::{self, CLOSE_REASON_BYTES_MAX, TextField};
use rusqlite::{OptionalExtension, Row, Transaction, params};
use serde_json::{Value, json};

pub(crate) const CRITERIA_OF_TICKET: &str = "SELECT id, ticket, position, text, state, version \
     FROM tk_criteria WHERE ticket = ?1 ORDER BY position";
pub(crate) const CRITERION_TEXT_UPDATE: &str =
    "UPDATE tk_criteria SET text = ?3, version = version + 1 WHERE id = ?1 AND version = ?2";
pub(crate) const CHECKS_OF_CRITERION: &str =
    "SELECT id, criterion, kind, ref FROM tk_checks WHERE criterion = ?1 ORDER BY id";
pub(crate) const CHECK_INSERT: &str =
    "INSERT INTO tk_checks(criterion, kind, ref) VALUES (?1, ?2, ?3)";
pub(crate) const CHECKS_OF_PROJECT: &str = "SELECT k.id, k.criterion, k.kind, k.ref \
     FROM tk_checks k JOIN tk_criteria c ON c.id = k.criterion \
     JOIN tk_tickets t ON t.id = c.ticket WHERE t.project = ?1 ORDER BY k.id";
pub(crate) const EVIDENCE_INSERT: &str = "INSERT INTO tk_evidence(check_id, result, ref, \
     recorded_by, recorded_ms) VALUES (?1, ?2, ?3, ?4, ?5)";
pub(crate) const EVIDENCE_LATEST: &str = "SELECT id, check_id, result, ref, recorded_by, \
     recorded_ms FROM tk_evidence WHERE check_id = ?1 ORDER BY id DESC LIMIT 1";
pub(crate) const EVIDENCE_OF_PROJECT: &str = "SELECT e.id, e.check_id, e.result, e.ref, \
     e.recorded_by, e.recorded_ms FROM tk_evidence e JOIN tk_checks k ON k.id = e.check_id \
     JOIN tk_criteria c ON c.id = k.criterion JOIN tk_tickets t ON t.id = c.ticket \
     WHERE t.project = ?1 ORDER BY e.id";
pub(crate) const TICKETS_LINKED_OPEN: &str = "SELECT t.id FROM tk_links l \
     JOIN tk_tickets t ON t.id = l.ticket WHERE l.kind = ?1 AND l.target = ?2 \
     AND t.status NOT IN ('done', 'cancelled') ORDER BY t.id LIMIT ?3";

/// The most open tickets one host event re-evaluates.
pub const NOTIFY_TICKETS_MAX: usize = TICKETS_PER_PROJECT_MAX;
/// The core's own actor for criteria it re-derives and tickets it closes on evidence.
pub const EVIDENCE_SOURCE: &str = "evidence";

const REASON_FIXED_BYTES_MAX: usize = 128;

const _: () = assert!(
    REASON_FIXED_BYTES_MAX
        + text::LINK_KIND_BYTES_MAX
        + text::LINK_TARGET_BYTES_MAX
        + crate::model::ACTOR_BYTES_MAX
        <= CLOSE_REASON_BYTES_MAX
);
const _: () = assert!(text::CHECK_REF_BYTES_MAX <= text::LINK_TARGET_BYTES_MAX);
const _: () = assert!(CHECKS_PER_CRITERION_MAX * CRITERIA_PER_TICKET_MAX <= 256);

/// One outcome a caller reports for a check. Fields: the ticket, the criterion's position,
/// the check's kind and reference, the result, and where the outcome can be read (a run or
/// a look), if the caller names it; none is stored as an empty reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEvidence {
    pub ticket: ShortId,
    pub criterion: u32,
    pub kind: CheckKind,
    pub check: String,
    pub result: EvidenceResult,
    pub reference: Option<String>,
}

/// What a criterion write left: the criterion and its ticket, closed when the write
/// completed the proof under `close = evidence`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub criterion: Criterion,
    pub ticket: Ticket,
}

/// What completed a proof, named in the reason of an evidence close.
enum Proof<'a> {
    Check(CheckKind, &'a str),
    Event(&'a HostEvent),
    Criterion(u32, &'a Actor),
    Changed(&'a str),
}

impl Proof<'_> {
    fn describe(&self) -> String {
        match self {
            Proof::Check(kind, reference) => format!("{kind} check {reference}"),
            Proof::Event(event) => format!("{} {} changed", event.kind, event.target),
            Proof::Criterion(position, actor) => format!("criterion {position} set by {actor}"),
            Proof::Changed(change) => (*change).to_owned(),
        }
    }
}

/// Adds an open criterion to an open ticket after authorizing the actor. Inputs: the
/// transaction, the host, the actor, the ticket, the text and the time. Output: the
/// criterion, or `ticket_closed`, `criteria_full`, a text refusal or the host's refusal.
pub fn add_criterion(
    tx: &Transaction<'_>,
    host: &impl Host,
    actor: &Actor,
    id: &ShortId,
    criterion: &str,
    now_ms: i64,
) -> Result<Criterion, StoreError> {
    let ticket = open_ticket(tx, host, actor, OpKind::CriterionAdd, id)?;
    let added = store::add_criterion(tx, actor, id, criterion, now_ms)?;
    assert_eq!(added.ticket, ticket.id);
    assert_eq!(added.state, CriterionState::Open);
    Ok(added)
}

/// Rewrites a criterion's text at the version the caller read, keeping its state and checks.
/// The same text writes nothing. A waived criterion is the owner's to rewrite, since the
/// waiver was given for its text. Inputs: the transaction, the host, the actor, the ticket,
/// the position, the version, the text and the time. Output: the criterion, or
/// `criterion_unknown`, `stale_version`, `owner_only`, `ticket_closed`, a text or the host's
/// refusal.
#[allow(clippy::too_many_arguments)]
pub fn edit_criterion(
    tx: &Transaction<'_>,
    host: &impl Host,
    actor: &Actor,
    id: &ShortId,
    position: u32,
    version: i64,
    criterion: &str,
    now_ms: i64,
) -> Result<Criterion, StoreError> {
    text::check(TextField::Criterion, criterion)?;
    store::write(tx, || {
        let ticket = open_ticket(tx, host, actor, OpKind::CriterionEdit, id)?;
        let before = criterion_at(tx, &ticket, position)?;
        store::current(
            &format!("{id} criterion {position}"),
            before.version,
            version,
        )?;
        if before.state == CriterionState::Waived && policy::role(actor) != Role::Owner {
            return Err(store::refusal(
                codes::OWNER_ONLY,
                format!("the owner waived {id} criterion {position}; only the owner rewrites it"),
            ));
        }
        if before.text == criterion {
            return Ok(before);
        }
        let written = store::prepare(tx, CRITERION_TEXT_UPDATE)?.execute(params![
            before.id,
            before.version,
            criterion
        ])?;
        assert_eq!(
            written, 1,
            "the version read in this transaction still matches"
        );
        let after = criterion_at(tx, &ticket, position)?;
        assert_eq!(after.version, before.version + 1);
        assert_eq!(after.state, before.state);
        events::record(
            tx,
            Recorded {
                project: ticket.project,
                ticket: Some(ticket.id),
                op: events::CRITERION_EDIT,
                actor,
                at_ms: now_ms,
                before: Some(events::snapshot(&before)),
                after: Some(events::snapshot(&after)),
            },
        )?;
        Ok(after)
    })
}

/// Adds a check that proves a criterion, then re-derives that criterion through the host, so
/// a check already satisfied counts at once. Inputs: the transaction, the host, the actor,
/// the ticket, the position, the check's kind and reference, and the time. Output: the
/// check, or `check_exists`, `checks_full`, `criterion_unknown`, `ticket_closed`, a text
/// refusal or the host's refusal.
#[allow(clippy::too_many_arguments)]
pub fn add_check(
    tx: &Transaction<'_>,
    host: &impl Host,
    actor: &Actor,
    id: &ShortId,
    position: u32,
    kind: CheckKind,
    reference: &str,
    now_ms: i64,
) -> Result<Check, StoreError> {
    text::check(TextField::CheckRef, reference)?;
    store::write(tx, || {
        let ticket = open_ticket(tx, host, actor, OpKind::CheckAdd, id)?;
        let criterion = criterion_at(tx, &ticket, position)?;
        let checks = checks_of(tx, criterion.id)?;
        if checks
            .iter()
            .any(|check| check.kind == kind && check.reference == reference)
        {
            return Err(store::refusal(
                codes::CHECK_EXISTS,
                format!("{id} criterion {position} already has this {kind} check"),
            ));
        }
        if checks.len() >= CHECKS_PER_CRITERION_MAX {
            return Err(store::refusal(
                codes::CHECKS_FULL,
                format!("{id} criterion {position} holds {CHECKS_PER_CRITERION_MAX} checks"),
            ));
        }
        store::prepare(tx, CHECK_INSERT)?.execute(params![
            criterion.id,
            kind.as_str(),
            reference
        ])?;
        let added = Check {
            id: tx.last_insert_rowid(),
            criterion: criterion.id,
            kind,
            reference: reference.to_owned(),
        };
        assert_eq!(checks_of(tx, criterion.id)?.len(), checks.len() + 1);
        events::record(
            tx,
            Recorded {
                project: ticket.project,
                ticket: Some(ticket.id),
                op: events::CHECK_ADD,
                actor,
                at_ms: now_ms,
                before: None,
                after: Some(events::snapshot(&added)),
            },
        )?;
        evaluate(
            tx,
            host,
            &ticket,
            Some(position),
            &Proof::Check(kind, reference),
            now_ms,
        )?;
        Ok(added)
    })
}

/// Appends one evidence record for a check of an open ticket, once the host's
/// `evidence_may` lets the actor prove that check, then re-derives the check's criterion:
/// met when every check is satisfied, open again on a failing one. Under `close = evidence`
/// the last satisfied check closes the ticket as `host:evidence`, with a reason naming the
/// proof. Inputs: the transaction, the host, the actor, the evidence and the time. Output:
/// the evidence row and what it left, or `check_unknown`, `criterion_unknown`,
/// `ticket_closed`, a text refusal or the host's refusal.
pub fn record_evidence(
    tx: &Transaction<'_>,
    host: &impl Host,
    actor: &Actor,
    new: &NewEvidence,
    now_ms: i64,
) -> Result<(Evidence, Outcome), StoreError> {
    text::check(TextField::CheckRef, &new.check)?;
    if let Some(reference) = &new.reference {
        text::check(TextField::CheckRef, reference)?;
    }
    let reference = new.reference.clone().unwrap_or_default();
    store::write(tx, || {
        let ticket = open_ticket(tx, host, actor, OpKind::EvidenceRecord, &new.ticket)?;
        let criterion = criterion_at(tx, &ticket, new.criterion)?;
        let check = check_of(tx, &criterion, new.kind, &new.check)?;
        host.evidence_may(
            tx,
            ticket.short_id.key(),
            actor,
            &view_of(&ticket, &criterion, &check),
        )
        .map_err(StoreError::Refused)?;
        store::prepare(tx, EVIDENCE_INSERT)?.execute(params![
            check.id,
            new.result.as_str(),
            reference,
            actor.as_str(),
            now_ms
        ])?;
        let recorded = Evidence {
            id: tx.last_insert_rowid(),
            check: check.id,
            result: new.result,
            reference: reference.clone(),
            recorded_by: actor.clone(),
            recorded_ms: now_ms,
        };
        assert_eq!(latest_of(tx, check.id)?.as_ref(), Some(&recorded));
        events::record(
            tx,
            Recorded {
                project: ticket.project,
                ticket: Some(ticket.id),
                op: events::EVIDENCE_RECORD,
                actor,
                at_ms: now_ms,
                before: None,
                after: Some(events::snapshot(&recorded)),
            },
        )?;
        let proof = Proof::Check(new.kind, &new.check);
        let after = evaluate(tx, host, &ticket, Some(new.criterion), &proof, now_ms)?;
        let criterion = criterion_at(tx, &after, new.criterion)?;
        Ok((
            recorded,
            Outcome {
                criterion,
                ticket: after,
            },
        ))
    })
}

/// Sets a criterion's state by hand at the version the caller read. A criterion with checks
/// follows them: it is never set met or reopened by hand, only waived by the owner or taken
/// back from waived to open. Waiving is the owner's. Under `close = evidence` an agent may
/// not set a criterion without checks met, so no agent closes a ticket on its own word, and
/// the last met criterion closes the ticket. Inputs: the transaction, the host, the actor,
/// the ticket, the position, the version, the state and the time. Output: what the write
/// left, or `owner_only`, `criterion_has_checks`, `criterion_needs_check`, `stale_version`,
/// `ticket_closed` or a refusal.
#[allow(clippy::too_many_arguments)]
pub fn set_criterion(
    tx: &Transaction<'_>,
    host: &impl Host,
    actor: &Actor,
    id: &ShortId,
    position: u32,
    version: i64,
    state: CriterionState,
    now_ms: i64,
) -> Result<Outcome, StoreError> {
    store::write(tx, || {
        let ticket = open_ticket(tx, host, actor, OpKind::CriterionSet, id)?;
        let before = criterion_at(tx, &ticket, position)?;
        if state == CriterionState::Waived && policy::role(actor) != Role::Owner {
            return Err(store::refusal(
                codes::OWNER_ONLY,
                "only the owner waives a criterion",
            ));
        }
        let checked = !checks_of(tx, before.id)?.is_empty();
        if checked && !by_hand_with_checks(before.state, state) {
            return Err(store::refusal(
                codes::CRITERION_HAS_CHECKS,
                format!("{id} criterion {position} follows its checks; record evidence"),
            ));
        }
        if !checked && state == CriterionState::Met && self_certified(tx, actor, id)? {
            return Err(store::refusal(
                codes::CRITERION_NEEDS_CHECK,
                format!(
                    "under close = evidence an agent proves {id} criterion {position} with a \
                     check; add one or ask the owner"
                ),
            ));
        }
        let set = store::set_criterion(tx, actor, id, position, version, state, now_ms)?;
        assert_eq!(set.state, state);
        let proof = Proof::Criterion(position, actor);
        let after = evaluate(tx, host, &ticket, Some(position), &proof, now_ms)?;
        let criterion = criterion_at(tx, &after, position)?;
        Ok(Outcome {
            criterion,
            ticket: after,
        })
    })
}

/// Re-evaluates the open tickets linked to a host target that changed, for example an
/// accepted change set: every criterion with checks is re-derived through the host and,
/// under `close = evidence`, a fully proven ticket closes as `host:evidence`, all in the
/// caller's transaction. Closed and unlinked tickets are left as they are. Inputs: the
/// transaction, the host, the event and the time. Output: the tickets re-evaluated, in
/// creation order, or `notify_too_wide`, a text refusal or the host's refusal.
pub fn notify(
    tx: &Transaction<'_>,
    host: &impl Host,
    event: &HostEvent,
    now_ms: i64,
) -> Result<Vec<ShortId>, StoreError> {
    text::check(TextField::LinkKind, &event.kind)?;
    text::check(TextField::LinkTarget, &event.target)?;
    store::write(tx, || {
        let rows = store::prepare(tx, TICKETS_LINKED_OPEN)?
            .query_map(
                params![event.kind, event.target, NOTIFY_TICKETS_MAX as i64 + 1],
                |row| row.get::<_, i64>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if rows.len() > NOTIFY_TICKETS_MAX {
            return Err(store::refusal(
                codes::NOTIFY_TOO_WIDE,
                format!("more than {NOTIFY_TICKETS_MAX} open tickets link this target"),
            ));
        }
        let mut touched = Vec::with_capacity(rows.len());
        for row in rows {
            let ticket = store::ticket_by_id(tx, row)?;
            assert!(!ticket.status.is_closed());
            evaluate(tx, host, &ticket, None, &Proof::Event(event), now_ms)?;
            touched.push(ticket.short_id);
        }
        assert!(touched.len() <= NOTIFY_TICKETS_MAX);
        Ok(touched)
    })
}

/// Re-evaluates open tickets after a record beside them changed, such as an ended run: each
/// criterion with checks is re-derived through the host, and under `close = evidence` a fully
/// proven ticket closes as `host:evidence`. Inputs: the transaction, the host, open tickets'
/// row ids (at most `NOTIFY_TICKETS_MAX`), what changed (at most `LINK_TARGET_BYTES_MAX`
/// bytes, named in the close reason) and the time. Output: the tickets in order, or a refusal.
pub(crate) fn reevaluate(
    tx: &Transaction<'_>,
    host: &impl Host,
    tickets: &[i64],
    change: &str,
    now_ms: i64,
) -> Result<Vec<ShortId>, StoreError> {
    assert!(tickets.len() <= NOTIFY_TICKETS_MAX);
    assert!(!change.is_empty());
    assert!(change.len() <= text::LINK_TARGET_BYTES_MAX);
    store::write(tx, || {
        let mut touched = Vec::with_capacity(tickets.len());
        for row in tickets {
            let ticket = store::ticket_by_id(tx, *row)?;
            assert!(
                !ticket.status.is_closed(),
                "only open tickets are re-evaluated"
            );
            evaluate(tx, host, &ticket, None, &Proof::Changed(change), now_ms)?;
            touched.push(ticket.short_id);
        }
        assert_eq!(touched.len(), tickets.len());
        Ok(touched)
    })
}

/// A ticket's criteria by position. Inputs: the transaction and the ticket. Output: the
/// criteria, or `ticket_unknown`.
pub fn criteria(tx: &Transaction<'_>, id: &ShortId) -> Result<Vec<Criterion>, StoreError> {
    let ticket = store::ticket(tx, id)?;
    criteria_of(tx, ticket.id)
}

/// The checks of one criterion in the order they were added. Inputs: the transaction, the
/// ticket and the position. Output: the checks, or `ticket_unknown` or `criterion_unknown`.
pub fn checks(tx: &Transaction<'_>, id: &ShortId, position: u32) -> Result<Vec<Check>, StoreError> {
    let ticket = store::ticket(tx, id)?;
    let criterion = criterion_at(tx, &ticket, position)?;
    checks_of(tx, criterion.id)
}

/// The latest evidence recorded for a check, which a host's evidence follows for the check
/// kinds it does not decide itself. Inputs: the transaction and the check. Output: the
/// latest record or none, or `ticket_unknown`, `criterion_unknown` or `check_unknown`.
pub fn latest_evidence(
    tx: &Transaction<'_>,
    view: &CheckView,
) -> Result<Option<Evidence>, StoreError> {
    let ticket = store::ticket(tx, &view.ticket)?;
    let criterion = criterion_at(tx, &ticket, view.criterion)?;
    let check = check_of(tx, &criterion, view.kind, &view.reference)?;
    let latest = latest_of(tx, check.id)?;
    assert!(latest.as_ref().is_none_or(|row| row.check == check.id));
    Ok(latest)
}

/// Every check of a project's tickets, in the order they were added. Inputs: the transaction
/// and the project's row id. Output: the checks.
pub(crate) fn project_checks(tx: &Transaction<'_>, project: i64) -> Result<Vec<Check>, StoreError> {
    Ok(store::prepare(tx, CHECKS_OF_PROJECT)?
        .query_map([project], check_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Every evidence record of a project's checks, in the order they were recorded. Inputs:
/// the transaction and the project's row id. Output: the records.
pub(crate) fn project_evidence(
    tx: &Transaction<'_>,
    project: i64,
) -> Result<Vec<Evidence>, StoreError> {
    Ok(store::prepare(tx, EVIDENCE_OF_PROJECT)?
        .query_map([project], evidence_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Re-derives a ticket's criteria from their checks, one position or all, and closes the
/// ticket as `host:evidence` when `close = evidence` and every criterion is met or waived.
/// A ticket with no criteria never closes on evidence.
fn evaluate(
    tx: &Transaction<'_>,
    host: &impl Host,
    ticket: &Ticket,
    scope: Option<u32>,
    proof: &Proof<'_>,
    now_ms: i64,
) -> Result<Ticket, StoreError> {
    assert!(
        !ticket.status.is_closed(),
        "only open tickets are evaluated"
    );
    let evidence = Actor::host(EVIDENCE_SOURCE);
    let criteria = criteria_of(tx, ticket.id)?;
    assert!(scope.is_none_or(|position| criteria.iter().any(|c| c.position == position)));
    let mut states = Vec::with_capacity(criteria.len());
    for criterion in &criteria {
        let state = if scope.is_none_or(|position| position == criterion.position) {
            derive(tx, host, ticket, criterion, &evidence, now_ms)?
        } else {
            criterion.state
        };
        states.push(state);
    }
    let current = store::ticket(tx, &ticket.short_id)?;
    let policy = policy::load(tx, ticket.short_id.key())?;
    let proven = !states.is_empty() && states.iter().all(|state| *state != CriterionState::Open);
    if policy.close != ClosePolicy::Evidence || !proven {
        return Ok(current);
    }
    let reason = format!(
        "closed on evidence: {total} of {total} criteria met or waived; last proof: {}",
        proof.describe(),
        total = states.len(),
    );
    assert!(reason.len() <= CLOSE_REASON_BYTES_MAX);
    let change = TicketChange {
        status: Some(TicketStatus::Done),
        close_reason: Some(Some(reason)),
        ..TicketChange::default()
    };
    let closed = store::update_ticket(
        tx,
        &evidence,
        &ticket.short_id,
        current.version,
        &change,
        now_ms,
    )?;
    assert_eq!(closed.status, TicketStatus::Done);
    assert_eq!(closed.closed_by.as_ref(), Some(&evidence));
    Ok(closed)
}

/// One criterion's state from its checks: waived stays waived, a criterion without checks
/// keeps its hand-set state, otherwise met exactly when the host finds every check
/// satisfied. A changed state is written as `host:evidence`.
fn derive(
    tx: &Transaction<'_>,
    host: &impl Host,
    ticket: &Ticket,
    criterion: &Criterion,
    evidence: &Actor,
    now_ms: i64,
) -> Result<CriterionState, StoreError> {
    assert!(evidence.is_host());
    if criterion.state == CriterionState::Waived {
        return Ok(CriterionState::Waived);
    }
    let checks = checks_of(tx, criterion.id)?;
    if checks.is_empty() {
        return Ok(criterion.state);
    }
    let mut satisfied = true;
    for check in &checks {
        let view = view_of(ticket, criterion, check);
        let found = host
            .check(tx, ticket.short_id.key(), &view)
            .map_err(StoreError::Refused)?;
        if !found.satisfied {
            satisfied = false;
            break;
        }
    }
    let state = if satisfied {
        CriterionState::Met
    } else {
        CriterionState::Open
    };
    if state != criterion.state {
        let set = store::set_criterion(
            tx,
            evidence,
            &ticket.short_id,
            criterion.position,
            criterion.version,
            state,
            now_ms,
        )?;
        assert_eq!(set.state, state);
    }
    Ok(state)
}

/// Whether a hand `met` would be the actor's own word standing in for proof: an agent under
/// `close = evidence`. The owner and the host's engine actors are trusted to say so.
fn self_certified(tx: &Transaction<'_>, actor: &Actor, id: &ShortId) -> Result<bool, StoreError> {
    let agent = policy::role(actor) == Role::Agent;
    let evidence = policy::load(tx, id.key())?.close == ClosePolicy::Evidence;
    Ok(agent && evidence)
}

/// Whether a criterion with checks may move by hand from one state to another: to waived,
/// from waived back to open, or to the state it already has.
fn by_hand_with_checks(before: CriterionState, after: CriterionState) -> bool {
    after == before
        || after == CriterionState::Waived
        || (before == CriterionState::Waived && after == CriterionState::Open)
}

fn open_ticket(
    tx: &Transaction<'_>,
    host: &impl Host,
    actor: &Actor,
    op: OpKind,
    id: &ShortId,
) -> Result<Ticket, StoreError> {
    let ticket = store::ticket(tx, id)?;
    policy::authorize(tx, host, actor, op, &ticket)?;
    if ticket.status.is_closed() {
        return Err(store::refusal(
            codes::TICKET_CLOSED,
            format!("{id} is closed; reopen it first"),
        ));
    }
    Ok(ticket)
}

fn criterion_at(
    tx: &Transaction<'_>,
    ticket: &Ticket,
    position: u32,
) -> Result<Criterion, StoreError> {
    let found = store::prepare(tx, store::CRITERION_BY_POSITION)?
        .query_row(params![ticket.id, position], store::criterion_row)
        .optional()?
        .ok_or_else(|| {
            store::refusal(
                codes::CRITERION_UNKNOWN,
                format!("{} has no criterion {position}", ticket.short_id),
            )
        })?;
    assert_eq!(found.ticket, ticket.id);
    Ok(found)
}

/// A ticket's criteria by position. Inputs: the transaction and the ticket's row id.
/// Output: the criteria.
pub(crate) fn criteria_of(tx: &Transaction<'_>, ticket: i64) -> Result<Vec<Criterion>, StoreError> {
    let criteria = store::prepare(tx, CRITERIA_OF_TICKET)?
        .query_map([ticket], store::criterion_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert!(criteria.len() <= CRITERIA_PER_TICKET_MAX);
    Ok(criteria)
}

/// A criterion's checks in the order they were added. Inputs: the transaction and the
/// criterion's row id. Output: the checks.
pub(crate) fn checks_of(tx: &Transaction<'_>, criterion: i64) -> Result<Vec<Check>, StoreError> {
    let checks = store::prepare(tx, CHECKS_OF_CRITERION)?
        .query_map([criterion], check_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert!(checks.len() <= CHECKS_PER_CRITERION_MAX);
    Ok(checks)
}

fn check_of(
    tx: &Transaction<'_>,
    criterion: &Criterion,
    kind: CheckKind,
    reference: &str,
) -> Result<Check, StoreError> {
    checks_of(tx, criterion.id)?
        .into_iter()
        .find(|check| check.kind == kind && check.reference == reference)
        .ok_or_else(|| {
            store::refusal(
                codes::CHECK_UNKNOWN,
                format!("criterion {} has no such {kind} check", criterion.position),
            )
        })
}

fn view_of(ticket: &Ticket, criterion: &Criterion, check: &Check) -> CheckView {
    assert_eq!(
        criterion.ticket, ticket.id,
        "the criterion is on this ticket"
    );
    assert_eq!(
        check.criterion, criterion.id,
        "the check proves this criterion"
    );
    CheckView {
        ticket: ticket.short_id.clone(),
        criterion: criterion.position,
        kind: check.kind,
        reference: check.reference.clone(),
    }
}

/// The latest evidence of one check. Inputs: the transaction and the check's row id.
/// Output: the latest record, or none.
pub(crate) fn latest_of(tx: &Transaction<'_>, check: i64) -> Result<Option<Evidence>, StoreError> {
    Ok(store::prepare(tx, EVIDENCE_LATEST)?
        .query_row([check], evidence_row)
        .optional()?)
}

fn check_row(row: &Row<'_>) -> rusqlite::Result<Check> {
    Ok(Check {
        id: row.get(0)?,
        criterion: row.get(1)?,
        kind: store::named(row, 2, CheckKind::parse)?,
        reference: row.get(3)?,
    })
}

fn evidence_row(row: &Row<'_>) -> rusqlite::Result<Evidence> {
    Ok(Evidence {
        id: row.get(0)?,
        check: row.get(1)?,
        result: store::named(row, 2, EvidenceResult::parse)?,
        reference: row.get(3)?,
        recorded_by: store::actor(row, 4)?,
        recorded_ms: row.get(5)?,
    })
}

/// One criterion as a reply shows it, as `show` lists it without its checks. Input: the
/// criterion. Output: its position, text, state and row version.
fn criterion_json(criterion: &Criterion) -> Value {
    assert!(criterion.position >= 1);
    json!({
        "position": criterion.position,
        "text": criterion.text,
        "state": criterion.state.as_str(),
        "version": criterion.version,
    })
}

/// The criterion position a request names, which the catalog bounds from 1. Input: the
/// context. Output: the position.
fn position<H: Host>(cx: &Cx<'_, '_, H>) -> u32 {
    let position = cx.args.need::<i64>("position");
    let position = u32::try_from(position).expect("the catalog bounds a position");
    assert!((1..=CRITERIA_PER_TICKET_MAX as u32).contains(&position));
    position
}

/// The reply to a write on a criterion: the data and the next actions of its ticket as it
/// stands after the write. Inputs: the context and the data. Output: the outcome.
fn criterion_written<H: Host>(cx: &Cx<'_, '_, H>, data: Value) -> Result<ops::Outcome, Failure> {
    assert!(data.is_object());
    let ticket = store::ticket(cx.tx, &cx.ticket().short_id)?;
    assert_eq!(ticket.id, cx.ticket().id);
    Ok(ops::Outcome {
        data,
        next: ops::ticket_next(cx.tx, &ticket)?,
        cursor: None,
    })
}

/// Refuses a criterion write unless it carries the version of the criterion this
/// transaction read, as `ops::fresh` does for a ticket; a position with no criterion is left
/// to the store's refusal. Inputs: the context, the position and the version sent. Output:
/// Ok, or `stale_version` naming the current version with `show` next.
fn criterion_fresh<H: Host>(cx: &Cx<'_, '_, H>, at: u32, version: i64) -> Result<(), Failure> {
    let ticket = cx.ticket();
    let criteria = criteria_of(cx.tx, ticket.id)?;
    assert!(criteria.len() <= CRITERIA_PER_TICKET_MAX);
    let Some(found) = criteria
        .into_iter()
        .find(|criterion| criterion.position == at)
    else {
        return Ok(());
    };
    if found.version == version {
        return Ok(());
    }
    Err(Failure::new(
        codes::STALE_VERSION,
        format!(
            "criterion {at} of {} is at version {}, not {version}; show it and retry",
            ticket.short_id, found.version
        ),
    )
    .with_data(json!({
        "id": ticket.short_id.to_string(),
        "position": at,
        "version": found.version,
    }))
    .with_next(vec![Candidate::ticket(
        OpKind::Show,
        ticket,
        "read the criterion's current version, then retry",
    )]))
}

/// Runs `criterion_add` for dispatch. Input: the context. Output: the ticket's id and the
/// new criterion.
pub(crate) fn criterion_add<H: Host>(cx: &Cx<'_, '_, H>) -> Result<ops::Outcome, Failure> {
    let id = &cx.ticket().short_id;
    let text = cx.args.need::<&str>("text");
    let added = add_criterion(cx.tx, cx.host, cx.actor, id, text, cx.at_ms)?;
    assert_eq!(added.ticket, cx.ticket().id);
    assert_eq!(added.text, text);
    criterion_written(
        cx,
        json!({"id": id.to_string(), "criterion": criterion_json(&added)}),
    )
}

/// Runs `criterion_edit` for dispatch. Input: the context. Output: the ticket's id and the
/// criterion as rewritten.
pub(crate) fn criterion_edit<H: Host>(cx: &Cx<'_, '_, H>) -> Result<ops::Outcome, Failure> {
    let id = &cx.ticket().short_id;
    let (at, version) = (position(cx), cx.args.need::<i64>("version"));
    let text = cx.args.need::<&str>("text");
    criterion_fresh(cx, at, version)?;
    let edited = edit_criterion(cx.tx, cx.host, cx.actor, id, at, version, text, cx.at_ms)?;
    assert_eq!(edited.position, at);
    assert!(edited.version >= version);
    criterion_written(
        cx,
        json!({"id": id.to_string(), "criterion": criterion_json(&edited)}),
    )
}

/// Runs `check_add` for dispatch. Input: the context. Output: the ticket's id, the
/// criterion's position and the new check.
pub(crate) fn check_add<H: Host>(cx: &Cx<'_, '_, H>) -> Result<ops::Outcome, Failure> {
    let id = &cx.ticket().short_id;
    let at = position(cx);
    let kind = cx.args.need::<CheckKind>("kind");
    let reference = cx.args.need::<&str>("ref");
    let check = add_check(cx.tx, cx.host, cx.actor, id, at, kind, reference, cx.at_ms)?;
    assert_eq!(check.kind, kind);
    assert_eq!(check.reference, reference);
    criterion_written(
        cx,
        json!({
            "id": id.to_string(),
            "position": at,
            "check": {"kind": check.kind.as_str(), "ref": check.reference},
        }),
    )
}

/// Runs `evidence_record` for dispatch. Input: the context. Output: the ticket's id, the
/// criterion's position, the evidence, and the criterion and ticket after it.
pub(crate) fn evidence_record<H: Host>(cx: &Cx<'_, '_, H>) -> Result<ops::Outcome, Failure> {
    let id = &cx.ticket().short_id;
    let new = NewEvidence {
        ticket: id.clone(),
        criterion: position(cx),
        kind: cx.args.need::<CheckKind>("kind"),
        check: cx.args.need::<&str>("ref").to_owned(),
        result: cx.args.need::<EvidenceResult>("result"),
        reference: cx.args.get::<&str>("evidence").map(str::to_owned),
    };
    let (evidence, outcome) = record_evidence(cx.tx, cx.host, cx.actor, &new, cx.at_ms)?;
    assert_eq!(evidence.result, new.result);
    assert_eq!(outcome.criterion.position, new.criterion);
    criterion_written(
        cx,
        json!({
            "id": id.to_string(),
            "position": new.criterion,
            "evidence": {
                "result": evidence.result.as_str(),
                "evidence": evidence.reference,
                "by": evidence.recorded_by.as_str(),
                "at_ms": evidence.recorded_ms,
            },
            "criterion": criterion_json(&outcome.criterion),
            "ticket": ops::ticket_json(cx.tx, &outcome.ticket)?,
        }),
    )
}

/// Runs `criterion_set` for dispatch. Input: the context. Output: the ticket's id, the
/// criterion's position, and the criterion and ticket after it.
pub(crate) fn criterion_set<H: Host>(cx: &Cx<'_, '_, H>) -> Result<ops::Outcome, Failure> {
    let id = &cx.ticket().short_id;
    let (at, version) = (position(cx), cx.args.need::<i64>("version"));
    let state = cx.args.need::<CriterionState>("state");
    criterion_fresh(cx, at, version)?;
    let outcome = set_criterion(cx.tx, cx.host, cx.actor, id, at, version, state, cx.at_ms)?;
    assert_eq!(outcome.criterion.position, at);
    assert_eq!(outcome.ticket.id, cx.ticket().id);
    criterion_written(
        cx,
        json!({
            "id": id.to_string(),
            "position": at,
            "criterion": criterion_json(&outcome.criterion),
            "ticket": ops::ticket_json(cx.tx, &outcome.ticket)?,
        }),
    )
}
