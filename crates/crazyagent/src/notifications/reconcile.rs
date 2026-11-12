//! Keeps the store in step with the engine (DEC-136a decisions 1 to 3): every open Action
//! condition has one item, an item whose condition cleared is resolved, and new Info events
//! of the activity feed and the team alerts are copied in through id cursors.

use super::{
    InfoNotice, NoticeClass, NoticeKind, NoticeTarget, TICK_MS, error, prune, prune_due, record,
    settings, shown, target_text,
};
use crate::activity::ActivityKind;
use crate::config::ModelAccess;
use crate::engine::EngineError;
use crate::landing::ChangeSetId;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::collections::{BTreeMap, BTreeSet};

/// The most conditions one source reports and the most rows one import copies in a tick.
pub const SOURCE_ROWS_MAX: usize = 512;
/// How old an imported event may be; older rows are history a backfill brought in late.
pub const IMPORT_AGE_MS_MAX: i64 = 10 * 60 * 1000;
const ACTIVITY: &str = "project_activity";
const TEAM: &str = "team_notifications";
const TEAM_ALERTS: [&str; 4] = ["escalation", "wake_paused", "queue_full", "budget_stopped"];

/// One Action condition that holds now: its kind, project, the key that names it across
/// ticks, its text and the page that fixes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Condition {
    pub kind: NoticeKind,
    pub project_id: Option<ProjectId>,
    pub subject: String,
    pub text: String,
    pub target: NoticeTarget,
}

/// Runs `reconcile` at once and then at most once per `TICK_MS`, for the team runtime's
/// fast tick.
#[derive(Debug, Default)]
pub struct Ticker {
    last_ms: Option<i64>,
}

impl Ticker {
    /// Reconciles when the interval passed. Inputs: the engine database, the model access
    /// and the time. Output: whether it ran, or the first failure of a run.
    pub fn tick(
        &mut self,
        connection: &mut Connection,
        access: &ModelAccess,
        now: i64,
    ) -> Result<bool, EngineError> {
        if self.last_ms.is_some_and(|last| now - last < TICK_MS) {
            return Ok(false);
        }
        self.last_ms = Some(now);
        reconcile(connection, access, now)?;
        assert_eq!(self.last_ms, Some(now));
        Ok(true)
    }
}

/// One open item of an Action kind as `sync` compares it.
struct Open {
    id: i64,
    text: String,
    target: String,
    dismissed: bool,
}

/// What `sync` changes: conditions to open, open items to update, items to resolve, and
/// hidden items to delete because their state cleared.
#[derive(Default)]
struct Plan<'a> {
    opened: Vec<&'a Condition>,
    updated: Vec<(i64, String, String)>,
    resolved: Vec<i64>,
    deleted: Vec<i64>,
}

impl Plan<'_> {
    fn is_empty(&self) -> bool {
        self.opened.is_empty()
            && self.updated.is_empty()
            && self.resolved.is_empty()
            && self.deleted.is_empty()
    }
}

/// Reads every open item of the given kinds, keyed by kind and subject. Inputs: the
/// database and the kinds. Output: the items.
fn open_items(
    connection: &Connection,
    kinds: &[NoticeKind],
) -> Result<BTreeMap<(NoticeKind, String), Open>, EngineError> {
    let mut open = BTreeMap::new();
    let mut statement = connection
        .prepare(
            "SELECT id, subject, text, target, dismissed_ms IS NOT NULL FROM owner_notices
             WHERE kind=?1 AND resolved_ms IS NULL ORDER BY id LIMIT ?2",
        )
        .map_err(error)?;
    for kind in kinds {
        let rows: Vec<(i64, String, String, String, bool)> = statement
            .query_map(params![kind.name(), super::ROWS_MAX], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .map_err(error)?
            .collect::<Result<_, _>>()
            .map_err(error)?;
        for (id, subject, text, target, dismissed) in rows {
            let item = Open {
                id,
                text,
                target,
                dismissed,
            };
            assert!(open.insert((*kind, subject), item).is_none());
        }
    }
    Ok(open)
}

/// Compares the live conditions with the open items. Inputs: the kinds whose sources were
/// read, those of them read only in part, the conditions and the open items. Output: the
/// plan, which resolves and deletes nothing of a kind read only in part.
fn plan<'a>(
    kinds: &[NoticeKind],
    partial: &[NoticeKind],
    live: &'a [Condition],
    open: &BTreeMap<(NoticeKind, String), Open>,
) -> Result<Plan<'a>, EngineError> {
    assert!(kinds.iter().all(|kind| kind.class() == NoticeClass::Action));
    assert!(partial.iter().all(|kind| kinds.contains(kind)));
    assert!(live.iter().all(|condition| kinds.contains(&condition.kind)));
    let mut keys = BTreeSet::new();
    let mut plan = Plan::default();
    for condition in live {
        let key = (condition.kind, condition.subject.clone());
        assert!(
            keys.insert(key.clone()),
            "a source named one condition twice"
        );
        match open.get(&key) {
            None => plan.opened.push(condition),
            Some(item) if item.dismissed => {}
            Some(item) => {
                let text = shown(&condition.text);
                let target = target_text(&condition.target)?;
                if item.text != text || item.target != target {
                    plan.updated.push((item.id, text, target));
                }
            }
        }
    }
    for (key, item) in open {
        if keys.contains(key) || !kinds.contains(&key.0) || partial.contains(&key.0) {
            continue;
        }
        if item.dismissed {
            plan.deleted.push(item.id);
        } else {
            plan.resolved.push(item.id);
        }
    }
    Ok(plan)
}

/// Brings the open items of the given kinds in step with their live conditions: a new
/// condition opens a pending unread item, a changed one takes the new text and page, and an
/// item whose condition is gone is resolved and read, or deleted when the owner had hidden
/// it. Kinds not listed are left alone, so a source that could not be read resolves nothing,
/// and a kind read only in part opens and updates items but resolves none. Inputs: the
/// transaction, the kinds, those read in part, the conditions and the time. Output: Ok, or
/// the SQLite failure.
pub(crate) fn sync(
    tx: &Transaction<'_>,
    kinds: &[NoticeKind],
    partial: &[NoticeKind],
    live: &[Condition],
    now: i64,
) -> Result<(), EngineError> {
    let open = open_items(tx, kinds)?;
    let plan = plan(kinds, partial, live, &open)?;
    for condition in &plan.opened {
        let text = shown(&condition.text);
        assert!(!text.is_empty());
        assert_eq!(condition.target.project(), condition.project_id.as_ref());
        tx.execute(
            "INSERT INTO owner_notices(kind,action,project,subject,text,target,created_ms)
             VALUES (?1,1,?2,?3,?4,?5,?6)",
            params![
                condition.kind.name(),
                condition.project_id.as_ref().map(ProjectId::as_str),
                condition.subject,
                text,
                target_text(&condition.target)?,
                now
            ],
        )
        .map_err(error)?;
    }
    for (id, text, target) in &plan.updated {
        tx.execute(
            "UPDATE owner_notices SET text=?2, target=?3 WHERE id=?1",
            params![id, text, target],
        )
        .map_err(error)?;
    }
    for id in &plan.resolved {
        let changed = tx
            .execute(
                "UPDATE owner_notices SET resolved_ms=?2, read_ms=COALESCE(read_ms, ?2)
                 WHERE id=?1 AND resolved_ms IS NULL",
                params![id, now],
            )
            .map_err(error)?;
        assert_eq!(changed, 1);
    }
    for id in &plan.deleted {
        tx.execute("DELETE FROM owner_notices WHERE id=?1", [id])
            .map_err(error)?;
    }
    super::cap(tx)
}

/// One Info item an import copies in, and the cursor it moves.
struct Imported {
    kind: NoticeKind,
    project_id: ProjectId,
    subject: String,
    text: String,
    target: NoticeTarget,
}

/// What one import source holds past its cursor: the cursor read, where it moves and the
/// items to copy.
struct Import {
    source: &'static str,
    from: Option<i64>,
    to: i64,
    items: Vec<Imported>,
}

/// Reads one source's cursor. Inputs: the database and the source. Output: the last id it
/// copied, or none before its first tick.
pub(super) fn cursor(connection: &Connection, source: &str) -> Result<Option<i64>, EngineError> {
    connection
        .query_row(
            "SELECT last FROM owner_notice_cursors WHERE source=?1",
            [source],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)
}

fn table_exists(connection: &Connection, name: &str) -> Result<bool, EngineError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [name],
            |row| row.get(0),
        )
        .map_err(error)
}

/// The Info kind and page one activity row becomes, if any. Inputs: the row's kind, its
/// project and change set. Output: the kind and target, or none for a kind the bell skips.
fn activity_kind(
    kind: ActivityKind,
    project_id: &ProjectId,
    change_set: Option<&str>,
) -> Option<(NoticeKind, NoticeTarget)> {
    let agents = || NoticeTarget::Agents {
        project_id: project_id.clone(),
    };
    let changes = || NoticeTarget::ChangeSet {
        project_id: project_id.clone(),
        change_set: change_set.and_then(|id| ChangeSetId::new(id).ok()),
    };
    match kind {
        ActivityKind::TaskFinished => Some((NoticeKind::AgentFinished, agents())),
        ActivityKind::TaskFailed => Some((NoticeKind::AgentFailed, agents())),
        ActivityKind::TaskStopped => Some((NoticeKind::AgentStopped, agents())),
        ActivityKind::Accepted | ActivityKind::MergedIntoMain => {
            Some((NoticeKind::ChangeSetLanded, changes()))
        }
        ActivityKind::Conflict => Some((NoticeKind::ChangeSetConflict, changes())),
        ActivityKind::Undone => Some((NoticeKind::ChangeSetRolledBack, changes())),
        _ => None,
    }
}

/// Reads the activity rows past the cursor that the bell shows. The cursor starts at the
/// newest row, and steps back when ids went backwards after a project's rows were removed.
/// Rows older than `IMPORT_AGE_MS_MAX` are passed over. Inputs: the database and the time.
/// Output: the import, or none when the activity table is missing.
fn activity_import(connection: &Connection, now: i64) -> Result<Option<Import>, EngineError> {
    if !table_exists(connection, ACTIVITY)? {
        return Ok(None);
    }
    let head: i64 = connection
        .query_row(
            "SELECT COALESCE(MAX(id),0) FROM project_activity",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    let from = cursor(connection, ACTIVITY)?;
    let Some(last) = from.filter(|last| *last <= head) else {
        return Ok(Some(Import {
            source: ACTIVITY,
            from,
            to: head,
            items: Vec::new(),
        }));
    };
    type Row = (i64, String, i64, String, String, Option<String>);
    let rows: Vec<Row> = connection
        .prepare(
            "SELECT id, project, at_ms, kind, text, change_set FROM project_activity
             WHERE id>?1 ORDER BY id LIMIT ?2",
        )
        .map_err(error)?
        .query_map(params![last, SOURCE_ROWS_MAX as i64], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    let to = rows.last().map_or(last, |row| row.0);
    let mut items = Vec::new();
    for (id, project, at_ms, kind, text, change_set) in rows {
        let (Ok(project_id), Some(kind)) = (ProjectId::new(project), ActivityKind::parse(&kind))
        else {
            continue;
        };
        if now - at_ms > IMPORT_AGE_MS_MAX {
            continue;
        }
        let Some((kind, target)) = activity_kind(kind, &project_id, change_set.as_deref()) else {
            continue;
        };
        let subject = format!("activity:{id}:{at_ms}");
        items.push(Imported {
            kind,
            project_id,
            subject,
            text,
            target,
        });
    }
    assert!(to >= last);
    Ok(Some(Import {
        source: ACTIVITY,
        from,
        to,
        items,
    }))
}

/// Reads the team alerts past the cursor: escalations, paused wakes, full event queues and
/// stopped budgets become team alert items; question notifications are left to the question
/// items. The cursor starts at the newest row. Input: the database. Output: the import, or
/// none when the table is missing.
fn team_import(connection: &Connection) -> Result<Option<Import>, EngineError> {
    if !table_exists(connection, TEAM)? {
        return Ok(None);
    }
    let head: i64 = connection
        .query_row(
            "SELECT COALESCE(MAX(id),0) FROM team_notifications",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    let from = cursor(connection, TEAM)?;
    let Some(last) = from.filter(|last| *last <= head) else {
        return Ok(Some(Import {
            source: TEAM,
            from,
            to: head,
            items: Vec::new(),
        }));
    };
    let rows: Vec<(i64, String, String, String)> = connection
        .prepare("SELECT id, project, kind, text FROM team_notifications WHERE id>?1 ORDER BY id LIMIT ?2")
        .map_err(error)?
        .query_map(params![last, SOURCE_ROWS_MAX as i64], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    let to = rows.last().map_or(last, |row| row.0);
    let items: Vec<Imported> = rows
        .into_iter()
        .filter(|row| TEAM_ALERTS.contains(&row.2.as_str()))
        .filter_map(|(id, project, _, text)| {
            let project_id = ProjectId::new(project).ok()?;
            Some(Imported {
                kind: NoticeKind::TeamAlert,
                target: NoticeTarget::Conversation {
                    project_id: project_id.clone(),
                },
                project_id,
                subject: format!("team:{id}"),
                text,
            })
        })
        .collect();
    assert!(to >= last);
    Ok(Some(Import {
        source: TEAM,
        from,
        to,
        items,
    }))
}

/// Copies one import's items and moves its cursor, only when the cursor is still where the
/// import read it. Inputs: the transaction, the import and the time. Output: Ok, or the
/// SQLite failure.
fn copy(tx: &Transaction<'_>, import: &Import, now: i64) -> Result<(), EngineError> {
    if cursor(tx, import.source)? != import.from {
        return Ok(());
    }
    for item in &import.items {
        let info = InfoNotice {
            kind: item.kind,
            project_id: Some(&item.project_id),
            subject: &item.subject,
            text: &item.text,
            target: item.target.clone(),
        };
        record(tx, &info, now)?;
    }
    tx.execute(
        "INSERT INTO owner_notice_cursors(source,last) VALUES(?1,?2)
         ON CONFLICT(source) DO UPDATE SET last=excluded.last",
        params![import.source, import.to],
    )
    .map_err(error)?;
    assert_eq!(cursor(tx, import.source)?, Some(import.to));
    Ok(())
}

/// Brings the store in step with the engine once: reads every Action source and the Info
/// imports, and writes in one IMMEDIATE transaction only when an item, a cursor or the
/// retention changes something. A source that fails is skipped and its failure returned
/// after the rest is written. Inputs: the engine database, the model access and the time.
/// Output: Ok, or the first failure.
pub fn reconcile(
    connection: &mut Connection,
    access: &ModelAccess,
    now: i64,
) -> Result<(), EngineError> {
    let live = super::sources::live(connection, access);
    let imports: Vec<Import> = [activity_import(connection, now)?, team_import(connection)?]
        .into_iter()
        .flatten()
        .filter(|import| import.from != Some(import.to) || !import.items.is_empty())
        .collect();
    let settings = settings(connection)?;
    let open = open_items(connection, &live.kinds)?;
    let changed = !plan(&live.kinds, &live.partial, &live.conditions, &open)?.is_empty();
    if changed || !imports.is_empty() || prune_due(connection, &settings, now)? {
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        sync(&tx, &live.kinds, &live.partial, &live.conditions, now)?;
        for import in &imports {
            copy(&tx, import, now)?;
        }
        prune(&tx, &settings, now)?;
        tx.commit().map_err(error)?;
    }
    live.problem.map_or(Ok(()), Err)
}
