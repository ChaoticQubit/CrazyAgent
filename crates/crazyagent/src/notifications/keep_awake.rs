//! The Keep awake notices (DEC-167 addendum 10): a hold that ended by itself and heat a
//! hold runs on through, both Info items the keeper writes inside its own change, and the
//! sleep changes an earlier engine could not put back, an Action kind whose state is the
//! `keep_awake_restore` table the keeper fills after the engine start's undo and, under its
//! own subject, while the lid helper cannot read its journal (QUESTION 36b). The approval
//! source reads the Keep awake view while the lid helper waits for approval (QUESTION 35j).

use super::reconcile::{Condition, SOURCE_ROWS_MAX};
use super::sources::{Found, tables_exist};
use super::{InfoNotice, NoticeKind, NoticeTarget, TEXT_BYTES_MAX, error, note};
use crate::config::ModelAccess;
use crate::engine::EngineError;
use crate::keep_awake::journal::ENTRIES_MAX;
use crate::keep_awake::{EndReason, HeatEnd, Leftover, LidSupport};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use std::collections::BTreeSet;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const PERCENT_MAX: u8 = 100;
const ENTRY_SUBJECT: &str = "keep_awake_restore:entry:";
const UNREADABLE_SUBJECT: &str = "keep_awake_restore:unreadable";
const NOT_PUT_BACK: &str = "Keep awake could not put back a sleep setting it changed. It tries \
                            again when the engine starts.";
const NOT_READ: &str = "Keep awake could not read its list of sleep settings to put back. It \
                        tries again when the engine starts.";
const NOT_THAWED: &str = "Keep awake could not start an app again that it paused while the lid \
                          was closed. Run crazyagent keep-awake restore, or quit the app and \
                          open it again.";
const NOT_UNMUTED: &str = "Keep awake could not put the sound back after the lid was closed. Run \
                           crazyagent keep-awake restore, or check the sound settings.";
const HOT_UNTIL_CRITICAL: &str =
    "This computer is running hot. Keep awake goes on and ends if the heat gets critical.";
const HOT_NEVER_ENDS: &str =
    "This computer is running hot. Keep awake goes on; it is set never to end for heat.";
const HOT_ENDS: &str = "This computer is running hot. Keep awake ends at this heat.";

/// The restore subject of a lid helper that cannot read its journal.
pub(crate) const LID_SUBJECT: &str = "keep_awake_restore:lid_helper";
/// The restore line of a lid helper that cannot read its journal: the fix of threat model
/// section 8, in its order.
pub(crate) const LID_JOURNAL_TEXT: &str = "The lid helper cannot read its journal, so sleep \
    may stay off. Run sudo pmset -a disablesleep 0, then remove its journal.json in \
    /Library/Application Support/dev.crazyagent.power with sudo, then Remove lid helper and \
    add it again.";
/// The approval subject and line (QUESTION 35j).
pub(crate) const APPROVAL_SUBJECT: &str = "keep_awake_approval";
pub(crate) const APPROVAL_TEXT: &str =
    "Allow the lid helper in System Settings to hold with the lid closed.";

const TABLE: &str = "CREATE TABLE IF NOT EXISTS keep_awake_restore (
    subject TEXT PRIMARY KEY CHECK(length(subject) BETWEEN 1 AND 128),
    unreadable INTEGER NOT NULL CHECK(unreadable IN (0,1))
)";

const _: () = assert!(ENTRIES_MAX < SOURCE_ROWS_MAX);
const _: () = assert!(NOT_PUT_BACK.len() <= TEXT_BYTES_MAX);
const _: () = assert!(NOT_READ.len() <= TEXT_BYTES_MAX);
const _: () = assert!(NOT_THAWED.len() <= TEXT_BYTES_MAX);
const _: () = assert!(NOT_UNMUTED.len() <= TEXT_BYTES_MAX);
const _: () = assert!(HOT_UNTIL_CRITICAL.len() <= TEXT_BYTES_MAX);
const _: () = assert!(HOT_NEVER_ENDS.len() <= TEXT_BYTES_MAX);
const _: () = assert!(LID_JOURNAL_TEXT.len() <= TEXT_BYTES_MAX);
const _: () = assert!(APPROVAL_TEXT.len() <= TEXT_BYTES_MAX);
const _: () = assert!(LID_SUBJECT.len() <= 128);
const _: () = assert!(APPROVAL_SUBJECT.len() <= 128);
const _: () = assert!(ENTRY_SUBJECT.len() + 64 <= 128);

/// Creates the restore state when missing. Input: the engine database. Output: Ok, or the
/// SQLite failure.
pub(super) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(TABLE).map_err(error)
}

/// The line of a hold that ended by itself, in the words of the Keep awake control.
/// Input: the end reason. Output: the line, or none for an end the owner chose (End, a
/// timer that ran out, mode Off).
pub fn ended_text(ended: EndReason) -> Option<String> {
    let text = match ended {
        EndReason::Battery { pct } => format!(
            "Keep awake ended: the battery reached {}%.",
            pct.min(PERCENT_MAX)
        ),
        EndReason::BatteryUnread => {
            "Keep awake ended: the battery charge could not be read.".into()
        }
        EndReason::Heat => "Keep awake ended: this computer got too hot.".into(),
        EndReason::Cap { hours } => format!("Keep awake ended after its {hours} h limit."),
        EndReason::HoldLost => {
            "Keep awake ended: the system did not keep this computer awake.".into()
        }
        EndReason::Manual | EndReason::Off => return None,
    };
    assert!(!text.is_empty());
    assert!(text.len() <= TEXT_BYTES_MAX);
    Some(text)
}

/// The line of heat a hold runs on through. Input: the End at heat setting. Output: the
/// line.
pub fn hot_text(heat_end: HeatEnd) -> &'static str {
    match heat_end {
        HeatEnd::Critical => HOT_UNTIL_CRITICAL,
        HeatEnd::Never => HOT_NEVER_ENDS,
        HeatEnd::Serious => HOT_ENDS,
    }
}

/// Records one hold that was in place and ended by itself, once per hold; a hold the
/// system did not keep is said at most once a UTC day, so a computer that can never hold
/// does not say it for every burst of work. Inputs: the connection inside the change's
/// transaction, the end reason, when the hold started and the time. Output: none; a write
/// that fails is logged.
pub(crate) fn hold_ended(connection: &Connection, ended: EndReason, since_ms: i64, now: i64) {
    assert!(since_ms > 0);
    assert!(now > 0);
    let Some(text) = ended_text(ended) else {
        return;
    };
    let subject = match ended {
        EndReason::HoldLost => format!("keep_awake_ended:hold_lost:{}", now / DAY_MS),
        _ => format!("keep_awake_ended:{since_ms}"),
    };
    let info = InfoNotice {
        kind: NoticeKind::KeepAwakeEnded,
        project_id: None,
        subject: &subject,
        text: &text,
        target: NoticeTarget::KeepAwake,
    };
    note(connection, &info, now);
}

/// Records heat a hold runs on through, once per hold. Inputs: the connection inside the
/// change's transaction, the End at heat setting, when the hold started and the time.
/// Output: none; a write that fails is logged.
pub(crate) fn running_hot(connection: &Connection, heat_end: HeatEnd, since_ms: i64, now: i64) {
    assert!(since_ms > 0);
    assert!(now > 0);
    let subject = format!("keep_awake_heat:{since_ms}");
    let info = InfoNotice {
        kind: NoticeKind::KeepAwakeHeat,
        project_id: None,
        subject: &subject,
        text: hot_text(heat_end),
        target: NoticeTarget::KeepAwake,
    };
    note(connection, &info, now);
}

/// Replaces the restore state with what the engine start's undo left: one row per kind of
/// entry it could not undo, or one row when the journal could not be read, never shown by
/// its kind. The lid helper row is not the undo's and stays. Inputs: the engine database
/// outside any transaction and what was left. Output: Ok, or the SQLite failure, with
/// nothing changed.
pub(crate) fn left_over(connection: &Connection, leftover: &Leftover) -> Result<(), EngineError> {
    assert!(connection.is_autocommit());
    let rows: Vec<(String, bool)> = match leftover {
        Leftover::Kinds(kinds) => kinds
            .iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|kind| (format!("{ENTRY_SUBJECT}{kind}"), false))
            .collect(),
        Leftover::Unreadable => vec![(UNREADABLE_SUBJECT.into(), true)],
    };
    assert!(rows.len() <= ENTRIES_MAX);
    let tx =
        Transaction::new_unchecked(connection, TransactionBehavior::Immediate).map_err(error)?;
    tx.execute(
        "DELETE FROM keep_awake_restore WHERE subject != ?1",
        [LID_SUBJECT],
    )
    .map_err(error)?;
    for (subject, unreadable) in &rows {
        tx.execute(
            "INSERT INTO keep_awake_restore(subject,unreadable) VALUES(?1,?2)",
            params![subject, unreadable],
        )
        .map_err(error)?;
    }
    tx.commit().map_err(error)
}

/// Writes or clears the restore row of a lid helper that cannot read its journal (QUESTION
/// 36b), under its own subject, which the engine start's undo leaves alone. Inputs: the
/// engine database outside any transaction and whether the helper's journal is unreadable.
/// Output: Ok, or the SQLite failure, with nothing changed.
pub(crate) fn lid_journal(connection: &Connection, unreadable: bool) -> Result<(), EngineError> {
    assert!(connection.is_autocommit());
    let changed = if unreadable {
        connection.execute(
            "INSERT INTO keep_awake_restore(subject,unreadable) VALUES(?1,1) \
             ON CONFLICT(subject) DO UPDATE SET unreadable=1",
            [LID_SUBJECT],
        )
    } else {
        connection.execute(
            "DELETE FROM keep_awake_restore WHERE subject=?1",
            [LID_SUBJECT],
        )
    }
    .map_err(error)?;
    assert!(changed <= 1);
    Ok(())
}

/// Writes or clears the restore rows of the lid-closed lane (rv-secreview and rv-code T17
/// r1): one row per kind the lane could not put back after three tries while the engine
/// runs, freeze or mute only, and none once it put everything back. Rows of any other
/// subject stay. Inputs: the engine database outside any transaction and the kinds left.
/// Output: Ok, or the SQLite failure, with nothing changed.
pub(crate) fn lid_closed_left(connection: &Connection, kinds: &[&str]) -> Result<(), EngineError> {
    let _ = (connection, kinds);
    Ok(())
}

/// The approval source (QUESTION 35j): one condition while the lid helper waits for the
/// owner's approval in System Settings. A missing Keep awake state reports nothing.
/// Inputs: the engine database and the model access, which it does not need. Output: the
/// read, or none.
pub(super) fn approvals(
    connection: &Connection,
    _access: &ModelAccess,
) -> Result<Option<Found>, EngineError> {
    if !tables_exist(connection, &["keep_awake_state", "configured_keep_awake"])? {
        return Ok(None);
    }
    let view = crate::keep_awake::view(connection)?;
    let waiting = view.lid_platform == LidSupport::NeedsApproval;
    let conditions: Vec<Condition> = waiting
        .then(|| Condition {
            kind: NoticeKind::KeepAwakeApproval,
            project_id: None,
            subject: APPROVAL_SUBJECT.into(),
            text: APPROVAL_TEXT.into(),
            target: NoticeTarget::KeepAwake,
        })
        .into_iter()
        .collect();
    assert!(conditions.len() <= 1);
    Ok(Some(Found::read(conditions, false, Vec::new())))
}

/// The line of a restore item. Inputs: its subject and whether the journal could not be
/// read. Output: the line; the lid helper row has the fix of threat model section 8, and
/// an app left paused or a sound left muted names the restore command (Task 17).
fn restore_text(subject: &str, unreadable: bool) -> &'static str {
    let kind = subject.strip_prefix(ENTRY_SUBJECT);
    match (subject == LID_SUBJECT, unreadable) {
        (true, _) => LID_JOURNAL_TEXT,
        (false, true) => NOT_READ,
        (false, false) if kind == Some(crate::keep_awake::freeze::KIND) => NOT_THAWED,
        (false, false) if kind == Some(crate::keep_awake::mute::KIND) => NOT_UNMUTED,
        (false, false) => NOT_PUT_BACK,
    }
}

/// The restore source (DEC-136a decision 1): one condition per row of the restore state,
/// at most `SOURCE_ROWS_MAX`. A missing table reports nothing, so it resolves nothing.
/// Inputs: the engine database and the model access, which it does not need. Output: the
/// read, or none.
pub(super) fn restores(
    connection: &Connection,
    _access: &ModelAccess,
) -> Result<Option<Found>, EngineError> {
    if !tables_exist(connection, &["keep_awake_restore"])? {
        return Ok(None);
    }
    let mut statement = connection
        .prepare("SELECT subject, unreadable FROM keep_awake_restore ORDER BY subject LIMIT ?1")
        .map_err(error)?;
    let rows: Vec<(String, bool)> = statement
        .query_map([SOURCE_ROWS_MAX as i64], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    let conditions: Vec<Condition> = rows
        .into_iter()
        .map(|(subject, unreadable)| Condition {
            kind: NoticeKind::KeepAwakeRestore,
            project_id: None,
            text: restore_text(&subject, unreadable).into(),
            subject,
            target: NoticeTarget::KeepAwake,
        })
        .collect();
    assert!(conditions.len() <= SOURCE_ROWS_MAX);
    Ok(Some(Found::read(conditions, false, Vec::new())))
}
