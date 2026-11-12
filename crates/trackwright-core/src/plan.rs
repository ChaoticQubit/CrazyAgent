use crate::events::{self, Recorded};
use crate::host::{Host, TicketView};
use crate::model::{
    CYCLES_PER_PROJECT_MAX, Cycle, CycleStatus, DEPENDENCIES_PER_TICKET_MAX, OpKind, Project,
    ProjectKey, ReleaseVersion, RolloverPolicy, ShortId, TICKETS_PER_PROJECT_MAX, Ticket,
    VERSIONS_PER_PROJECT_MAX, VersionStatus, codes,
};
use crate::ops::{self, Candidate, Cx, Failure, Outcome};
use crate::policy;
use crate::ready;
use crate::reply::{NEXT_MAX, PAGE_ROWS_MAX, Page};
use crate::requests;
use crate::store::{self, Placement, StoreError, TicketChange};
use rusqlite::{OptionalExtension, Row, Transaction, params};
use serde_json::{Value, json};

pub const PARENT_DEPTH_MAX: usize = 16;
pub const DAY_MS: i64 = 86_400_000;
pub const DAY_NUMBER_MIN: i64 = day_number(1, 1, 1);
pub const DAY_NUMBER_MAX: i64 = day_number(9999, 12, 31);

const _: () = assert!(DAY_NUMBER_MIN == -719_162);
const _: () = assert!(DAY_NUMBER_MAX == 2_932_896);
const _: () = assert!(VERSIONS_PER_PROJECT_MAX <= u32::MAX as usize);
const _: () = assert!(CYCLES_PER_PROJECT_MAX <= u32::MAX as usize);

macro_rules! version_select {
    () => {
        "SELECT id, project, name, position, target_day, status, source, version FROM tk_versions"
    };
}

macro_rules! cycle_select {
    () => {
        "SELECT id, project, version_id, number, starts_day, ends_day, status, version \
         FROM tk_cycles"
    };
}

pub(crate) const VERSION_BY_NAME: &str =
    concat!(version_select!(), " WHERE project = ?1 AND name = ?2");
pub(crate) const VERSION_TALLY: &str =
    "SELECT count(*), max(position) FROM tk_versions WHERE project = ?1";
pub(crate) const VERSION_INSERT: &str = "INSERT INTO tk_versions(project, name, position, \
    target_day, status, source, version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)";
pub(crate) const VERSION_UPDATE: &str = "UPDATE tk_versions SET position = ?2, target_day = ?3, \
    status = ?4, version = ?5 WHERE id = ?1 AND version = ?6";
pub(crate) const CYCLE_BY_NUMBER: &str =
    concat!(cycle_select!(), " WHERE project = ?1 AND number = ?2");
pub(crate) const CYCLE_LATEST: &str = concat!(
    cycle_select!(),
    " WHERE project = ?1 ORDER BY number DESC LIMIT 1"
);
pub(crate) const CYCLE_NEXT_OPEN: &str = concat!(
    cycle_select!(),
    " WHERE project = ?1 AND number > ?2 AND status <> 'closed' ORDER BY number LIMIT 1"
);
pub(crate) const CYCLE_FIRST_UPCOMING: &str = concat!(
    cycle_select!(),
    " WHERE project = ?1 AND status = 'upcoming' ORDER BY number LIMIT 1"
);
pub(crate) const CYCLE_COUNT: &str = "SELECT count(*) FROM tk_cycles WHERE project = ?1";
pub(crate) const CYCLE_ACTIVE: &str =
    "SELECT count(*) FROM tk_cycles WHERE project = ?1 AND status = 'active'";
pub(crate) const CYCLE_INSERT: &str = "INSERT INTO tk_cycles(project, version_id, number, \
    starts_day, ends_day, status, version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)";
pub(crate) const CYCLE_STATUS_SET: &str =
    "UPDATE tk_cycles SET status = ?2, version = ?3 WHERE id = ?1 AND version = ?4";
pub(crate) const CYCLE_OPEN_TICKETS: &str = "SELECT id FROM tk_tickets WHERE cycle_id = ?1 \
    AND status NOT IN ('done', 'cancelled') ORDER BY number LIMIT ?2";
pub(crate) const PARENT_OF: &str = "SELECT parent_id FROM tk_tickets WHERE id = ?1";
pub(crate) const DEP_GET: &str = "SELECT count(*) FROM tk_deps WHERE ticket = ?1 AND blocks = ?2";
pub(crate) const DEP_COUNT: &str = "SELECT count(*) FROM tk_deps WHERE blocks = ?1";
pub(crate) const DEP_REACHES: &str = "WITH RECURSIVE downstream(id) AS (SELECT ?1 UNION \
    SELECT d.blocks FROM tk_deps d JOIN downstream s ON d.ticket = s.id LIMIT ?3) \
    SELECT count(*) FROM downstream WHERE id = ?2";
pub(crate) const DEP_INSERT: &str = "INSERT INTO tk_deps(ticket, blocks) VALUES (?1, ?2)";
pub(crate) const DEP_DELETE: &str = "DELETE FROM tk_deps WHERE ticket = ?1 AND blocks = ?2";

/// The day count since 1970-01-01 of a civil date, by Howard Hinnant's algorithm. Inputs:
/// the year, month and day of a real calendar date. Output: the day number.
pub const fn day_number(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year.rem_euclid(400);
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let of_cycle = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    era * 146_097 + of_cycle - 719_468
}

/// Reads a calendar day `YYYY-MM-DD`. Input: the text. Output: its day number, or None
/// when it is not a real date from year 1 to 9999.
pub fn day_of(text: &str) -> Option<i64> {
    if !requests::calendar_day(text) {
        return None;
    }
    let part = |from: usize, to: usize| text[from..to].parse::<i64>().ok();
    let day = day_number(part(0, 4)?, part(5, 7)?, part(8, 10)?);
    assert!((DAY_NUMBER_MIN..=DAY_NUMBER_MAX).contains(&day));
    Some(day)
}

/// Writes a day number as `YYYY-MM-DD`, the inverse of `day_of`. Input: a day from year 1
/// to 9999. Output: the text.
pub fn day_text(day: i64) -> String {
    assert!((DAY_NUMBER_MIN..=DAY_NUMBER_MAX).contains(&day));
    let shifted = day + 719_468;
    let era = shifted.div_euclid(146_097);
    let of_era = shifted.rem_euclid(146_097);
    let year_of_era = (of_era - of_era / 1_460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * of_year + 2) / 153;
    let month_day = of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    let text = format!("{year:04}-{month:02}-{month_day:02}");
    assert_eq!(
        day_of(&text),
        Some(day),
        "a written day reads back as itself"
    );
    text
}

fn stored_day(text: &str) -> Result<i64, Failure> {
    day_of(text).ok_or_else(|| {
        Failure::from(StoreError::Corrupt(
            "a stored day is not a calendar day".into(),
        ))
    })
}

fn version_row(row: &Row<'_>) -> rusqlite::Result<ReleaseVersion> {
    Ok(ReleaseVersion {
        id: row.get(0)?,
        project: row.get(1)?,
        name: row.get(2)?,
        position: row.get(3)?,
        target_day: row.get(4)?,
        status: store::named(row, 5, VersionStatus::parse)?,
        source: row.get(6)?,
        version: row.get(7)?,
    })
}

fn cycle_row(row: &Row<'_>) -> rusqlite::Result<Cycle> {
    Ok(Cycle {
        id: row.get(0)?,
        project: row.get(1)?,
        version_id: row.get(2)?,
        number: row.get(3)?,
        starts_day: row.get(4)?,
        ends_day: row.get(5)?,
        status: store::named(row, 6, CycleStatus::parse)?,
        version: row.get(7)?,
    })
}

/// Reads a product version by name. Inputs: the transaction, the project's row id and the
/// name. Output: the version, or None.
pub(crate) fn version_by_name(
    tx: &Transaction<'_>,
    project: i64,
    name: &str,
) -> Result<Option<ReleaseVersion>, Failure> {
    let found = store::prepare(tx, VERSION_BY_NAME)?
        .query_row(params![project, name], version_row)
        .optional()?;
    assert!(found.as_ref().is_none_or(|version| version.name == name));
    assert!(
        found
            .as_ref()
            .is_none_or(|version| version.project == project)
    );
    Ok(found)
}

fn cycle_by_number(
    tx: &Transaction<'_>,
    project: i64,
    number: i64,
) -> Result<Option<Cycle>, Failure> {
    let found = store::prepare(tx, CYCLE_BY_NUMBER)?
        .query_row(params![project, number], cycle_row)
        .optional()?;
    assert!(
        found
            .as_ref()
            .is_none_or(|cycle| i64::from(cycle.number) == number)
    );
    assert!(found.as_ref().is_none_or(|cycle| cycle.project == project));
    Ok(found)
}

fn one_cycle(
    tx: &Transaction<'_>,
    sql: &'static str,
    params: impl rusqlite::Params,
) -> Result<Option<Cycle>, Failure> {
    Ok(store::prepare(tx, sql)?
        .query_row(params, cycle_row)
        .optional()?)
}

fn counted(tx: &Transaction<'_>, sql: &'static str, id: i64) -> Result<i64, Failure> {
    let count: i64 = store::prepare(tx, sql)?.query_row([id], |row| row.get(0))?;
    assert!(count >= 0);
    Ok(count)
}

fn roadmap_next(key: &ProjectKey) -> Candidate {
    Candidate::project(OpKind::Roadmap, key, "read the versions and cycles")
}

fn version_unknown(key: &ProjectKey, name: &str) -> Failure {
    Failure::new(
        codes::VERSION_UNKNOWN,
        format!("{key} has no version {name}"),
    )
    .with_next(vec![roadmap_next(key)])
}

fn closed_ticket(ticket: &Ticket) -> Failure {
    Failure::new(
        codes::TICKET_CLOSED,
        format!("{} is closed; reopen opens it", ticket.short_id),
    )
    .with_next(vec![Candidate::ticket(
        OpKind::Reopen,
        ticket,
        "reopen it to work on it again",
    )])
}

/// Assigns an open ticket to the caller. A ticket claimed by another actor refuses
/// `already_claimed` with `ready` next; claiming one's own claim changes nothing. Input:
/// the context. Output: the ticket.
pub(crate) fn claim<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let ticket = cx.ticket();
    if ticket.status.is_closed() {
        return Err(closed_ticket(ticket));
    }
    match &ticket.assignee {
        Some(assignee) if assignee == cx.actor => return ops::written(cx, ticket),
        Some(assignee) => {
            return Err(Failure::new(
                codes::ALREADY_CLAIMED,
                format!(
                    "{} is claimed by {assignee}; take other ready work",
                    ticket.short_id
                ),
            )
            .with_data(json!({
                "id": ticket.short_id.to_string(),
                "assignee": assignee.as_str(),
                "version": ticket.version,
            }))
            .with_next(vec![Candidate::project(
                OpKind::Ready,
                ticket.short_id.key(),
                "other unblocked work",
            )]));
        }
        None => {}
    }
    let change = TicketChange {
        assignee: Some(Some(cx.actor.clone())),
        ..TicketChange::default()
    };
    let outcome = ops::update(cx, ticket, &change)?;
    assert_eq!(outcome.data["assignee"], cx.actor.as_str());
    Ok(outcome)
}

/// Gives up a claim. The claimant may release it; anyone else needs the host's authority
/// to edit the ticket, as the owner and engines have under the standalone policy. An
/// unclaimed ticket refuses `not_claimant`. Input: the context. Output: the ticket.
pub(crate) fn release<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let ticket = cx.ticket();
    let Some(assignee) = &ticket.assignee else {
        return Err(Failure::new(
            codes::NOT_CLAIMANT,
            format!("{} is not claimed", ticket.short_id),
        ));
    };
    let view = TicketView::of(ticket);
    if assignee != cx.actor
        && cx
            .host
            .actor_may(cx.tx, cx.project(), cx.actor, OpKind::Edit, Some(&view))
            .is_err()
    {
        return Err(Failure::new(
            codes::NOT_CLAIMANT,
            format!(
                "{} is claimed by {assignee}; only they or an actor who may edit it release it",
                ticket.short_id
            ),
        )
        .with_data(json!({"id": ticket.short_id.to_string(), "assignee": assignee.as_str()})));
    }
    let change = TicketChange {
        assignee: Some(None),
        ..TicketChange::default()
    };
    let outcome = ops::update(cx, ticket, &change)?;
    assert!(outcome.data["assignee"].is_null());
    Ok(outcome)
}

fn dependency(ticket: &Ticket, on: &Ticket) -> Value {
    json!({"id": ticket.short_id.to_string(), "on": on.short_id.to_string()})
}

/// Records that a ticket waits for another open or closed ticket of its project. A
/// dependency that would close a loop refuses `dependency_cycle`. Input: the context.
/// Output: both ids and whether the ticket is now blocked.
pub(crate) fn depend<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let ticket = cx.ticket();
    let on = store::ticket(cx.tx, cx.args.need::<&ShortId>("on"))?;
    if on.id == ticket.id || on.project != ticket.project {
        return Err(Failure::new(
            codes::DEPENDENCY_INVALID,
            format!(
                "{} cannot wait for {}: a ticket waits for another ticket of its project",
                ticket.short_id, on.short_id
            ),
        ));
    }
    if counted_pair(cx.tx, DEP_GET, on.id, ticket.id)? > 0 {
        return Err(Failure::new(
            codes::DEPENDENCY_EXISTS,
            format!("{} already waits for {}", ticket.short_id, on.short_id),
        ));
    }
    if counted(cx.tx, DEP_COUNT, ticket.id)? >= DEPENDENCIES_PER_TICKET_MAX as i64 {
        return Err(Failure::new(
            codes::DEPENDENCIES_FULL,
            format!(
                "{} waits for {DEPENDENCIES_PER_TICKET_MAX} tickets",
                ticket.short_id
            ),
        ));
    }
    let reaches: i64 = store::prepare(cx.tx, DEP_REACHES)?.query_row(
        params![ticket.id, on.id, TICKETS_PER_PROJECT_MAX as i64 + 1],
        |row| row.get(0),
    )?;
    if reaches > 0 {
        return Err(Failure::new(
            codes::DEPENDENCY_CYCLE,
            format!("{} already waits for {}", on.short_id, ticket.short_id),
        )
        .with_data(dependency(ticket, &on)));
    }
    store::write(cx.tx, || {
        let written = store::prepare(cx.tx, DEP_INSERT)?.execute([on.id, ticket.id])?;
        assert_eq!(written, 1);
        record_dependency(
            cx,
            ticket,
            events::DEP_ADD,
            None,
            Some(dependency(ticket, &on)),
        )
    })?;
    assert_eq!(counted_pair(cx.tx, DEP_GET, on.id, ticket.id)?, 1);
    let mut data = dependency(ticket, &on);
    data["blocked"] = json!(!on.status.is_closed());
    Ok(Outcome {
        data,
        next: ops::ticket_next(cx.tx, ticket)?,
        cursor: None,
    })
}

/// Removes a dependency, or refuses `dependency_unknown`. Input: the context. Output: both
/// ids and `removed`.
pub(crate) fn undepend<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let ticket = cx.ticket();
    let on = store::ticket(cx.tx, cx.args.need::<&ShortId>("on"))?;
    if counted_pair(cx.tx, DEP_GET, on.id, ticket.id)? == 0 {
        return Err(Failure::new(
            codes::DEPENDENCY_UNKNOWN,
            format!("{} does not wait for {}", ticket.short_id, on.short_id),
        ));
    }
    store::write(cx.tx, || {
        let written = store::prepare(cx.tx, DEP_DELETE)?.execute([on.id, ticket.id])?;
        assert_eq!(
            written, 1,
            "the dependency read in this transaction is still there"
        );
        record_dependency(
            cx,
            ticket,
            events::DEP_REMOVE,
            Some(dependency(ticket, &on)),
            None,
        )
    })?;
    assert_eq!(counted_pair(cx.tx, DEP_GET, on.id, ticket.id)?, 0);
    let mut data = dependency(ticket, &on);
    data["removed"] = json!(true);
    Ok(Outcome {
        data,
        next: ops::ticket_next(cx.tx, ticket)?,
        cursor: None,
    })
}

fn counted_pair(
    tx: &Transaction<'_>,
    sql: &'static str,
    first: i64,
    second: i64,
) -> Result<i64, Failure> {
    let count: i64 = store::prepare(tx, sql)?.query_row([first, second], |row| row.get(0))?;
    assert!((0..=1).contains(&count), "a dependency is stored once");
    Ok(count)
}

fn record_dependency<H: Host>(
    cx: &Cx<'_, '_, H>,
    ticket: &Ticket,
    op: &'static str,
    before: Option<Value>,
    after: Option<Value>,
) -> Result<(), StoreError> {
    events::record(
        cx.tx,
        Recorded {
            project: ticket.project,
            ticket: Some(ticket.id),
            op,
            actor: cx.actor,
            at_ms: cx.at_ms,
            before,
            after,
        },
    )
}

/// Reads and checks where a create or an edit places a ticket: its parent, product version
/// and cycle, each set by value or, on edit, cleared by its flag. Inputs: the context and
/// the ticket an edit changes. Output: the placement, or the refusal of the first bad field.
pub(crate) fn placement<H: Host>(
    cx: &Cx<'_, '_, H>,
    ticket: Option<&Ticket>,
) -> Result<Placement, Failure> {
    assert!(matches!(cx.kind, OpKind::Create | OpKind::Edit));
    assert_eq!(ticket.is_some(), cx.kind == OpKind::Edit);
    let project = store::project(cx.tx, cx.project())?;
    let mut placement = Placement::default();
    if let Some(by_value) = wanted(cx, "parent")? {
        placement.parent_id = Some(
            match cx.args.get::<&ShortId>("parent").filter(|_| by_value) {
                Some(id) => Some(parent_id(cx, &project, ticket, id)?),
                None => None,
            },
        );
    }
    if let Some(by_value) = wanted(cx, "release")? {
        placement.version_id = Some(match cx.args.get::<&str>("release").filter(|_| by_value) {
            Some(name) => Some(
                version_by_name(cx.tx, project.id, name)?
                    .ok_or_else(|| version_unknown(&project.key, name))?
                    .id,
            ),
            None => None,
        });
    }
    if let Some(by_value) = wanted(cx, "cycle")? {
        placement.cycle_id = Some(match cx.args.get::<i64>("cycle").filter(|_| by_value) {
            Some(number) => Some(open_cycle(cx.tx, &project, number)?.id),
            None => None,
        });
    }
    if let (Some(Some(parent)), Some(ticket)) = (placement.parent_id, ticket) {
        assert_ne!(parent, ticket.id, "a ticket is never its own parent");
    }
    Ok(placement)
}

fn wanted<H: Host>(cx: &Cx<'_, '_, H>, name: &str) -> Result<Option<bool>, Failure> {
    let clear = format!("{name}_clear");
    let given = match name {
        "parent" => cx.args.get::<&ShortId>(name).is_some(),
        "release" => cx.args.get::<&str>(name).is_some(),
        _ => cx.args.get::<i64>(name).is_some(),
    };
    let cleared = cx.args.has_field(&clear) && cx.args.get::<bool>(&clear) == Some(true);
    match (given, cleared) {
        (true, true) => Err(Failure::new(
            codes::FIELD_INVALID,
            format!("give {name} or {clear}, not both"),
        )),
        (true, false) => Ok(Some(true)),
        (false, true) => Ok(Some(false)),
        (false, false) => Ok(None),
    }
}

fn parent_id<H: Host>(
    cx: &Cx<'_, '_, H>,
    project: &Project,
    ticket: Option<&Ticket>,
    id: &ShortId,
) -> Result<i64, Failure> {
    let tx = cx.tx;
    let parent = store::ticket(tx, id)?;
    let invalid = |why: String| Err(Failure::new(codes::PARENT_INVALID, why));
    if parent.project != project.id {
        return invalid(format!("{id} is not a ticket of {}", project.key));
    }
    if parent.status.is_closed() {
        return invalid(format!("{id} is closed; a parent is open"));
    }
    let view = TicketView::of(&parent);
    assert_eq!(view.id.key(), &project.key);
    cx.host
        .actor_may(tx, &project.key, cx.actor, OpKind::Edit, Some(&view))?;
    let own = ticket.map(|ticket| ticket.id);
    let mut at = Some(parent.id);
    for _ in 0..PARENT_DEPTH_MAX {
        let Some(ancestor) = at else {
            return Ok(parent.id);
        };
        if Some(ancestor) == own {
            return invalid(format!("{id} is the ticket itself or one of its children"));
        }
        at = store::prepare(tx, PARENT_OF)?.query_row([ancestor], |row| row.get(0))?;
    }
    if at.is_some() {
        return invalid(format!("a parent chain is at most {PARENT_DEPTH_MAX} deep"));
    }
    assert_ne!(Some(parent.id), own);
    Ok(parent.id)
}

fn open_cycle(tx: &Transaction<'_>, project: &Project, number: i64) -> Result<Cycle, Failure> {
    let Some(cycle) = cycle_by_number(tx, project.id, number)? else {
        return Err(Failure::new(
            codes::CYCLE_UNKNOWN,
            format!("{} has no cycle {number}", project.key),
        )
        .with_next(vec![roadmap_next(&project.key)]));
    };
    if cycle.status == CycleStatus::Closed {
        return Err(Failure::new(
            codes::CYCLE_CLOSED,
            format!("cycle {number} of {} is closed", project.key),
        )
        .with_next(vec![roadmap_next(&project.key)]));
    }
    assert_ne!(cycle.status, CycleStatus::Closed);
    Ok(cycle)
}

/// The fields a reply carries for a product version. Input: the version. Output: the JSON
/// object.
pub(crate) fn version_json(version: &ReleaseVersion) -> Value {
    json!({
        "name": version.name,
        "position": version.position,
        "target_day": version.target_day,
        "status": version.status.as_str(),
        "version": version.version,
    })
}

/// Adds a product version, or changes one at the row version the caller read. Input: the
/// context. Output: the version as stored.
pub(crate) fn version_upsert<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let project = store::project(cx.tx, cx.project())?;
    let name = cx.args.need::<&str>("name");
    let expected = cx.args.get::<i64>("version");
    let stored = match (version_by_name(cx.tx, project.id, name)?, expected) {
        (None, Some(_)) => return Err(version_unknown(&project.key, name)),
        (None, None) => add_version(cx, &project, name)?,
        (Some(found), Some(expected)) if found.version == expected => {
            change_version(cx, &project, found)?
        }
        (Some(found), _) => {
            return Err(Failure::new(
                codes::STALE_VERSION,
                format!(
                    "version {name} is at row version {}; give it to change {name}",
                    found.version
                ),
            )
            .with_data(json!({"name": name, "version": found.version}))
            .with_next(vec![roadmap_next(&project.key)]));
        }
    };
    assert_eq!(stored.name, name);
    Ok(Outcome {
        data: json!({"version": version_json(&stored)}),
        next: vec![roadmap_next(&project.key)],
        cursor: None,
    })
}

fn add_version<H: Host>(
    cx: &Cx<'_, '_, H>,
    project: &Project,
    name: &str,
) -> Result<ReleaseVersion, Failure> {
    let (count, last): (i64, Option<i64>) = store::prepare(cx.tx, VERSION_TALLY)?
        .query_row([project.id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    if count >= VERSIONS_PER_PROJECT_MAX as i64 {
        return Err(Failure::new(
            codes::VERSIONS_FULL,
            format!("{} holds {VERSIONS_PER_PROJECT_MAX} versions", project.key),
        ));
    }
    let position_max = VERSIONS_PER_PROJECT_MAX as i64 - 1;
    let position = match cx.args.get::<i64>("position") {
        Some(position) => position,
        None => last.map_or(0, |last| (last + 1).min(position_max)),
    };
    assert!((0..=position_max).contains(&position));
    let mut added = ReleaseVersion {
        id: 0,
        project: project.id,
        name: name.to_owned(),
        position: u32::try_from(position).expect("a position is 0 to 63"),
        target_day: cx.args.get::<&str>("target_day").map(str::to_owned),
        status: cx
            .args
            .get::<VersionStatus>("status")
            .unwrap_or(VersionStatus::Planned),
        source: "op".to_owned(),
        version: 1,
    };
    store::write(cx.tx, || {
        store::prepare(cx.tx, VERSION_INSERT)?.execute(params![
            project.id,
            added.name,
            added.position,
            added.target_day,
            added.status.as_str(),
            added.source,
        ])?;
        added.id = cx.tx.last_insert_rowid();
        record_project(cx, project, events::VERSION_UPSERT, None, Some(&added))
    })?;
    assert!(added.id > 0);
    Ok(added)
}

fn change_version<H: Host>(
    cx: &Cx<'_, '_, H>,
    project: &Project,
    found: ReleaseVersion,
) -> Result<ReleaseVersion, Failure> {
    let mut changed = found.clone();
    if let Some(position) = cx.args.get::<i64>("position") {
        changed.position = u32::try_from(position).expect("a position is 0 to 63");
    }
    if let Some(day) = cx.args.get::<&str>("target_day") {
        changed.target_day = Some(day.to_owned());
    }
    if let Some(status) = cx.args.get::<VersionStatus>("status") {
        changed.status = status;
    }
    if changed == found {
        return Ok(found);
    }
    changed.version = found.version + 1;
    store::write(cx.tx, || {
        let written = store::prepare(cx.tx, VERSION_UPDATE)?.execute(params![
            changed.id,
            changed.position,
            changed.target_day,
            changed.status.as_str(),
            changed.version,
            found.version,
        ])?;
        assert_eq!(
            written, 1,
            "the version read in this transaction is current"
        );
        record_project(
            cx,
            project,
            events::VERSION_UPSERT,
            Some(&found),
            Some(&changed),
        )
    })?;
    assert_eq!(changed.version, found.version + 1);
    Ok(changed)
}

fn record_project<H: Host, T: serde::Serialize>(
    cx: &Cx<'_, '_, H>,
    project: &Project,
    op: &'static str,
    before: Option<&T>,
    after: Option<&T>,
) -> Result<(), StoreError> {
    events::record(
        cx.tx,
        Recorded {
            project: project.id,
            ticket: None,
            op,
            actor: cx.actor,
            at_ms: cx.at_ms,
            before: before.map(events::snapshot),
            after: after.map(events::snapshot),
        },
    )
}

/// Starts a project's next cycle: from the day after the latest one, or today, for the
/// policy's `cycle_days` unless the request says otherwise. Input: the context. Output:
/// the cycle, active when no other cycle is.
pub(crate) fn cycle_start<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let project = store::project(cx.tx, cx.project())?;
    let version_id = match cx.args.get::<&str>("release") {
        Some(name) => Some(
            version_by_name(cx.tx, project.id, name)?
                .ok_or_else(|| version_unknown(&project.key, name))?
                .id,
        ),
        None => None,
    };
    let starts = cx
        .args
        .get::<&str>("starts_day")
        .map(|text| day_of(text).expect("a parsed day is a calendar day"));
    let cycle = new_cycle(cx, &project, starts, cx.args.get::<i64>("days"), version_id)?;
    assert_eq!(cycle.version, 1);
    assert_eq!(cycle.version_id, version_id);
    Ok(Outcome {
        data: json!({"cycle": ready::cycle_json(cx.tx, &cycle)?}),
        next: vec![roadmap_next(&project.key)],
        cursor: None,
    })
}

fn new_cycle<H: Host>(
    cx: &Cx<'_, '_, H>,
    project: &Project,
    starts: Option<i64>,
    days: Option<i64>,
    version_id: Option<i64>,
) -> Result<Cycle, Failure> {
    if counted(cx.tx, CYCLE_COUNT, project.id)? >= CYCLES_PER_PROJECT_MAX as i64 {
        return Err(Failure::new(
            codes::CYCLES_FULL,
            format!("{} holds {CYCLES_PER_PROJECT_MAX} cycles", project.key),
        ));
    }
    let latest = one_cycle(cx.tx, CYCLE_LATEST, [project.id])?;
    let after = latest
        .as_ref()
        .map(|cycle| stored_day(&cycle.ends_day))
        .transpose()?;
    let today = cx.at_ms.div_euclid(DAY_MS);
    let starts = starts.unwrap_or_else(|| after.map_or(today, |after| (after + 1).max(today)));
    if let Some(after) = after.filter(|after| starts <= *after) {
        return Err(Failure::new(
            codes::CYCLE_OVERLAPS,
            format!("a new cycle starts after {}", day_text(after)),
        )
        .with_data(json!({"after": day_text(after)})));
    }
    let days = match days {
        Some(days) => days,
        None => i64::from(policy::load(cx.tx, &project.key)?.cycle_days.get()),
    };
    assert!(days >= 1);
    let ends = starts + days - 1;
    if ends > DAY_NUMBER_MAX {
        return Err(Failure::new(
            codes::FIELD_INVALID,
            "a cycle ends by 9999-12-31",
        ));
    }
    let active = counted(cx.tx, CYCLE_ACTIVE, project.id)? > 0;
    let number = latest.map_or(1, |cycle| cycle.number + 1);
    assert!(number as usize <= CYCLES_PER_PROJECT_MAX);
    let cycle = Cycle {
        id: 0,
        project: project.id,
        version_id,
        number,
        starts_day: day_text(starts),
        ends_day: day_text(ends),
        status: if active {
            CycleStatus::Upcoming
        } else {
            CycleStatus::Active
        },
        version: 1,
    };
    insert_cycle(cx, project, cycle)
}

fn insert_cycle<H: Host>(
    cx: &Cx<'_, '_, H>,
    project: &Project,
    mut cycle: Cycle,
) -> Result<Cycle, Failure> {
    assert_eq!(cycle.id, 0);
    assert_eq!(cycle.project, project.id);
    store::write(cx.tx, || {
        store::prepare(cx.tx, CYCLE_INSERT)?.execute(params![
            project.id,
            cycle.version_id,
            cycle.number,
            cycle.starts_day,
            cycle.ends_day,
            cycle.status.as_str(),
        ])?;
        cycle.id = cx.tx.last_insert_rowid();
        record_project(cx, project, events::CYCLE_UPSERT, None, Some(&cycle))
    })?;
    assert!(cycle.id > 0);
    Ok(cycle)
}

fn set_cycle_status<H: Host>(
    cx: &Cx<'_, '_, H>,
    project: &Project,
    cycle: &Cycle,
    status: CycleStatus,
) -> Result<Cycle, Failure> {
    assert_ne!(cycle.status, status);
    assert_ne!(
        cycle.status,
        CycleStatus::Closed,
        "a closed cycle stays closed"
    );
    let mut changed = cycle.clone();
    changed.status = status;
    changed.version = cycle.version + 1;
    store::write(cx.tx, || {
        let written = store::prepare(cx.tx, CYCLE_STATUS_SET)?.execute(params![
            changed.id,
            changed.status.as_str(),
            changed.version,
            cycle.version,
        ])?;
        assert_eq!(written, 1, "the cycle read in this transaction is current");
        record_project(
            cx,
            project,
            events::CYCLE_UPSERT,
            Some(cycle),
            Some(&changed),
        )
    })?;
    Ok(changed)
}

/// Closes a cycle at the version the caller read. Its unfinished tickets roll as the
/// `rollover` policy says: `auto` moves them to the next open cycle, made when none is,
/// `ask` lists them with `edit` next, `off` leaves them. When no cycle is left active, the
/// first upcoming one starts. Input: the context. Output: what closed and what moved.
pub(crate) fn cycle_close<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let project = store::project(cx.tx, cx.project())?;
    let number = cx.args.need::<i64>("number");
    let cycle = open_cycle(cx.tx, &project, number)?;
    let expected = cx.args.need::<i64>("version");
    if cycle.version != expected {
        return Err(Failure::new(
            codes::STALE_VERSION,
            format!(
                "cycle {number} is at version {}, not {expected}",
                cycle.version
            ),
        )
        .with_data(json!({"number": number, "version": cycle.version}))
        .with_next(vec![roadmap_next(&project.key)]));
    }
    let closed = set_cycle_status(cx, &project, &cycle, CycleStatus::Closed)?;
    let ids: Vec<i64> = store::prepare(cx.tx, CYCLE_OPEN_TICKETS)?
        .query_map(params![cycle.id, TICKETS_PER_PROJECT_MAX as i64], |row| {
            row.get(0)
        })?
        .collect::<rusqlite::Result<_>>()?;
    let unfinished = ids
        .iter()
        .map(|id| store::ticket_by_id(cx.tx, *id))
        .collect::<Result<Vec<Ticket>, StoreError>>()?;
    let rollover = policy::load(cx.tx, &project.key)?.rollover;
    let moved_to = if rollover == RolloverPolicy::Auto && !unfinished.is_empty() {
        Some(roll(cx, &project, &closed, &unfinished)?)
    } else {
        None
    };
    if counted(cx.tx, CYCLE_ACTIVE, project.id)? == 0
        && let Some(upcoming) = one_cycle(cx.tx, CYCLE_FIRST_UPCOMING, [project.id])?
    {
        set_cycle_status(cx, &project, &upcoming, CycleStatus::Active)?;
    }
    assert!(counted(cx.tx, CYCLE_ACTIVE, project.id)? <= 1);
    closed_outcome(cx, &project, &closed, rollover, &unfinished, moved_to)
}

fn roll<H: Host>(
    cx: &Cx<'_, '_, H>,
    project: &Project,
    closed: &Cycle,
    unfinished: &[Ticket],
) -> Result<u32, Failure> {
    assert!(!unfinished.is_empty());
    let target = match one_cycle(cx.tx, CYCLE_NEXT_OPEN, params![project.id, closed.number])? {
        Some(target) => target,
        None => new_cycle(cx, project, None, None, closed.version_id)?,
    };
    assert!(target.number > closed.number);
    let change = TicketChange {
        placement: Placement {
            cycle_id: Some(Some(target.id)),
            ..Placement::default()
        },
        ..TicketChange::default()
    };
    for ticket in unfinished {
        assert_eq!(ticket.cycle_id, Some(closed.id));
        let moved = store::update_ticket(
            cx.tx,
            cx.actor,
            &ticket.short_id,
            ticket.version,
            &change,
            cx.at_ms,
        )?;
        assert_eq!(moved.cycle_id, Some(target.id));
    }
    Ok(target.number)
}

fn closed_outcome<H: Host>(
    cx: &Cx<'_, '_, H>,
    project: &Project,
    closed: &Cycle,
    rollover: RolloverPolicy,
    unfinished: &[Ticket],
    moved_to: Option<u32>,
) -> Result<Outcome, Failure> {
    let mut page = Page::new(cx.data_bytes_max);
    assert!(page.put("closed", ready::cycle_json(cx.tx, closed)?));
    assert!(page.put("rollover", json!(rollover.as_str())));
    assert!(page.put("unfinished", json!(unfinished.len())));
    assert!(page.put("moved_to", json!(moved_to)));
    assert!(page.put("tickets", json!([])));
    for ticket in unfinished.iter().take(PAGE_ROWS_MAX) {
        if !page.push("tickets", json!(ticket.short_id.to_string())) {
            break;
        }
    }
    let mut next: Vec<Candidate> = Vec::new();
    if rollover == RolloverPolicy::Ask {
        next.extend(unfinished.iter().take(NEXT_MAX - 1).map(|ticket| {
            Candidate::ticket(OpKind::Edit, ticket, "move it to another cycle or clear it")
        }));
    }
    next.push(roadmap_next(&project.key));
    assert!(next.len() <= NEXT_MAX);
    Ok(Outcome {
        data: page.into_value(),
        next,
        cursor: None,
    })
}
