use super::{
    FAILURE_BYTES_MAX, Offered, Research, Tab, clipped, evidence, named_tabs, offer, parse_reply,
    prompt, read, unavailable,
};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::roadmap_status::{Claim, Runs};
use rusqlite::{Connection, params};
use std::path::Path;
use std::time::Duration;

/// The longest one writing run may take; a run still marked running after it is closed.
pub const RUN_TIME: Duration = Duration::from_secs(10 * 60);
/// The most writing runs one tick starts.
pub const STARTS_PER_TICK_MAX: usize = 4;
/// Why a run that was closed for running past [`RUN_TIME`] failed.
pub const UNFINISHED: &str = "The last refresh did not finish";
/// Why a run whose reply held no tab failed.
pub const NO_TABS: &str = "The agent's reply held no Overview tab";
/// The most bytes of an agent's reply a run reads; a longer one fails the run unread.
pub const REPLY_BYTES_MAX: usize = 256 * 1024;
/// Why a run whose reply was longer than [`REPLY_BYTES_MAX`] failed.
pub const REPLY_TOO_LONG: &str = "The agent's reply was too long";
/// Why a run failed when none of its tabs could be written.
pub const NOT_WRITTEN: &str = "The Overview tabs could not be written";
/// Why a run failed when no account of the project's pool can run it.
pub const NO_ACCOUNT: &str = "No account in this project's pool can run the refresh";

pub(super) const RUN_MS: i64 = RUN_TIME.as_millis() as i64;

const _: () = assert!(RUN_MS > 0 && STARTS_PER_TICK_MAX >= 1);
const _: () = assert!(UNFINISHED.len() <= FAILURE_BYTES_MAX && NO_TABS.len() <= FAILURE_BYTES_MAX);
const _: () =
    assert!(NOT_WRITTEN.len() <= FAILURE_BYTES_MAX && NO_ACCOUNT.len() <= FAILURE_BYTES_MAX);
const _: () = assert!(REPLY_TOO_LONG.len() <= FAILURE_BYTES_MAX && REPLY_BYTES_MAX > 0);

/// How a writing run ended: the agent's reply with whether competitor research was in its
/// evidence, the words saying why it failed, or nothing to do (the project cannot hold tab
/// files, so no agent was started).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended<'a> {
    Reply {
        reply: &'a str,
        with_competitors: bool,
    },
    Failed(&'a str),
    Skipped,
}

/// One line saying why a run failed, safe to store and show: words that look like
/// credentials masked, hidden characters shown escaped, control characters turned into
/// spaces, trimmed, within [`FAILURE_BYTES_MAX`]. Input: the words. Output: the line.
fn line(words: &str) -> String {
    let shown: String = crate::memory::visible_text(&crate::project_setup::redact(words))
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let cut = clipped(shown.trim().to_owned(), FAILURE_BYTES_MAX);
    let cut = cut.trim_end().to_owned();
    assert!(cut.len() <= FAILURE_BYTES_MAX);
    assert!(!cut.chars().any(char::is_control));
    cut
}

/// The projects whose tabs are due a writing run: stale, past their due time, not running
/// and not switched off, oldest due first. Inputs: the connection and now. Output: at most
/// [`STARTS_PER_TICK_MAX`] projects, or `overview_unavailable`.
fn due(db: &Connection, now_ms: i64) -> Result<Vec<ProjectId>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT project FROM project_overview WHERE stale=1 AND due_ms <= ?1
               AND running_ms IS NULL AND refresh <> 'off'
             ORDER BY due_ms, project LIMIT ?2",
        )
        .map_err(unavailable)?;
    let names = statement
        .query_map(params![now_ms, STARTS_PER_TICK_MAX as i64], |row| {
            row.get::<_, String>(0)
        })
        .map_err(unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(unavailable)?;
    let projects: Vec<ProjectId> = names
        .into_iter()
        .filter_map(|name| ProjectId::new(name).ok())
        .collect();
    assert!(projects.len() <= STARTS_PER_TICK_MAX);
    Ok(projects)
}

/// Claims one project's writing run: no longer stale, running from now, unless it stopped
/// being stale, started running or was switched off since it was listed. Inputs: the
/// connection, the project and now. Output: the claim the run ends with, or none.
pub(super) fn claim(
    db: &Connection,
    project: &ProjectId,
    now_ms: i64,
) -> Result<Option<Claim>, EngineError> {
    assert!(now_ms >= 0);
    let claimed = db
        .execute(
            "UPDATE project_overview SET stale=0, running_ms=?2
             WHERE project=?1 AND stale=1 AND running_ms IS NULL AND refresh <> 'off'",
            params![project.as_str(), now_ms],
        )
        .map_err(unavailable)?;
    assert!(claimed <= 1);
    Ok((claimed == 1).then_some(Claim::at(now_ms)))
}

/// Starts the writing runs that are due: a run past [`RUN_TIME`] is closed with
/// [`UNFINISHED`], then each due project is claimed and handed to `runs`. A landing
/// during a run marks the project stale again, so it is written once more after the run.
/// Inputs: the connection, the runs and now. Output: how many runs started, or
/// `overview_unavailable`.
pub fn tick(db: &Connection, runs: &mut dyn Runs, now_ms: i64) -> Result<usize, EngineError> {
    assert!(now_ms >= 0);
    db.execute(
        "UPDATE project_overview SET running_ms=NULL, failure=?1
         WHERE running_ms IS NOT NULL AND running_ms <= ?2",
        params![UNFINISHED, now_ms.saturating_sub(RUN_MS)],
    )
    .map_err(unavailable)?;
    let mut started = 0;
    for project in due(db, now_ms)? {
        if let Some(claimed) = claim(db, &project, now_ms)? {
            runs.start(&project, claimed);
            started += 1;
        }
    }
    assert!(started <= STARTS_PER_TICK_MAX);
    Ok(started)
}

/// What a claimed run asks its agent: the evidence and the prompt built on it, with each
/// current tab and whether the owner edited it; the Competitors tab is asked for and shown
/// only when competitor research exists. Inputs: the connection, the project, its folder,
/// the competitor research read, the project's file paths and now. Output: the prompt and
/// whether competitor research was in it, or `overview_unavailable`.
pub fn brief(
    db: &Connection,
    project: &ProjectId,
    folder: &Path,
    research: &[Research],
    tree: &[String],
    now_ms: i64,
) -> Result<(String, bool), EngineError> {
    let with_competitors = !research.is_empty();
    let current: Vec<(Tab, String, bool)> = read(db, project, folder, now_ms)?
        .tabs
        .into_iter()
        .filter(|state| with_competitors || state.tab != Tab::Competitors)
        .filter_map(|state| Some((state.tab, state.text?, state.edited)))
        .collect();
    assert!(current.len() <= Tab::ALL.len());
    let text = prompt(
        &evidence(folder, research, tree),
        &current,
        with_competitors,
    );
    Ok((text, with_competitors))
}

/// What a run says when its reply named tabs it could not use: too long, blank, cut off
/// or refused by the tab's file. Input: how many. Output: the line, none for zero.
fn left_note(left: usize) -> Option<String> {
    let note = match left {
        0 => return None,
        1 => "1 tab in the reply could not be used and stayed as it was".to_owned(),
        more => format!("{more} tabs in the reply could not be used and stayed as they were"),
    };
    assert!(note.len() <= FAILURE_BYTES_MAX);
    Some(note)
}

/// Offers the tabs of a reply. Inputs: the connection, the project, its folder, the reply
/// and whether competitor research was in the evidence (without it a Competitors tab in
/// the reply is dropped). Output: Ok when a tab was written, proposed or already the same,
/// with a line when the reply named more tabs than could be used, else the words saying
/// why the run failed.
fn offered(
    db: &Connection,
    project: &ProjectId,
    folder: &Path,
    reply: &str,
    with_competitors: bool,
) -> Result<Option<String>, &'static str> {
    if reply.len() > REPLY_BYTES_MAX {
        return Err(REPLY_TOO_LONG);
    }
    let asked = |tab: &Tab| with_competitors || *tab != Tab::Competitors;
    let tabs: Vec<(Tab, String)> = parse_reply(reply)
        .into_iter()
        .filter(|(tab, _)| asked(tab))
        .collect();
    if tabs.is_empty() {
        return Err(NO_TABS);
    }
    let mut taken = 0usize;
    for (tab, text) in &tabs {
        let outcome: Result<Offered, EngineError> = offer(db, project, folder, *tab, text);
        match outcome {
            Ok(_) => taken += 1,
            Err(error) => eprintln!(
                "project_overview_tab_not_offered cause={}",
                crate::protocol::plain_code(&error.0)
            ),
        }
    }
    assert!(taken <= tabs.len());
    if taken == 0 {
        return Err(NOT_WRITTEN);
    }
    let named = named_tabs(reply)
        .into_iter()
        .filter(|tab| asked(tab))
        .count();
    assert!(taken <= named, "every tab taken was named in the reply");
    Ok(left_note(named - taken))
}

/// Ends the run a claim names, and only that run. The run is closed first, by its claim:
/// a run that was closed for its time, or whose project is gone, ends nothing and writes
/// no tab. A reply's tabs are offered only after that, then the time is stored, with a
/// line when some named tab could not be used; a failure stores one plain line and is not
/// retried until the next landing or Refresh; a skip only closes the run. A reply whose
/// offers do not finish leaves the run closed as unfinished. Inputs: the connection, the
/// project, its folder, the claim, how the run ended and now. Output: whether this run
/// was the one in flight, or `overview_unavailable`.
pub fn finish(
    db: &Connection,
    project: &ProjectId,
    folder: &Path,
    claim: Claim,
    ended: Ended<'_>,
    now_ms: i64,
) -> Result<bool, EngineError> {
    assert!(claim.ms() >= 0 && now_ms >= 0);
    let closing = match ended {
        Ended::Reply { .. } => Some(UNFINISHED.to_owned()),
        Ended::Failed(words) => Some(line(words)),
        Ended::Skipped => None,
    };
    let closed = db
        .execute(
            "UPDATE project_overview SET running_ms=NULL, failure=?3
             WHERE project=?1 AND running_ms=?2",
            params![project.as_str(), claim.ms(), closing],
        )
        .map_err(unavailable)?;
    assert!(closed <= 1);
    if closed == 0 {
        return Ok(false);
    }
    let Ended::Reply {
        reply,
        with_competitors,
    } = ended
    else {
        return Ok(true);
    };
    let (failure, checked) = match offered(db, project, folder, reply, with_competitors) {
        Ok(left) => (left, true),
        Err(words) => (Some(words.to_owned()), false),
    };
    let stored = db
        .execute(
            "UPDATE project_overview
             SET failure=?2, checked_ms=CASE WHEN ?3 THEN ?4 ELSE checked_ms END
             WHERE project=?1 AND running_ms IS NULL",
            params![project.as_str(), failure, checked, now_ms],
        )
        .map_err(unavailable)?;
    assert!(stored <= 1);
    Ok(true)
}

/// Ends the run a claim names with the words saying why it failed, for a run that never
/// reached its project folder. Inputs: the connection, the project, the claim, the words
/// and now. Output: whether this run was the one in flight, or `overview_unavailable`.
pub fn fail(
    db: &Connection,
    project: &ProjectId,
    claim: Claim,
    words: &str,
    now_ms: i64,
) -> Result<bool, EngineError> {
    finish(
        db,
        project,
        Path::new(""),
        claim,
        Ended::Failed(words),
        now_ms,
    )
}

/// Puts the run holding `claim` back for a later try: no longer running, stale again and
/// due at `due_ms`, for a run the engine stopped or whose account was busy or paused. A run
/// that was closed or replaced changes nothing. Inputs: the connection, the project, the
/// claim and when to try again. Output: whether this run was the one in flight, or
/// `overview_unavailable`.
pub fn requeue(
    db: &Connection,
    project: &ProjectId,
    claim: Claim,
    due_ms: i64,
) -> Result<bool, EngineError> {
    assert!(due_ms >= 0 && claim.ms() >= 0);
    let changed = db
        .execute(
            "UPDATE project_overview SET running_ms=NULL, stale=1, due_ms=?3
             WHERE project=?1 AND running_ms=?2",
            params![project.as_str(), claim.ms(), due_ms],
        )
        .map_err(unavailable)?;
    assert!(changed <= 1);
    Ok(changed == 1)
}

/// Makes the Overview tables and closes every run an engine that stopped left open: a run
/// lives on a thread of its engine, so at engine start none can still report, and each is
/// closed with [`UNFINISHED`] without a new try. Input: the connection. Output: how many
/// runs were closed, or `overview_unavailable`.
pub fn close_orphans(db: &Connection) -> Result<usize, EngineError> {
    super::initialize(db)?;
    let closed = db
        .execute(
            "UPDATE project_overview SET running_ms=NULL, failure=?1
             WHERE running_ms IS NOT NULL",
            [UNFINISHED],
        )
        .map_err(unavailable)?;
    let open: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM project_overview WHERE running_ms IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    assert_eq!(open, 0, "no run stays open once every open one is closed");
    Ok(closed)
}
