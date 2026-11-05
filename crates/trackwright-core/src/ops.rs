use crate::catalog::{self, FieldType};
use crate::host::{self, Host, LinkState, TicketView};
use crate::model::{
    Actor, LINKS_PER_TICKET_MAX, OPEN_TICKETS_CREATED_PER_ACTOR_MAX, OpKind, Priority, ProjectKey,
    Refusal, ShortId, Ticket, TicketKind, TicketStatus, WRITES_PER_ACTOR_MINUTE_MAX, codes,
};
use crate::policy;
use crate::reply::{
    self, LISTED_TITLE_BYTES_MAX, NEXT_MAX, Next, PAGE_ROWS_MAX, Page, Reply, WHY_BYTES_MAX,
};
use crate::requests::{self, Args, Remembered, Request};
use crate::store::{self, NewTicket, StoreError, TicketChange};
use crate::tests_model::TestId;
use crate::{criteria, export, import, plan, ready};
use rusqlite::{Transaction, params};
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

mod test_ops;

pub const WRITE_WINDOW_MS: i64 = 60_000;
pub const SHOWN_EVENTS_MAX: usize = 10;
pub const SHOWN_REF_BYTES_MAX: usize = 80;
pub const SHOWN_LABEL_BYTES_MAX: usize = 80;
pub const SHOWN_STATE_BYTES_MAX: usize = 40;

const PRIORITY_DEFAULT: u8 = 2;
const IGNORED_FIELDS: &[&str] = &[
    "id",
    "project",
    "ticket",
    "criterion",
    "check",
    "version",
    "updated_ms",
];

pub(crate) const WRITES_RECENT: &str = "SELECT count(*) FROM (SELECT 1 FROM tk_events \
     WHERE actor = ?1 AND at_ms > ?2 LIMIT ?3)";
pub(crate) const OPEN_CREATED: &str = "SELECT count(*) FROM (SELECT 1 FROM tk_tickets \
     WHERE created_by = ?1 AND status NOT IN ('done', 'cancelled') LIMIT ?2)";
pub(crate) const CRITERIA_TALLY: &str =
    "SELECT count(*), coalesce(sum(state = 'open'), 0) FROM tk_criteria WHERE ticket = ?1";
pub(crate) const LIST_PAGE: &str = "SELECT t.number, t.title, t.status, t.priority, t.assignee \
     FROM tk_tickets t WHERE t.project = ?1 AND t.number > ?2 \
     AND (?3 IS NULL OR t.status = ?3) AND (?4 IS NULL OR t.kind = ?4) \
     AND (?5 IS NULL OR t.assignee = ?5) ORDER BY t.number LIMIT ?6";
pub(crate) const CRITERIA_FROM: &str = "SELECT id, position, text, state, version \
     FROM tk_criteria WHERE ticket = ?1 AND position >= ?2 ORDER BY position";
pub(crate) const CHECKS_OF: &str =
    "SELECT id, kind, ref FROM tk_checks WHERE criterion = ?1 ORDER BY id";
pub(crate) const EVIDENCE_LATEST: &str = "SELECT result, ref, recorded_by, recorded_ms \
     FROM tk_evidence WHERE check_id = ?1 ORDER BY id DESC LIMIT 1";
pub(crate) const LINKS_FROM: &str = "SELECT kind, target FROM tk_links WHERE ticket = ?1 \
     ORDER BY kind, target LIMIT ?2 OFFSET ?3";
pub(crate) const EVENTS_RECENT: &str = "SELECT at_ms, actor, op, before_json, after_json \
     FROM tk_events WHERE ticket = ?1 ORDER BY id DESC LIMIT ?2";
pub(crate) const EVENTS_FROM: &str = "SELECT at_ms, actor, op, before_json, after_json \
     FROM tk_events WHERE ticket = ?1 ORDER BY id LIMIT ?2 OFFSET ?3";
pub(crate) const EVENTS_COUNT: &str = "SELECT count(*) FROM tk_events WHERE ticket = ?1";

/// One next action a handler proposes, with what the host's authority needs to decide
/// whether this caller may take it.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    next: Next,
    project: ProjectKey,
    ticket: Option<TicketView>,
}

impl Candidate {
    /// Proposes an operation on a ticket. Inputs: the operation, the ticket and why.
    /// Output: the candidate.
    pub(crate) fn ticket(op: OpKind, ticket: &Ticket, why: impl Into<String>) -> Self {
        let why = why.into();
        assert!(!why.is_empty() && why.len() <= WHY_BYTES_MAX);
        Self {
            next: Next {
                op,
                id: Some(ticket.short_id.clone()),
                why,
            },
            project: ticket.short_id.key().clone(),
            ticket: Some(TicketView::of(ticket)),
        }
    }

    /// Proposes an operation on a project. Inputs: the operation, the project and why.
    /// Output: the candidate.
    pub(crate) fn project(op: OpKind, project: &ProjectKey, why: impl Into<String>) -> Self {
        let why = why.into();
        assert!(!why.is_empty() && why.len() <= WHY_BYTES_MAX);
        Self {
            next: Next { op, id: None, why },
            project: project.clone(),
            ticket: None,
        }
    }
}

/// What a handler produced: the reply data, the next actions it proposes and the cursor of
/// the next page.
#[derive(Debug)]
pub(crate) struct Outcome {
    pub data: Value,
    pub next: Vec<Candidate>,
    pub cursor: Option<String>,
}

/// Why an operation was refused: a code, one line, optional detail and the next actions it
/// proposes. Every refusal rolls the operation back.
#[derive(Debug)]
pub(crate) struct Failure {
    code: &'static str,
    message: String,
    data: Option<Value>,
    next: Vec<Candidate>,
}

impl Failure {
    /// Builds a refusal. Inputs: the code and the message. Output: the failure.
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
            next: Vec::new(),
        }
    }

    /// Adds detail the caller acts on. Input: a JSON object. Output: the failure.
    pub(crate) fn with_data(mut self, data: Value) -> Self {
        assert!(data.is_object());
        self.data = Some(data);
        self
    }

    /// Adds next actions. Input: the candidates. Output: the failure.
    pub(crate) fn with_next(mut self, next: Vec<Candidate>) -> Self {
        self.next = next;
        self
    }
}

impl From<Refusal> for Failure {
    fn from(refusal: Refusal) -> Self {
        Self::new(refusal.code, refusal.message)
    }
}

impl From<StoreError> for Failure {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Refused(refusal) => refusal.into(),
            StoreError::Sqlite(_) | StoreError::Corrupt(_) => Self::new(
                codes::STORE_FAILED,
                "the tracker store failed; nothing changed",
            ),
        }
    }
}

impl From<rusqlite::Error> for Failure {
    fn from(error: rusqlite::Error) -> Self {
        StoreError::from(error).into()
    }
}

/// What one handler sees: the transaction, the host, the caller, the checked arguments,
/// the project and ticket the request names, the time and the reply data budget.
pub(crate) struct Cx<'a, 't, H: Host> {
    pub tx: &'a Transaction<'t>,
    pub host: &'a H,
    pub actor: &'a Actor,
    pub kind: OpKind,
    pub args: &'a Args,
    pub project: Option<&'a ProjectKey>,
    pub ticket: Option<&'a Ticket>,
    pub at_ms: i64,
    pub data_bytes_max: usize,
}

impl<H: Host> Cx<'_, '_, H> {
    /// The project the request names, which every operation but `schema` has. Output: it.
    pub(crate) fn project(&self) -> &ProjectKey {
        match self.project {
            Some(project) => project,
            None => panic!("{} names a project", self.kind),
        }
    }

    /// The ticket the request names by `id`. Output: it, as read in this transaction.
    pub(crate) fn ticket(&self) -> &Ticket {
        match self.ticket {
            Some(ticket) => ticket,
            None => panic!("{} names a ticket", self.kind),
        }
    }
}

/// Runs one request inside the caller's transaction and replies with the envelope. A write
/// that repeats a request id with the same fields gets its first reply and writes nothing;
/// any refusal leaves the store as it was. Inputs: the transaction, the host, the actor the
/// host vouches for, and the parsed request. Output: the reply, within its bound.
pub fn dispatch<H: Host>(tx: &Transaction<'_>, host: &H, actor: &Actor, request: Request) -> Reply {
    let bound = request.reply_bytes_max();
    let reply = match attempt(tx, host, actor, &request, clock_ms()) {
        Ok(reply) => reply,
        Err(failure) => {
            let next = allowed(tx, host, actor, failure.next);
            Reply::failure(
                failure.code,
                &failure.message,
                failure.data.as_ref(),
                &next,
                bound,
            )
        }
    };
    assert!(reply.json().len() <= bound);
    reply
}

fn attempt<H: Host>(
    tx: &Transaction<'_>,
    host: &H,
    actor: &Actor,
    request: &Request,
    at_ms: i64,
) -> Result<Reply, Failure> {
    match requests::remembered(tx, actor, request)? {
        Remembered::Repeat(reply) => {
            if reply.json().len() > request.reply_bytes_max() {
                return Err(Failure::new(
                    codes::REQUEST_ID_REUSED,
                    "this request id's first reply is over this call's reply bound; repeat it \
                     with the reply bound of the first call",
                ));
            }
            return Ok(reply);
        }
        Remembered::Reused => {
            return Err(Failure::new(
                codes::REQUEST_ID_REUSED,
                "this request id named a write with other fields; use a new request id",
            ));
        }
        Remembered::New => {}
    }
    savepoint(tx, || run(tx, host, actor, request, at_ms))
}

fn savepoint<T>(
    tx: &Transaction<'_>,
    body: impl FnOnce() -> Result<T, Failure>,
) -> Result<T, Failure> {
    tx.execute_batch("SAVEPOINT tk_dispatch")?;
    match body() {
        Ok(value) => {
            tx.execute_batch("RELEASE tk_dispatch")?;
            Ok(value)
        }
        Err(failure) => {
            if let Err(rollback) = tx.execute_batch("ROLLBACK TO tk_dispatch; RELEASE tk_dispatch")
            {
                assert!(
                    tx.is_autocommit(),
                    "a refused operation rolls back to its savepoint: {rollback}"
                );
            }
            Err(failure)
        }
    }
}

fn run<H: Host>(
    tx: &Transaction<'_>,
    host: &H,
    actor: &Actor,
    request: &Request,
    at_ms: i64,
) -> Result<Reply, Failure> {
    let kind = request.kind();
    let args = request.args();
    let ticket = target_ticket(tx, args)?;
    let project = target_project(tx, args, ticket.as_ref())?;
    assert!(project.is_some() || kind == OpKind::Schema);
    if let Some(project) = &project {
        let view = ticket.as_ref().map(TicketView::of);
        host.actor_may(tx, project, actor, kind, view.as_ref())?;
    }
    if kind.is_write() {
        within_limits(tx, actor, kind, at_ms)?;
    }
    let bound = request.reply_bytes_max();
    let cx = Cx {
        tx,
        host,
        actor,
        kind,
        args,
        project: project.as_ref(),
        ticket: ticket.as_ref(),
        at_ms,
        data_bytes_max: reply::data_bytes_max(bound),
    };
    let outcome = handle(&cx)?;
    let next = allowed(tx, host, actor, outcome.next);
    let reply = Reply::success(&outcome.data, &next, outcome.cursor.as_deref(), bound);
    if kind.is_write() {
        requests::remember(tx, actor, request, &reply, at_ms)?;
    }
    Ok(reply)
}

fn handle<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    match cx.kind {
        OpKind::List => list(cx),
        OpKind::Show => show(cx),
        OpKind::History => history(cx),
        OpKind::Schema => schema(cx),
        OpKind::Create => create(cx),
        OpKind::Edit => edit(cx),
        OpKind::Move => move_to(cx),
        OpKind::Ready => ready::ready(cx),
        OpKind::Roadmap => ready::roadmap(cx),
        OpKind::Claim => plan::claim(cx),
        OpKind::Release => plan::release(cx),
        OpKind::Depend => plan::depend(cx),
        OpKind::Undepend => plan::undepend(cx),
        OpKind::VersionUpsert => plan::version_upsert(cx),
        OpKind::CycleStart => plan::cycle_start(cx),
        OpKind::CycleClose => plan::cycle_close(cx),
        OpKind::Link => link(cx),
        OpKind::Unlink => unlink(cx),
        OpKind::PolicyGet => policy::policy_get(cx),
        OpKind::CriterionAdd => criteria::criterion_add(cx),
        OpKind::CriterionEdit => criteria::criterion_edit(cx),
        OpKind::CheckAdd => criteria::check_add(cx),
        OpKind::EvidenceRecord => criteria::evidence_record(cx),
        OpKind::CriterionSet => criteria::criterion_set(cx),
        OpKind::Close => policy::close_ticket(cx),
        OpKind::Reopen => policy::reopen_ticket(cx),
        OpKind::PolicySet => policy::policy_set(cx),
        OpKind::Export => export::export(cx),
        OpKind::ImportPreview => import::import_preview(cx),
        OpKind::ImportApply => import::import_apply(cx),
        OpKind::CaseList => test_ops::case_list(cx),
        OpKind::CaseShow => test_ops::case_show(cx),
        OpKind::SetShow => test_ops::set_show(cx),
        OpKind::PlanList => test_ops::plan_list(cx),
        OpKind::PlanShow => test_ops::plan_show(cx),
        OpKind::RunList => test_ops::run_list(cx),
        OpKind::RunShow => test_ops::run_show(cx),
        OpKind::Coverage => test_ops::coverage(cx),
        OpKind::CaseCreate => test_ops::case_create(cx),
        OpKind::CaseEdit => test_ops::case_edit(cx),
        OpKind::CaseLink => test_ops::case_link(cx),
        OpKind::CaseUnlink => test_ops::case_unlink(cx),
        OpKind::SetUpsert => test_ops::set_upsert(cx),
        OpKind::SetAdd => test_ops::set_add(cx),
        OpKind::SetRemove => test_ops::set_remove(cx),
        OpKind::PlanUpsert => test_ops::plan_upsert(cx),
        OpKind::PlanAdd => test_ops::plan_add(cx),
        OpKind::PlanRemove => test_ops::plan_remove(cx),
        OpKind::RunStart => test_ops::run_start(cx),
        OpKind::RunRecord => test_ops::run_record(cx),
        OpKind::RunFinish => test_ops::run_finish(cx),
        OpKind::RunCancel => test_ops::run_cancel(cx),
    }
}

/// Keeps reply data that has no pages within the caller's reply budget. Inputs: the
/// context, the data, the code of the refusal and what the caller does instead. Output: the
/// data, or the refusal naming its size.
pub(crate) fn whole<H: Host>(
    cx: &Cx<'_, '_, H>,
    data: Value,
    code: &'static str,
    instead: &str,
) -> Result<Value, Failure> {
    assert!(data.is_object());
    assert!(!instead.is_empty());
    let bytes = data.to_string().len();
    if bytes > cx.data_bytes_max {
        return Err(Failure::new(
            code,
            format!(
                "{} data is {bytes} bytes, more than the {} one reply holds; {instead}",
                cx.kind, cx.data_bytes_max
            ),
        ));
    }
    Ok(data)
}

fn clock_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

fn target_ticket(tx: &Transaction<'_>, args: &Args) -> Result<Option<Ticket>, Failure> {
    if args.field_type("id") != Some(FieldType::Ticket) {
        return Ok(None);
    }
    match args.get::<&ShortId>("id") {
        Some(id) => Ok(Some(store::ticket(tx, id)?)),
        None => Ok(None),
    }
}

fn target_test(args: &Args) -> Option<&TestId> {
    match args.field_type("id") {
        Some(FieldType::Test(_)) => args.get::<&TestId>("id"),
        _ => None,
    }
}

fn target_project(
    tx: &Transaction<'_>,
    args: &Args,
    ticket: Option<&Ticket>,
) -> Result<Option<ProjectKey>, Failure> {
    let named = if args.has_field("project") {
        args.get::<&ProjectKey>("project")
    } else {
        None
    };
    let tested = target_test(args);
    assert!(
        ticket.is_none() || tested.is_none(),
        "an id is a ticket or a test id"
    );
    match (named, ticket, tested) {
        (Some(named), Some(ticket), _) if named != ticket.short_id.key() => Err(Failure::new(
            codes::FIELD_INVALID,
            format!("{} is not a ticket of project {named}", ticket.short_id),
        )),
        (_, Some(ticket), _) => Ok(Some(ticket.short_id.key().clone())),
        (Some(named), None, Some(id)) if named != id.key() => Err(Failure::new(
            codes::FIELD_INVALID,
            format!("{id} is not of project {named}"),
        )),
        (_, None, Some(id)) => {
            store::project(tx, id.key())?;
            Ok(Some(id.key().clone()))
        }
        (Some(named), None, None) => {
            store::project(tx, named)?;
            Ok(Some(named.clone()))
        }
        (None, None, None) => Ok(None),
    }
}

fn within_limits(
    tx: &Transaction<'_>,
    actor: &Actor,
    kind: OpKind,
    at_ms: i64,
) -> Result<(), Failure> {
    let writes_max = WRITES_PER_ACTOR_MINUTE_MAX as i64;
    let recent: i64 = store::prepare(tx, WRITES_RECENT)?.query_row(
        params![actor.as_str(), at_ms - WRITE_WINDOW_MS, writes_max],
        |row| row.get(0),
    )?;
    assert!((0..=writes_max).contains(&recent));
    if recent >= writes_max {
        return Err(Failure::new(
            codes::RATE_LIMITED,
            format!("{actor} made {writes_max} writes in the last minute; wait and retry"),
        ));
    }
    if kind != OpKind::Create {
        return Ok(());
    }
    let open_max = OPEN_TICKETS_CREATED_PER_ACTOR_MAX as i64;
    let open: i64 = store::prepare(tx, OPEN_CREATED)?
        .query_row(params![actor.as_str(), open_max], |row| row.get(0))?;
    assert!((0..=open_max).contains(&open));
    if open >= open_max {
        return Err(Failure::new(
            codes::OPEN_TICKETS_FULL,
            format!("{actor} created {open_max} tickets that are still open; close some first"),
        ));
    }
    Ok(())
}

fn allowed<H: Host>(
    tx: &Transaction<'_>,
    host: &H,
    actor: &Actor,
    candidates: Vec<Candidate>,
) -> Vec<Next> {
    let mut next: Vec<Next> = Vec::with_capacity(NEXT_MAX);
    for candidate in candidates {
        if next.len() == NEXT_MAX {
            break;
        }
        let listed = next
            .iter()
            .any(|kept| kept.op == candidate.next.op && kept.id == candidate.next.id);
        let may = host
            .actor_may(
                tx,
                &candidate.project,
                actor,
                candidate.next.op,
                candidate.ticket.as_ref(),
            )
            .is_ok();
        if !listed && may {
            next.push(candidate.next);
        }
    }
    assert!(next.len() <= NEXT_MAX);
    next
}

/// The next actions a ticket's state suggests, most useful first; `allowed` keeps those the
/// caller may run. Inputs: the transaction and the ticket. Output: the candidates.
pub(crate) fn ticket_next(
    tx: &Transaction<'_>,
    ticket: &Ticket,
) -> Result<Vec<Candidate>, StoreError> {
    let (criteria, open): (i64, i64) = store::prepare(tx, CRITERIA_TALLY)?
        .query_row([ticket.id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    assert!((0..=criteria).contains(&open));
    let mut next = Vec::new();
    let mut add = |op: OpKind, why: String| next.push(Candidate::ticket(op, ticket, why));
    if ticket.status.is_closed() {
        add(
            OpKind::Reopen,
            "closed; reopen it to work on it again".into(),
        );
        add(OpKind::Show, "read it in full".into());
        add(OpKind::History, "read who changed what".into());
        return Ok(next);
    }
    if ticket.assignee.is_none() {
        add(OpKind::Claim, "unassigned; claim it to work on it".into());
    }
    if criteria == 0 {
        add(OpKind::CriterionAdd, "no acceptance criteria yet".into());
    }
    if open > 0 {
        add(
            OpKind::CriterionSet,
            format!("{open} of {criteria} criteria open"),
        );
    }
    match ticket.status {
        TicketStatus::InReview => add(OpKind::Close, "in review; close it once accepted".into()),
        TicketStatus::InProgress => {
            add(
                OpKind::Move,
                "move it to in_review when the work is done".into(),
            );
        }
        _ => add(OpKind::Move, "move it to in_progress to start".into()),
    }
    if criteria > 0 && open == 0 && ticket.status != TicketStatus::InReview {
        add(OpKind::Close, "every criterion is met".into());
    }
    add(
        OpKind::Edit,
        "change its title, body, kind or priority".into(),
    );
    add(OpKind::Link, "link its branch, change or notes".into());
    add(OpKind::Depend, "record what it waits for".into());
    add(OpKind::Show, "read it in full".into());
    Ok(next)
}

/// The ticket fields a reply carries, its body as a byte count, its placement by name and
/// the open tickets it waits for. Inputs: the transaction and the ticket. Output: the JSON
/// object.
pub(crate) fn ticket_json(tx: &Transaction<'_>, ticket: &Ticket) -> Result<Value, Failure> {
    let (parent, release, cycle) = ready::placement_names(tx, ticket)?;
    Ok(json!({
        "id": ticket.short_id.to_string(),
        "version": ticket.version,
        "title": ticket.title,
        "body_bytes": ticket.body.len(),
        "kind": ticket.kind.as_str(),
        "status": ticket.status.as_str(),
        "priority": ticket.priority.get(),
        "assignee": ticket.assignee.as_ref().map(Actor::as_str),
        "created_by": ticket.created_by.as_str(),
        "created_ms": ticket.created_ms,
        "updated_ms": ticket.updated_ms,
        "closed_ms": ticket.closed_ms,
        "closed_by": ticket.closed_by.as_ref().map(Actor::as_str),
        "close_reason": ticket.close_reason,
        "parent": parent,
        "release": release,
        "cycle": cycle,
        "blocked_by": ready::blocked_by(tx, ticket)?,
    }))
}

/// Refuses a write unless it carries the version this transaction read. Inputs: the
/// context and the ticket. Output: the version, or `stale_version` naming the current one
/// with `show` as the next action.
pub(crate) fn fresh<H: Host>(cx: &Cx<'_, '_, H>, ticket: &Ticket) -> Result<i64, Failure> {
    let version = cx.args.need::<i64>("version");
    if version == ticket.version {
        return Ok(version);
    }
    Err(Failure::new(
        codes::STALE_VERSION,
        format!(
            "{} is at version {}, not {version}; show it and retry",
            ticket.short_id, ticket.version
        ),
    )
    .with_data(json!({"id": ticket.short_id.to_string(), "version": ticket.version}))
    .with_next(vec![Candidate::ticket(
        OpKind::Show,
        ticket,
        "read the current version, then retry",
    )]))
}

/// The reply to a write on a ticket: the ticket and the next actions its state suggests.
/// Inputs: the context and the ticket as stored. Output: the outcome.
pub(crate) fn written<H: Host>(cx: &Cx<'_, '_, H>, ticket: &Ticket) -> Result<Outcome, Failure> {
    Ok(Outcome {
        data: ticket_json(cx.tx, ticket)?,
        next: ticket_next(cx.tx, ticket)?,
        cursor: None,
    })
}

fn priority(value: i64) -> Result<Priority, Failure> {
    let value = u8::try_from(value)
        .map_err(|_| Failure::new(codes::PRIORITY_INVALID, "a priority is 0 to 4"))?;
    Ok(Priority::new(value)?)
}

fn create<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let placement = plan::placement(cx, None)?;
    let new = NewTicket {
        title: cx.args.need::<&str>("title").to_owned(),
        body: cx.args.get::<&str>("body").unwrap_or_default().to_owned(),
        kind: cx
            .args
            .get::<TicketKind>("kind")
            .unwrap_or(TicketKind::Feature),
        status: start_status(cx)?,
        priority: match cx.args.get::<i64>("priority") {
            Some(value) => priority(value)?,
            None => Priority::new(PRIORITY_DEFAULT)?,
        },
    };
    let ticket =
        store::create_ticket_placed(cx.tx, cx.actor, cx.project(), &new, placement, cx.at_ms)?;
    assert_eq!(ticket.version, 1);
    assert_eq!(ticket.version_id, placement.version_id.flatten());
    written(cx, &ticket)
}

fn start_status<H: Host>(cx: &Cx<'_, '_, H>) -> Result<TicketStatus, Failure> {
    let requested = cx
        .args
        .get::<TicketStatus>("status")
        .unwrap_or(TicketStatus::Backlog);
    if requested.is_closed() {
        return Err(Failure::new(
            codes::STATUS_CLOSED_AT_CREATE,
            format!("{requested} is a closed status; a ticket starts open"),
        ));
    }
    let status = policy::start_status(&policy::load(cx.tx, cx.project())?, cx.actor, requested);
    assert!(!status.is_closed());
    assert!(status == requested || status == TicketStatus::Triage);
    Ok(status)
}

/// Writes a change to a ticket at the version the caller read. Inputs: the context, the
/// ticket as read and the change. Output: the ticket as stored, or `stale_version`.
pub(crate) fn update<H: Host>(
    cx: &Cx<'_, '_, H>,
    ticket: &Ticket,
    change: &TicketChange,
) -> Result<Outcome, Failure> {
    let version = fresh(cx, ticket)?;
    let stored =
        match store::update_ticket(cx.tx, cx.actor, &ticket.short_id, version, change, cx.at_ms) {
            Err(StoreError::Refused(refusal)) if refusal.code == codes::STALE_VERSION => {
                panic!("the version read in this transaction is current: {refusal}")
            }
            other => other?,
        };
    assert!(stored.version == version || stored.version == version + 1);
    written(cx, &stored)
}

fn edit<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let change = TicketChange {
        title: cx.args.get::<&str>("title").map(str::to_owned),
        body: cx.args.get::<&str>("body").map(str::to_owned),
        kind: cx.args.get::<TicketKind>("kind"),
        priority: cx.args.get::<i64>("priority").map(priority).transpose()?,
        placement: plan::placement(cx, Some(cx.ticket()))?,
        ..TicketChange::default()
    };
    update(cx, cx.ticket(), &change)
}

fn move_to<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let ticket = cx.ticket();
    let status = cx.args.need::<TicketStatus>("status");
    if ticket.status.is_closed() {
        return Err(Failure::new(
            codes::TICKET_CLOSED,
            format!("{} is closed; reopen opens it", ticket.short_id),
        )
        .with_next(vec![Candidate::ticket(
            OpKind::Reopen,
            ticket,
            "reopen it to work on it again",
        )]));
    }
    if status.is_closed() {
        return Err(Failure::new(
            codes::USE_CLOSE,
            format!("{status} is a closed status; close sets it"),
        )
        .with_next(vec![Candidate::ticket(
            OpKind::Close,
            ticket,
            "close it as done or cancelled",
        )]));
    }
    let change = TicketChange {
        status: Some(status),
        ..TicketChange::default()
    };
    update(cx, ticket, &change)
}

fn link<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let ticket = cx.ticket();
    let kind = cx.args.need::<&str>("kind");
    let target = cx.args.need::<&str>("target");
    let (added, state) = host::link(
        cx.tx,
        cx.host,
        cx.actor,
        &ticket.short_id,
        kind,
        target,
        cx.at_ms,
    )?;
    assert_eq!(added.kind, kind);
    assert_eq!(added.target, target);
    let mut data = link_json(kind, target, &Ok(state));
    data["id"] = json!(ticket.short_id.to_string());
    Ok(Outcome {
        data,
        next: ticket_next(cx.tx, ticket)?,
        cursor: None,
    })
}

fn unlink<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let ticket = cx.ticket();
    let kind = cx.args.need::<&str>("kind");
    let target = cx.args.need::<&str>("target");
    let removed = host::unlink(
        cx.tx,
        cx.host,
        cx.actor,
        &ticket.short_id,
        kind,
        target,
        cx.at_ms,
    )?;
    assert_eq!(removed.kind, kind);
    assert_eq!(removed.target, target);
    Ok(Outcome {
        data: json!({
            "id": ticket.short_id.to_string(),
            "kind": kind,
            "target": target,
            "removed": true,
        }),
        next: ticket_next(cx.tx, ticket)?,
        cursor: None,
    })
}

fn link_json(kind: &str, target: &str, state: &Result<LinkState, Refusal>) -> Value {
    match state {
        Ok(state) => json!({
            "kind": kind,
            "target": target,
            "live": state.live,
            "label": reply::elide(&state.label, SHOWN_LABEL_BYTES_MAX).0,
            "state": reply::elide(&state.state, SHOWN_STATE_BYTES_MAX).0,
        }),
        Err(refusal) => json!({
            "kind": kind,
            "target": target,
            "live": false,
            "label": "",
            "state": refusal.code,
        }),
    }
}

/// Reads a page cursor `<section>:<at>`. Inputs: the arguments and the section this
/// operation pages. Output: the position, 0 without a cursor, or `cursor_invalid`.
pub(crate) fn cursor_at(args: &Args, section: &str) -> Result<u64, Failure> {
    let Some(cursor) = args.get::<&str>("cursor") else {
        return Ok(0);
    };
    match cursor.split_once(':') {
        Some((named, at)) if named == section => decimal(at).ok_or_else(cursor_invalid),
        _ => Err(cursor_invalid()),
    }
}

fn decimal(text: &str) -> Option<u64> {
    if text.is_empty() || text.len() > 19 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The refusal of a cursor this operation never gave. Output: `cursor_invalid`.
pub(crate) fn cursor_invalid() -> Failure {
    Failure::new(
        codes::CURSOR_INVALID,
        "that cursor is not one this operation gave; read the first page again",
    )
}

fn list<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let project = store::project(cx.tx, cx.project())?;
    let after = cursor_at(cx.args, "tickets")?;
    let after = i64::try_from(after).map_err(|_| cursor_invalid())?;
    let status = cx
        .args
        .get::<TicketStatus>("status")
        .map(TicketStatus::as_str);
    let kind = cx.args.get::<TicketKind>("kind").map(TicketKind::as_str);
    let assignee = cx.args.get::<&Actor>("assignee").map(Actor::as_str);
    let rows: Vec<(i64, String, String, i64, Option<String>)> = store::prepare(cx.tx, LIST_PAGE)?
        .query_map(
            params![
                project.id,
                after,
                status,
                kind,
                assignee,
                PAGE_ROWS_MAX as i64 + 1
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<_>>()?;
    assert!(rows.len() <= PAGE_ROWS_MAX + 1);
    let mut page = Page::new(cx.data_bytes_max);
    assert!(page.put("tickets", json!([])));
    let mut cursor = None;
    for (shown, (number, title, status, priority, assignee)) in rows.iter().enumerate() {
        let (title, elided) = reply::elide(title, LISTED_TITLE_BYTES_MAX);
        let mut row = json!({
            "id": format!("{}-{number}", project.key),
            "title": title,
            "status": status,
            "priority": priority,
            "assignee": assignee,
        });
        if elided {
            row["elided"] = json!(true);
        }
        if shown == PAGE_ROWS_MAX || !page.push("tickets", row) {
            assert!(shown > 0, "one listed row always fits a page");
            cursor = Some(format!("tickets:{}", rows[shown - 1].0));
            break;
        }
    }
    let mut next = Vec::new();
    if let Some((number, ..)) = rows.first() {
        let first = store::ticket(cx.tx, &ShortId::new(project.key.clone(), *number as u32)?)?;
        next.push(Candidate::ticket(
            OpKind::Show,
            &first,
            "read the first listed ticket",
        ));
    }
    next.push(Candidate::project(
        OpKind::Ready,
        &project.key,
        "unblocked work in order",
    ));
    next.push(Candidate::project(
        OpKind::Create,
        &project.key,
        "add a ticket",
    ));
    Ok(Outcome {
        data: page.into_value(),
        next,
        cursor,
    })
}

fn history<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let ticket = cx.ticket();
    let offset = i64::try_from(cursor_at(cx.args, "events")?).map_err(|_| cursor_invalid())?;
    let rows = events(
        cx.tx,
        EVENTS_FROM,
        params![ticket.id, PAGE_ROWS_MAX as i64 + 1, offset],
    )?;
    let mut page = Page::new(cx.data_bytes_max);
    assert!(page.put("id", json!(ticket.short_id.to_string())));
    assert!(page.put("events", json!([])));
    let mut cursor = None;
    for (shown, event) in rows.into_iter().enumerate() {
        if shown == PAGE_ROWS_MAX || !page.push("events", event) {
            assert!(shown > 0, "one event always fits a page");
            cursor = Some(format!("events:{}", offset + shown as i64));
            break;
        }
    }
    Ok(Outcome {
        data: page.into_value(),
        next: vec![Candidate::ticket(OpKind::Show, ticket, "read it in full")],
        cursor,
    })
}

/// One stored event as `events` reads it: when, who, which op, and the row before and after.
type EventRow = (i64, String, String, Option<String>, Option<String>);

fn events(
    tx: &Transaction<'_>,
    sql: &'static str,
    params: impl rusqlite::Params,
) -> Result<Vec<Value>, Failure> {
    let rows: Vec<EventRow> = store::prepare(tx, sql)?
        .query_map(params, |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    rows.into_iter()
        .map(|(at_ms, actor, op, before, after)| {
            Ok(json!({
                "at_ms": at_ms,
                "actor": actor,
                "op": op,
                "fields": changed_fields(before.as_deref(), after.as_deref())?,
            }))
        })
        .collect()
}

fn changed_fields(before: Option<&str>, after: Option<&str>) -> Result<Vec<String>, Failure> {
    let (Some(before), Some(after)) = (before, after) else {
        return Ok(Vec::new());
    };
    let parse = |text: &str| match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(object)) => Ok(object),
        _ => Err(Failure::from(StoreError::Corrupt(
            "an event row is not a JSON object".into(),
        ))),
    };
    let (before, after) = (parse(before)?, parse(after)?);
    let mut fields: Vec<String> = after
        .iter()
        .filter(|(name, value)| before.get(name.as_str()) != Some(*value))
        .map(|(name, _)| name.clone())
        .chain(
            before
                .keys()
                .filter(|name| !after.contains_key(name.as_str()))
                .cloned(),
        )
        .filter(|name| !IGNORED_FIELDS.contains(&name.as_str()))
        .map(|name| shown_field(&name).to_owned())
        .collect();
    fields.sort();
    fields.dedup();
    Ok(fields)
}

fn shown_field(stored: &str) -> &str {
    match stored {
        "parent_id" => "parent",
        "version_id" => "release",
        "cycle_id" => "cycle",
        other => other,
    }
}

fn schema<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let mut page = Page::new(cx.data_bytes_max);
    if let Some(kind) = cx.args.get::<OpKind>("op") {
        let mut op = catalog::op_schema(catalog::spec(kind));
        op["op"] = json!(kind.as_str());
        assert!(page.put("op", op), "one operation's schema fits a page");
        return Ok(Outcome {
            data: page.into_value(),
            next: Vec::new(),
            cursor: None,
        });
    }
    let from = usize::try_from(cursor_at(cx.args, "ops")?).map_err(|_| cursor_invalid())?;
    if from >= catalog::catalog().len() {
        return Err(cursor_invalid());
    }
    assert!(page.put("api", json!(reply::API)));
    assert!(page.put("ops", json!([])));
    let mut cursor = None;
    for (at, spec) in catalog::catalog().iter().enumerate().skip(from) {
        let entry = json!({
            "op": spec.kind.as_str(),
            "write": spec.kind.is_write(),
            "summary": spec.summary,
        });
        if !page.push("ops", entry) {
            assert!(at > from, "one operation entry always fits a page");
            cursor = Some(format!("ops:{at}"));
            break;
        }
    }
    Ok(Outcome {
        data: page.into_value(),
        next: Vec::new(),
        cursor,
    })
}

/// The parts of a ticket `show` pages through, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Body,
    Criteria,
    Links,
    Events,
}

impl Section {
    const ALL: [Section; 4] = [
        Section::Body,
        Section::Criteria,
        Section::Links,
        Section::Events,
    ];

    fn name(self) -> &'static str {
        match self {
            Section::Body => "body",
            Section::Criteria => "criteria",
            Section::Links => "links",
            Section::Events => "events",
        }
    }
}

/// Where one show page stopped: nothing left, or a section and the position to resume at.
type Stop = Option<(Section, u64)>;

fn show_cursor(args: &Args, ticket: &Ticket) -> Result<(Section, u64), Failure> {
    let Some(cursor) = args.get::<&str>("cursor") else {
        return Ok((Section::Body, 0));
    };
    let mut parts = cursor.split(':');
    let (Some(version), Some(section), Some(at), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(cursor_invalid());
    };
    if decimal(version) != u64::try_from(ticket.version).ok() {
        return Err(Failure::new(
            codes::CURSOR_INVALID,
            format!("{} changed since that page; show it again", ticket.short_id),
        )
        .with_next(vec![Candidate::ticket(
            OpKind::Show,
            ticket,
            "read the current version",
        )]));
    }
    let section = Section::ALL
        .into_iter()
        .find(|known| known.name() == section)
        .ok_or_else(cursor_invalid)?;
    let at = decimal(at).ok_or_else(cursor_invalid)?;
    Ok((section, at))
}

fn show<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let ticket = cx.ticket();
    let (from, at) = show_cursor(cx.args, ticket)?;
    let first = cx.args.get::<&str>("cursor").is_none();
    let mut page = Page::new(cx.data_bytes_max);
    let header = if first {
        ticket_json(cx.tx, ticket)?
    } else {
        json!({
            "id": ticket.short_id.to_string(),
            "version": ticket.version,
            "body_bytes": ticket.body.len(),
        })
    };
    assert!(page.put("ticket", header), "a ticket's fields fit a page");
    let mut added = 0;
    let mut stop = None;
    for section in Section::ALL
        .into_iter()
        .skip_while(|section| *section != from)
    {
        let start = if section == from { at } else { 0 };
        let (count, stopped) = match section {
            Section::Body => body_part(ticket, &mut page, start)?,
            Section::Criteria => criteria_part(cx, ticket, &mut page, start)?,
            Section::Links => links_part(cx, ticket, &mut page, start)?,
            Section::Events => events_part(cx, ticket, &mut page, start)?,
        };
        added += count;
        if let Some(position) = stopped {
            stop = Some((section, position));
            break;
        }
    }
    assert!(
        first || stop.is_none() || added > 0,
        "a continuation page always moves on"
    );
    let cursor = stop_cursor(ticket, stop);
    let mut next = ticket_next(cx.tx, ticket)?;
    let events: i64 =
        store::prepare(cx.tx, EVENTS_COUNT)?.query_row([ticket.id], |row| row.get(0))?;
    if events > SHOWN_EVENTS_MAX as i64 {
        next.insert(
            0,
            Candidate::ticket(OpKind::History, ticket, "older events are in history"),
        );
    }
    Ok(Outcome {
        data: page.into_value(),
        next,
        cursor,
    })
}

fn stop_cursor(ticket: &Ticket, stop: Stop) -> Option<String> {
    let (section, at) = stop?;
    let cursor = format!("{}:{}:{at}", ticket.version, section.name());
    assert!(cursor.len() <= catalog::CURSOR_BYTES_MAX);
    Some(cursor)
}

fn body_part(
    ticket: &Ticket,
    page: &mut Page,
    start: u64,
) -> Result<(usize, Option<u64>), Failure> {
    let body = &ticket.body;
    let start = usize::try_from(start).map_err(|_| cursor_invalid())?;
    if start > body.len() || !body.is_char_boundary(start) {
        return Err(cursor_invalid());
    }
    let rest = &body[start..];
    if rest.is_empty() {
        return Ok((0, None));
    }
    let at_cost = page.member_bytes("body_at", &json!(start));
    let left = page.left();
    let ends: Vec<usize> = rest
        .char_indices()
        .map(|(at, c)| at + c.len_utf8())
        .collect();
    let fitting = ends
        .partition_point(|end| at_cost + page.member_bytes("body", &json!(&rest[..*end])) <= left);
    if fitting == 0 {
        return Ok((0, Some(start as u64)));
    }
    let end = ends[fitting - 1];
    assert!(page.put("body_at", json!(start)));
    assert!(page.put("body", json!(&rest[..end])));
    let next = start + end;
    assert!(next <= body.len());
    Ok((1, (next < body.len()).then_some(next as u64)))
}

fn criteria_part<H: Host>(
    cx: &Cx<'_, '_, H>,
    ticket: &Ticket,
    page: &mut Page,
    start: u64,
) -> Result<(usize, Option<u64>), Failure> {
    let start = i64::try_from(start.max(1)).map_err(|_| cursor_invalid())?;
    let rows: Vec<(i64, i64, String, String, i64)> = store::prepare(cx.tx, CRITERIA_FROM)?
        .query_map(params![ticket.id, start], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut added = 0;
    for (id, position, text, state, version) in rows {
        let criterion = json!({
            "position": position,
            "text": text,
            "state": state,
            "version": version,
            "checks": checks_json(cx.tx, id)?,
        });
        if !page.push("criteria", criterion) {
            return Ok((added, Some(position as u64)));
        }
        added += 1;
    }
    Ok((added, None))
}

fn checks_json(tx: &Transaction<'_>, criterion: i64) -> Result<Vec<Value>, Failure> {
    let checks: Vec<(i64, String, String)> = store::prepare(tx, CHECKS_OF)?
        .query_map([criterion], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut shown = Vec::with_capacity(checks.len());
    for (id, kind, reference) in checks {
        let latest: Vec<(String, String, String, i64)> = store::prepare(tx, EVIDENCE_LATEST)?
            .query_map([id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        assert!(latest.len() <= 1);
        let (reference, elided) = reply::elide(&reference, SHOWN_REF_BYTES_MAX);
        let latest = latest.first().map(|(result, evidence, by, at_ms)| {
            json!({
                "result": result,
                "evidence": reply::elide(evidence, SHOWN_REF_BYTES_MAX).0,
                "by": by,
                "at_ms": at_ms,
            })
        });
        shown.push(json!({
            "kind": kind,
            "ref": reference,
            "ref_elided": elided,
            "latest": latest,
        }));
    }
    Ok(shown)
}

fn links_part<H: Host>(
    cx: &Cx<'_, '_, H>,
    ticket: &Ticket,
    page: &mut Page,
    start: u64,
) -> Result<(usize, Option<u64>), Failure> {
    let offset = i64::try_from(start).map_err(|_| cursor_invalid())?;
    let rows: Vec<(String, String)> = store::prepare(cx.tx, LINKS_FROM)?
        .query_map(
            params![ticket.id, LINKS_PER_TICKET_MAX as i64, offset],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
        .collect::<rusqlite::Result<_>>()?;
    let mut added = 0;
    for (kind, target) in rows {
        let state = cx.host.resolve(cx.tx, cx.project(), &kind, &target);
        if !page.push("links", link_json(&kind, &target, &state)) {
            return Ok((added, Some(start + added as u64)));
        }
        added += 1;
    }
    Ok((added, None))
}

fn events_part<H: Host>(
    cx: &Cx<'_, '_, H>,
    ticket: &Ticket,
    page: &mut Page,
    start: u64,
) -> Result<(usize, Option<u64>), Failure> {
    let skip = usize::try_from(start).map_err(|_| cursor_invalid())?;
    let recent = events(
        cx.tx,
        EVENTS_RECENT,
        params![ticket.id, SHOWN_EVENTS_MAX as i64],
    )?;
    assert!(recent.len() <= SHOWN_EVENTS_MAX);
    let mut added = 0;
    for event in recent.into_iter().skip(skip) {
        if !page.push("events", event) {
            return Ok((added, Some((skip + added) as u64)));
        }
        added += 1;
    }
    Ok((added, None))
}
