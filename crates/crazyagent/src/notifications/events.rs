//! The Info events engine producers record at the point they happen (DEC-136a decision 5):
//! an engine that started again after it stopped without closing, a live master the engine
//! turned off or moved to another pool account, an account near or at a limit window, and a
//! newer harness version. Each records through `note`, so a producer never fails because of
//! it.

use super::{InfoNotice, NoticeKind, NoticeTarget, error, note, plain_version};
use crate::config::ModelAccess;
use crate::engine::EngineError;
use crate::protocol::{CredentialRef, ProjectId};
use rusqlite::{Connection, OptionalExtension, params};

/// The share of a limit window at which an account counts as near its limit.
pub const NEAR_PERCENT: f64 = 90.0;
/// The share of a limit window at which an account counts as at its limit.
pub const AT_PERCENT: f64 = 100.0;
/// How far apart two readings' reset times may be and still name one window: a reset time
/// worked out from "resets in N seconds" moves a little with every reading.
pub const RESET_SLACK_MS: i64 = 5 * 60 * 1000;
const ENGINE_RUN: &str = "engine_run";
const ENGINE_RESTARTED: &str = "The CrazyAgent engine stopped without closing and started again.";

const _: () = assert!(NEAR_PERCENT < AT_PERCENT);
const _: () = assert!(RESET_SLACK_MS > 0 && RESET_SLACK_MS * 12 <= 5 * 60 * 60 * 1000);
const _: () = assert!(ENGINE_RESTARTED.len() <= super::TEXT_BYTES_MAX);

fn logged(problem: &EngineError) {
    let code: String = problem.0.chars().take(200).collect();
    eprintln!("notification_not_recorded: {code}");
}

fn mark(connection: &Connection, running: bool) -> Result<(), EngineError> {
    connection
        .execute(
            "INSERT INTO owner_notice_cursors(source,last) VALUES(?1,?2)
             ON CONFLICT(source) DO UPDATE SET last=excluded.last",
            params![ENGINE_RUN, i64::from(running)],
        )
        .map_err(error)?;
    Ok(())
}

/// Notes that an engine started: when the last engine never reached `engine_stopped`, it
/// stopped without closing and this start records one restart item. Inputs: the engine
/// database and the time. Output: none; a failure is logged by its plain code.
pub(crate) fn engine_started(connection: &Connection, now: i64) {
    assert!(now >= 0);
    match super::reconcile::cursor(connection, ENGINE_RUN) {
        Ok(Some(1)) => note(
            connection,
            &InfoNotice {
                kind: NoticeKind::EngineRestarted,
                project_id: None,
                subject: &format!("engine_restarted:{now}"),
                text: ENGINE_RESTARTED,
                target: NoticeTarget::Home,
            },
            now,
        ),
        Ok(_) => {}
        Err(problem) => logged(&problem),
    }
    if let Err(problem) = mark(connection, true) {
        logged(&problem);
    }
}

/// Notes that the engine closed, so the next start records no restart. Input: the team
/// runtime's stop transaction. Output: Ok, or the SQLite failure.
pub(crate) fn engine_stopped(tx: &Connection) -> Result<(), EngineError> {
    assert!(!tx.is_autocommit());
    mark(tx, false)?;
    assert_eq!(super::reconcile::cursor(tx, ENGINE_RUN)?, Some(0));
    Ok(())
}

/// Records a live master the engine turned off, with the line it left in the conversation.
/// Inputs: the engine database, the project, the line and the time. Output: none.
pub(crate) fn master_off(connection: &Connection, project: &ProjectId, line: &str, now: i64) {
    assert!(!line.is_empty());
    assert!(now >= 0);
    note(
        connection,
        &InfoNotice {
            kind: NoticeKind::MasterOff,
            project_id: Some(project),
            subject: &format!("master_off:{}:{now}", project.as_str()),
            text: line,
            target: NoticeTarget::Conversation {
                project_id: project.clone(),
            },
        },
        now,
    );
}

/// Records a live master that moved to another account of its pool. Inputs: the engine
/// database, the project, the account names it left and moved to, and the time. Output:
/// none.
pub(crate) fn master_moved(
    connection: &Connection,
    project: &ProjectId,
    from: &str,
    to: &str,
    now: i64,
) {
    assert!(!from.is_empty() && !to.is_empty());
    assert!(now >= 0);
    note(
        connection,
        &InfoNotice {
            kind: NoticeKind::PoolFailover,
            project_id: Some(project),
            subject: &format!("master_moved:{}:{now}", project.as_str()),
            text: &format!("The live master moved from {from} to {to} in this project's pool."),
            target: NoticeTarget::Conversation {
                project_id: project.clone(),
            },
        },
        now,
    );
}

/// The name the owner gave one connection's account. Inputs: the engine database, the live
/// access and the connection. Output: the name, or "another account" when it is unknown.
pub(crate) fn connection_name(
    connection: &Connection,
    access: &ModelAccess,
    id: &CredentialRef,
) -> String {
    let model = access.connections.iter().find(|model| model.id == *id);
    let name = model
        .and_then(|model| crate::storage::accounts::account_name(connection, model).ok())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "another account".to_owned());
    assert!(!name.is_empty());
    name
}

/// One limit window as stored: the share used and when it resets, or none before a reading.
type Window = Option<(f64, Option<i64>)>;

/// One account's two limit windows as stored.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Limits {
    five_hour: Window,
    weekly: Window,
}

/// Reads one account's stored limit windows. Inputs: the usage store and the account.
/// Output: the windows, none of them before the first reading, or none at all when they
/// cannot be read, which records nothing.
pub(crate) fn limits(db: &Connection, account: &CredentialRef) -> Option<Limits> {
    let read = db
        .query_row(
            "SELECT five_hour_percent, five_hour_resets_ms, weekly_percent, weekly_resets_ms
             FROM account_limits WHERE account=?1",
            [account.as_str()],
            |row| {
                let window = |used: usize, resets: usize| -> rusqlite::Result<Window> {
                    let percent: Option<f64> = row.get(used)?;
                    let resets_ms: Option<i64> = row.get(resets)?;
                    Ok(percent.map(|percent| (percent, resets_ms)))
                };
                Ok(Limits {
                    five_hour: window(0, 1)?,
                    weekly: window(2, 3)?,
                })
            },
        )
        .optional();
    match read {
        Ok(found) => Some(found.unwrap_or(Limits {
            five_hour: None,
            weekly: None,
        })),
        Err(problem) => {
            logged(&error(problem));
            None
        }
    }
}

/// Whether two readings' reset times name one window: both unknown, or both known and at
/// most `RESET_SLACK_MS` apart. Inputs: the two reset times. Output: true for one window.
fn one_window(before: Option<i64>, after: Option<i64>) -> bool {
    match (before, after) {
        (None, None) => true,
        (Some(before), Some(after)) => before.abs_diff(after) <= RESET_SLACK_MS.unsigned_abs(),
        _ => false,
    }
}

/// Which item a window's new reading makes: at the limit when it reaches `AT_PERCENT`, near
/// it when it reaches `NEAR_PERCENT`, each only when the reading before in the same window
/// was below. Inputs: the window before and after. Output: the kind, or none.
fn crossed(before: Window, after: Window) -> Option<NoticeKind> {
    let (used, resets) = after?;
    let prior = before
        .filter(|(_, prior_resets)| one_window(*prior_resets, resets))
        .map_or(0.0, |(prior, _)| prior);
    assert!((0.0..=100.0).contains(&used) && (0.0..=100.0).contains(&prior));
    if used >= AT_PERCENT && prior < AT_PERCENT {
        return Some(NoticeKind::AccountAtLimit);
    }
    (used >= NEAR_PERCENT && prior < NEAR_PERCENT).then_some(NoticeKind::AccountNearLimit)
}

/// Records an account whose stored window crossed `NEAR_PERCENT` or `AT_PERCENT` since the
/// `limits` read before a write, one item per account, window and reset time. Inputs: the usage store,
/// the account, the windows read before the write and the time. Output: none.
pub(crate) fn limits_crossed(
    db: &Connection,
    account: &CredentialRef,
    before: Option<Limits>,
    now: i64,
) {
    let Some(before) = before else {
        return;
    };
    let Some(after) = limits(db, account) else {
        return;
    };
    let name = crate::storage::accounts::registered_name(db, account)
        .ok()
        .flatten()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "An account".to_owned());
    let windows = [
        ("5-hour", "five_hour", before.five_hour, after.five_hour),
        ("weekly", "weekly", before.weekly, after.weekly),
    ];
    for (label, key, prior, latest) in windows {
        let Some(kind) = crossed(prior, latest) else {
            continue;
        };
        let (used, resets) = latest.expect("a crossed window has a reading");
        let text = match kind {
            NoticeKind::AccountAtLimit => format!("{name} reached its {label} limit."),
            _ => format!("{name} is near its {label} limit: {used:.0}% used."),
        };
        let subject = format!(
            "limit:{}:{}:{key}:{}",
            kind.name(),
            account.as_str(),
            resets.unwrap_or(0)
        );
        note(
            db,
            &InfoNotice {
                kind,
                project_id: None,
                subject: &subject,
                text: &text,
                target: NoticeTarget::Usage,
            },
            now,
        );
    }
}

/// Records a newer version of a known harness once per version. Inputs: the engine
/// database, the harness id or alias, the newer version and the time. Output: none; an
/// unknown harness or a version that is not plain records nothing.
pub(crate) fn harness_update(connection: &Connection, harness: &str, version: &str, now: i64) {
    assert!(now >= 0);
    let Some(spec) = crate::harnesses::spec(harness) else {
        return;
    };
    if !plain_version(version) {
        return;
    }
    note(
        connection,
        &InfoNotice {
            kind: NoticeKind::HarnessUpdate,
            project_id: None,
            subject: &format!("harness_update:{}:{version}", spec.id),
            text: &format!("{} {version} is available.", spec.name),
            target: NoticeTarget::Harnesses,
        },
        now,
    );
}
