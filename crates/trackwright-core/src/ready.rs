use crate::host::Host;
use crate::model::{
    CYCLES_PER_PROJECT_MAX, Cycle, DEPENDENCIES_PER_TICKET_MAX, OpKind, ProjectKey, ShortId,
    TICKETS_PER_PROJECT_MAX, Ticket, TicketStatus, VERSIONS_PER_PROJECT_MAX,
};
use crate::ops::{self, Candidate, Cx, Failure, Outcome};
use crate::reply::{self, LISTED_TITLE_BYTES_MAX, PAGE_ROWS_MAX, Page};
use crate::requests::Args;
use crate::store;
use rusqlite::{Transaction, params};
use serde_json::{Value, json};

const CLOSED_NAMES: [&str; 2] = ["done", "cancelled"];
const READY_NAMES: [&str; 2] = ["todo", "in_progress"];

pub(crate) const READY_PAGE: &str = "SELECT t.id, t.number, t.title, t.status, t.priority, \
    t.assignee, c.number, v.name FROM tk_tickets t \
    LEFT JOIN tk_cycles c ON c.id = t.cycle_id LEFT JOIN tk_versions v ON v.id = t.version_id \
    WHERE t.project = ?1 AND t.status IN ('todo', 'in_progress') \
    AND (t.assignee IS NULL OR t.assignee = ?2) \
    AND NOT EXISTS (SELECT 1 FROM tk_deps d JOIN tk_tickets b ON b.id = d.ticket \
        WHERE d.blocks = t.id AND b.status NOT IN ('done', 'cancelled')) \
    AND NOT EXISTS (SELECT 1 FROM tk_tickets k \
        WHERE k.parent_id = t.id AND k.status NOT IN ('done', 'cancelled')) \
    ORDER BY t.priority, c.status IS NOT 'active', t.created_ms, t.number LIMIT ?3 OFFSET ?4";
pub(crate) const VERSIONS_PAGE: &str = "SELECT v.name, v.position, v.target_day, v.status, \
    v.version, (SELECT count(*) FROM tk_tickets t WHERE t.version_id = v.id \
        AND t.status IN ('done', 'cancelled')), \
    (SELECT count(*) FROM tk_tickets t WHERE t.version_id = v.id) \
    FROM tk_versions v WHERE v.project = ?1 ORDER BY v.position, v.name LIMIT ?2 OFFSET ?3";
pub(crate) const UNPLANNED_TALLY: &str = "SELECT count(CASE WHEN status IN ('done', \
    'cancelled') THEN 1 END), count(*) FROM tk_tickets WHERE project = ?1 AND version_id IS NULL";
pub(crate) const CYCLES_PAGE: &str = "SELECT c.number, c.status, c.starts_day, c.ends_day, \
    v.name, c.version, (SELECT count(*) FROM tk_tickets t WHERE t.cycle_id = c.id \
        AND t.status IN ('done', 'cancelled')), \
    (SELECT count(*) FROM tk_tickets t WHERE t.cycle_id = c.id) \
    FROM tk_cycles c LEFT JOIN tk_versions v ON v.id = c.version_id \
    WHERE c.project = ?1 ORDER BY c.number LIMIT ?2 OFFSET ?3";
pub(crate) const PLACEMENT_NAMES: &str = "SELECT (SELECT number FROM tk_tickets WHERE id = ?1), \
    (SELECT name FROM tk_versions WHERE id = ?2), (SELECT number FROM tk_cycles WHERE id = ?3)";
pub(crate) const VERSION_NAME: &str = "SELECT name FROM tk_versions WHERE id = ?1";
pub(crate) const BLOCKERS_OPEN: &str = "SELECT b.number FROM tk_deps d \
    JOIN tk_tickets b ON b.id = d.ticket WHERE d.blocks = ?1 \
    AND b.status NOT IN ('done', 'cancelled') ORDER BY b.number LIMIT ?2";

type ReadyRow = (
    i64,
    u32,
    String,
    String,
    i64,
    Option<String>,
    Option<u32>,
    Option<String>,
);

fn names_hold() {
    let closed: Vec<&str> = TicketStatus::ALL
        .iter()
        .filter(|status| status.is_closed())
        .map(|status| status.as_str())
        .collect();
    assert_eq!(
        closed, CLOSED_NAMES,
        "the SQL's closed statuses are the model's"
    );
    assert!(
        READY_NAMES
            .iter()
            .all(|name| TicketStatus::parse(name).is_some_and(|status| !status.is_closed())),
        "the SQL's ready statuses are open ones"
    );
}

/// Lists unblocked work the caller may take, most urgent first: open tickets in todo or
/// in progress, unclaimed or claimed by the caller, with no open blocker and no open child,
/// by priority, then the active cycle, then age. Input: the context. Output: one page.
pub(crate) fn ready<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    names_hold();
    let project = store::project(cx.tx, cx.project())?;
    let offset = ops::cursor_at(cx.args, "ready")?;
    if offset > TICKETS_PER_PROJECT_MAX as u64 {
        return Err(ops::cursor_invalid());
    }
    let offset = i64::try_from(offset).expect("an offset is at most the ticket bound");
    let rows: Vec<ReadyRow> = store::prepare(cx.tx, READY_PAGE)?
        .query_map(
            params![
                project.id,
                cx.actor.as_str(),
                PAGE_ROWS_MAX as i64 + 1,
                offset
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<_>>()?;
    assert!(rows.len() <= PAGE_ROWS_MAX + 1);
    let mut page = Page::new(cx.data_bytes_max);
    assert!(page.put("tickets", json!([])));
    let mut cursor = None;
    for (shown, row) in rows.iter().enumerate() {
        if shown == PAGE_ROWS_MAX || !page.push("tickets", ready_json(&project.key, row)?) {
            assert!(shown > 0, "one ready row always fits a page");
            cursor = Some(format!("ready:{}", offset + shown as i64));
            break;
        }
    }
    let mut next = Vec::new();
    if let Some((id, ..)) = rows.first() {
        let first = store::ticket_by_id(cx.tx, *id)?;
        if first.assignee.is_none() {
            next.push(Candidate::ticket(
                OpKind::Claim,
                &first,
                "take the most urgent work",
            ));
        }
        next.push(Candidate::ticket(
            OpKind::Show,
            &first,
            "read the most urgent work",
        ));
    }
    next.push(Candidate::project(
        OpKind::List,
        cx.project(),
        "every ticket by number",
    ));
    Ok(Outcome {
        data: page.into_value(),
        next,
        cursor,
    })
}

fn ready_json(key: &ProjectKey, row: &ReadyRow) -> Result<Value, Failure> {
    let (_, number, title, status, priority, assignee, cycle, release) = row;
    let short_id = ShortId::new(key.clone(), *number)?;
    let (title, elided) = reply::elide(title, LISTED_TITLE_BYTES_MAX);
    assert!(READY_NAMES.contains(&status.as_str()));
    assert!((0..=4).contains(priority));
    let mut value = json!({
        "id": short_id.to_string(),
        "title": title,
        "status": status,
        "priority": priority,
        "assignee": assignee,
        "cycle": cycle,
        "release": release,
    });
    if elided {
        value["elided"] = json!(true);
    }
    Ok(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Versions,
    Cycles,
}

fn roadmap_cursor(args: &Args) -> Result<(Part, i64), Failure> {
    let (part, at, max) = match args.get::<&str>("cursor") {
        None => return Ok((Part::Versions, 0)),
        Some(cursor) if cursor.starts_with("cycles:") => (
            Part::Cycles,
            ops::cursor_at(args, "cycles")?,
            CYCLES_PER_PROJECT_MAX,
        ),
        Some(_) => (
            Part::Versions,
            ops::cursor_at(args, "versions")?,
            VERSIONS_PER_PROJECT_MAX,
        ),
    };
    if at > max as u64 {
        return Err(ops::cursor_invalid());
    }
    Ok((part, i64::try_from(at).expect("a roadmap offset is small")))
}

/// Shows a project's roadmap: its product versions by position and its cycles by number,
/// each with closed and total tickets, and the tickets in no version. Input: the context.
/// Output: one page; versions come before cycles.
pub(crate) fn roadmap<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    names_hold();
    let project = store::project(cx.tx, cx.project())?;
    let (part, at) = roadmap_cursor(cx.args)?;
    let (closed, total): (i64, i64) = store::prepare(cx.tx, UNPLANNED_TALLY)?
        .query_row([project.id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    assert!((0..=total).contains(&closed));
    let mut page = Page::new(cx.data_bytes_max);
    assert!(page.put("unplanned", json!({"closed": closed, "total": total})));
    assert!(page.put("versions", json!([])));
    assert!(page.put("cycles", json!([])));
    let mut cursor = None;
    let mut fresh = true;
    if part == Part::Versions {
        let rows = tallied(cx.tx, VERSIONS_PAGE, project.id, at, version_json)?;
        fresh = rows.is_empty();
        cursor = paged(&mut page, "versions", rows, at, true);
    }
    if cursor.is_none() {
        let from = if part == Part::Cycles { at } else { 0 };
        let rows = tallied(cx.tx, CYCLES_PAGE, project.id, from, cycle_tally_json)?;
        cursor = paged(&mut page, "cycles", rows, from, fresh);
    }
    let key = cx.project();
    Ok(Outcome {
        data: page.into_value(),
        next: vec![
            Candidate::project(OpKind::Ready, key, "unblocked work in order"),
            Candidate::project(OpKind::VersionUpsert, key, "add or change a version"),
            Candidate::project(OpKind::CycleStart, key, "start the next cycle"),
        ],
        cursor,
    })
}

fn tallied(
    tx: &Transaction<'_>,
    sql: &'static str,
    project: i64,
    at: i64,
    shape: fn(&rusqlite::Row<'_>) -> rusqlite::Result<Value>,
) -> Result<Vec<Value>, Failure> {
    let rows: Vec<Value> = store::prepare(tx, sql)?
        .query_map(params![project, PAGE_ROWS_MAX as i64 + 1, at], shape)?
        .collect::<rusqlite::Result<_>>()?;
    assert!(rows.len() <= PAGE_ROWS_MAX + 1);
    Ok(rows)
}

fn paged(page: &mut Page, part: &str, rows: Vec<Value>, at: i64, fresh: bool) -> Option<String> {
    for (shown, row) in rows.into_iter().enumerate() {
        if shown == PAGE_ROWS_MAX || !page.push(part, row) {
            assert!(
                shown > 0 || !fresh,
                "one row always fits a page holding no other row"
            );
            return Some(format!("{part}:{}", at + shown as i64));
        }
    }
    None
}

fn tally(row: &rusqlite::Row<'_>, from: usize) -> rusqlite::Result<(i64, i64)> {
    let closed: i64 = row.get(from)?;
    let total: i64 = row.get(from + 1)?;
    assert!((0..=total).contains(&closed));
    Ok((closed, total))
}

fn version_json(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let (closed, total) = tally(row, 5)?;
    Ok(json!({
        "name": row.get::<_, String>(0)?,
        "position": row.get::<_, i64>(1)?,
        "target_day": row.get::<_, Option<String>>(2)?,
        "status": row.get::<_, String>(3)?,
        "version": row.get::<_, i64>(4)?,
        "closed": closed,
        "total": total,
    }))
}

fn cycle_tally_json(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let (closed, total) = tally(row, 6)?;
    Ok(json!({
        "number": row.get::<_, i64>(0)?,
        "status": row.get::<_, String>(1)?,
        "starts_day": row.get::<_, String>(2)?,
        "ends_day": row.get::<_, String>(3)?,
        "release": row.get::<_, Option<String>>(4)?,
        "version": row.get::<_, i64>(5)?,
        "closed": closed,
        "total": total,
    }))
}

/// A ticket's placement names: its parent's short id, its release name and its cycle number.
type PlacementNames = (Option<String>, Option<String>, Option<u32>);

/// The names a ticket's placement shows: its parent's short id, its product version's name
/// and its cycle's number. Inputs: the transaction and the ticket. Output: the three names.
pub(crate) fn placement_names(
    tx: &Transaction<'_>,
    ticket: &Ticket,
) -> Result<PlacementNames, Failure> {
    let (parent, release, cycle): (Option<u32>, Option<String>, Option<u32>) =
        store::prepare(tx, PLACEMENT_NAMES)?.query_row(
            params![ticket.parent_id, ticket.version_id, ticket.cycle_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    assert_eq!(parent.is_some(), ticket.parent_id.is_some());
    assert_eq!(release.is_some(), ticket.version_id.is_some());
    assert_eq!(cycle.is_some(), ticket.cycle_id.is_some());
    let parent = parent
        .map(|number| ShortId::new(ticket.short_id.key().clone(), number))
        .transpose()?
        .map(|id| id.to_string());
    Ok((parent, release, cycle))
}

/// The open tickets a ticket waits for, by number. Inputs: the transaction and the
/// ticket. Output: their short ids.
pub(crate) fn blocked_by(tx: &Transaction<'_>, ticket: &Ticket) -> Result<Vec<String>, Failure> {
    let numbers: Vec<u32> = store::prepare(tx, BLOCKERS_OPEN)?
        .query_map(
            params![ticket.id, DEPENDENCIES_PER_TICKET_MAX as i64],
            |row| row.get(0),
        )?
        .collect::<rusqlite::Result<_>>()?;
    assert!(numbers.len() <= DEPENDENCIES_PER_TICKET_MAX);
    assert!(numbers.windows(2).all(|pair| pair[0] < pair[1]));
    numbers
        .into_iter()
        .map(|number| Ok(ShortId::new(ticket.short_id.key().clone(), number)?.to_string()))
        .collect()
}

/// The fields a reply carries for a cycle. Inputs: the transaction and the cycle. Output:
/// the JSON object, its product version by name.
pub(crate) fn cycle_json(tx: &Transaction<'_>, cycle: &Cycle) -> Result<Value, Failure> {
    let release: Option<String> = match cycle.version_id {
        Some(id) => Some(store::prepare(tx, VERSION_NAME)?.query_row([id], |row| row.get(0))?),
        None => None,
    };
    assert_eq!(release.is_some(), cycle.version_id.is_some());
    Ok(json!({
        "number": cycle.number,
        "status": cycle.status.as_str(),
        "starts_day": cycle.starts_day,
        "ends_day": cycle.ends_day,
        "release": release,
        "version": cycle.version,
    }))
}
