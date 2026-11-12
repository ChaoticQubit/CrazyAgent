use super::error;
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};

/// The owner's Sub-agent rounds per message choices besides Until the goal is done, the
/// default, which is stored as none (DEC-147.5, F09).
pub const ROUND_CHOICES: [u8; 6] = [1, 2, 3, 5, 10, 20];
/// The owner's Master run time choices besides No limit, in seconds: 1, 3, 7, 14 and 30
/// days (DEC-147.2, F01).
pub const RUN_CHOICES_SECS: [u32; 5] = [86_400, 259_200, 604_800, 1_209_600, 2_592_000];
/// The Master run time a project has until the owner picks one: 7 days.
pub const RUN_DEFAULT_SECS: u32 = RUN_CHOICES_SECS[2];
/// The words an owner message holds, after trimming and lowercasing, to lift the round
/// limit for the rest of its goal.
pub const KEEP_GOING: &str = "keep going";
/// The longest round line a results message to the master ends with, in bytes.
pub const ROUND_LINE_BYTES_MAX: usize = 192;
/// The characters trimmed from both ends of an owner message before it is matched against
/// `KEEP_GOING`, as SQLite's `trim` takes them.
const KEEP_GOING_TRIM: &str = " .!\t\r\n";
const SECS_PER_DAY: u32 = 86_400;
/// What follows `master::LIVE_STOPPED` on the line a live master stopped at its run time
/// leaves.
const RUN_WHY: &str = ": it ran ";

const _: () = assert!(RUN_DEFAULT_SECS == 7 * SECS_PER_DAY);
const _: () = assert!(ROUND_CHOICES[0] >= 1);

/// A project's master limits: the sub-agent rounds one owner goal allows, none for Until
/// the goal is done, and how long its live master runs without the owner, in seconds, none
/// for No limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MasterLimits {
    pub rounds: Option<u8>,
    pub run_secs: Option<u32>,
}

impl Default for MasterLimits {
    fn default() -> Self {
        Self {
            rounds: None,
            run_secs: Some(RUN_DEFAULT_SECS),
        }
    }
}

/// The sub-agent rounds of the owner's current goal: how many master turns handed work to
/// sub-agents since the goal started, all hand-offs of one turn being one round, the owner's
/// limit, none for Until the goal is done, and whether the owner said keep going after the
/// goal started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rounds {
    pub used: u32,
    pub max: Option<u8>,
    pub kept_going: bool,
}

impl Rounds {
    /// Whether the master may hand over more work now: no limit, a keep going for this
    /// goal, or fewer rounds used than the limit. Output: the answer.
    pub fn open(self) -> bool {
        self.kept_going || self.max.is_none_or(|max| self.used < u32::from(max))
    }

    /// The line a results message to the master ends with: the rounds the owner's current
    /// goal used so far and the limit. Output: one line starting with a newline, at most
    /// `ROUND_LINE_BYTES_MAX` bytes.
    pub fn results_line(self) -> String {
        let used = self.used;
        let line = match (self.max, self.kept_going) {
            (None, _) => {
                format!("\nSub-agent rounds for the owner's current goal: {used}. No round limit.")
            }
            (Some(_), true) => format!(
                "\nSub-agent rounds for the owner's current goal: {used}. The owner said keep going, so no round limit."
            ),
            (Some(max), false) if used < u32::from(max) => {
                format!("\nSub-agent rounds for the owner's current goal: {used} of {max}.")
            }
            (Some(max), false) => format!(
                "\nSub-agent rounds for the owner's current goal: {used} of {max}, the limit: hand over no new work until the owner writes."
            ),
        };
        assert!(line.len() <= ROUND_LINE_BYTES_MAX);
        line
    }

    /// The note the owner reads when the master asked for more work past the limit. Takes
    /// rounds that are not open. Output: one plain line.
    pub fn limit_note(self) -> String {
        assert!(!self.open());
        let max = self.max.expect("a closed round count has a limit");
        format!(
            "The master used all {} for your last message. Reply keep going to let it finish this goal, or send a new message.",
            rounds_words(Some(max))
        )
    }

    /// What the master's own team call to hand work over reads past the limit, so a live
    /// master stops there too. Takes rounds that are not open. Output: one plain line.
    pub fn refusal(self) -> String {
        assert!(!self.open());
        let max = self.max.expect("a closed round count has a limit");
        format!(
            "Not handed off: the owner's current goal used all {} the owner allows for one message. Tell the owner what was done; a reply of keep going lets you finish the goal.",
            rounds_words(Some(max))
        )
    }
}

/// What a master view shows about a project's master limits, read from its team status:
/// the rounds of the owner's current goal, the Master run time in seconds, none for No
/// limit, and the running live master's deadline in milliseconds, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitsView {
    pub rounds: Rounds,
    pub run_secs: Option<u32>,
    pub deadline_ms: Option<i64>,
}

impl LimitsView {
    /// Words the time the live master has left and the rounds of the current goal, as both
    /// master views show them. Takes the time now in milliseconds. Output: such as "6 d 23 h
    /// left. Sub-agent rounds: 2 of 3."
    pub fn words(self, now: i64) -> String {
        let Rounds {
            used,
            max,
            kept_going,
        } = self.rounds;
        let rounds = match (max, kept_going) {
            (Some(_), true) => format!("Sub-agent rounds: {used}, no limit for this goal"),
            (None, _) => format!("Sub-agent rounds: {used}"),
            (Some(max), false) => format!("Sub-agent rounds: {used} of {max}"),
        };
        match (self.run_secs, self.deadline_ms) {
            (None, _) => format!("No time limit. {rounds}."),
            (Some(_), None) => format!("{rounds}."),
            (Some(_), Some(deadline)) => {
                format!(
                    "{}. {rounds}.",
                    time_left_words(deadline.saturating_sub(now))
                )
            }
        }
    }
}

/// Words the time a live master has left before its Master run time. Input: the
/// milliseconds left. Output: such as "6 d 23 h left", "5 h 10 min left" or "Time is up".
pub fn time_left_words(left: i64) -> String {
    if left <= 0 {
        return "Time is up; it stops if it works on without you".to_owned();
    }
    let minutes = left.saturating_add(59_999) / 60_000;
    assert!(minutes >= 1);
    let (days, hours) = (minutes / 1440, (minutes % 1440) / 60);
    assert!(days >= 0 && (0..24).contains(&hours));
    if days > 0 {
        format!("{days} d {hours} h left")
    } else if hours > 0 {
        format!("{hours} h {} min left", minutes % 60)
    } else {
        format!("{minutes} min left")
    }
}

/// Creates the master limit tables: the owner's choices per project and the clock of each
/// running live master, which counts from its pane's start or the owner's last team action,
/// whichever is later. Takes the engine database.
/// Returns any SQLite failure.
pub fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS team_master_limits (
                project TEXT PRIMARY KEY,
                rounds INTEGER CHECK(rounds IS NULL OR rounds BETWEEN 1 AND 255),
                run_secs INTEGER CHECK(run_secs IS NULL OR run_secs >= 0)
            );
            CREATE TABLE IF NOT EXISTS team_master_clocks (
                project TEXT PRIMARY KEY,
                agent INTEGER NOT NULL CHECK(agent > 0),
                since_ms INTEGER NOT NULL CHECK(since_ms >= 0)
            );",
        )
        .map_err(error)
}

/// Reads a project's master limits. Takes a connection and the project. Returns Until the
/// goal is done and 7 days until the owner picks, or `team_rounds_invalid` or
/// `team_run_time_invalid` for a stored value outside the choices.
pub fn read(connection: &Connection, project: &ProjectId) -> Result<MasterLimits, EngineError> {
    let row: Option<(Option<i64>, Option<i64>)> = connection
        .query_row(
            "SELECT rounds,run_secs FROM team_master_limits WHERE project=?1",
            [project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((rounds, run_secs)) = row else {
        return Ok(MasterLimits::default());
    };
    let rounds = rounds
        .map(|value| {
            u8::try_from(value)
                .ok()
                .filter(|value| ROUND_CHOICES.contains(value))
                .ok_or_else(|| error("team_rounds_invalid"))
        })
        .transpose()?;
    let run_secs = match run_secs {
        None => Some(RUN_DEFAULT_SECS),
        Some(0) => None,
        Some(value) => Some(
            u32::try_from(value)
                .ok()
                .filter(|value| RUN_CHOICES_SECS.contains(value))
                .ok_or_else(|| error("team_run_time_invalid"))?,
        ),
    };
    Ok(MasterLimits { rounds, run_secs })
}

/// Saves the owner's Sub-agent rounds per message. Takes a connection, the project and a
/// listed choice, none for Until the goal is done. Returns any SQLite failure.
pub(crate) fn set_rounds(
    connection: &Connection,
    project: &ProjectId,
    max: Option<u8>,
) -> Result<(), EngineError> {
    assert!(max.is_none_or(|max| ROUND_CHOICES.contains(&max)));
    connection
        .execute(
            "INSERT INTO team_master_limits(project,rounds) VALUES (?1,?2)
             ON CONFLICT(project) DO UPDATE SET rounds=excluded.rounds",
            params![project.as_str(), max],
        )
        .map_err(error)?;
    Ok(())
}

/// Saves the owner's Master run time, No limit as zero so it differs from the unset
/// default. Takes a connection, the project and a listed choice in seconds, none for No
/// limit. Returns any SQLite failure.
pub(crate) fn set_run_time(
    connection: &Connection,
    project: &ProjectId,
    secs: Option<u32>,
) -> Result<(), EngineError> {
    assert!(secs.is_none_or(|secs| RUN_CHOICES_SECS.contains(&secs)));
    connection
        .execute(
            "INSERT INTO team_master_limits(project,run_secs) VALUES (?1,?2)
             ON CONFLICT(project) DO UPDATE SET run_secs=excluded.run_secs",
            params![project.as_str(), i64::from(secs.unwrap_or(0))],
        )
        .map_err(error)?;
    Ok(())
}

/// Counts the sub-agent rounds of the owner's current goal. The goal starts at the owner's
/// last team message that is not keep going, or at the owner's last key in the live
/// master's terminal when that is later; a round is one master turn that handed work over
/// after that start, read as the distinct turn keys of the master's top-level tasks: a
/// reply's key is its message id, ordered against the goal message, and a live turn's is
/// below zero and timed. Takes a connection and the project. Returns the rounds with the
/// owner's limit, or any SQLite failure.
pub fn rounds(connection: &Connection, project: &ProjectId) -> Result<Rounds, EngineError> {
    let max = read(connection, project)?.rounds;
    let (goal, typed, start) = goal_start(connection, project)?;
    let (used, kept_going): (i64, bool) = connection
        .query_row(
            "SELECT
                 (SELECT COUNT(DISTINCT turn) FROM team_tasks WHERE project=?1 AND source='master'
                  AND parent IS NULL AND ((turn>0 AND turn>?2 AND created_ms>?3)
                                          OR (turn<0 AND created_ms>=?4))),
                 EXISTS(SELECT 1 FROM team_messages WHERE project=?1 AND author='owner'
                  AND id>?2 AND created_ms>?3 AND lower(trim(text,?5))=?6)",
            params![
                project.as_str(),
                goal,
                typed,
                start,
                KEEP_GOING_TRIM,
                KEEP_GOING
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(error)?;
    assert!(used >= 0);
    Ok(Rounds {
        used: u32::try_from(used).unwrap_or(u32::MAX),
        max,
        kept_going,
    })
}

/// Whether a hand-off key already holds a round of the owner's current goal, so more work
/// handed over in that same master turn joins it instead of starting another. Takes a
/// connection, the project and the key. Returns the answer, or any SQLite failure.
pub(crate) fn joins_round(
    connection: &Connection,
    project: &ProjectId,
    key: i64,
) -> Result<bool, EngineError> {
    assert!(key != 0);
    let (goal, typed, start) = goal_start(connection, project)?;
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM team_tasks WHERE project=?1 AND source='master'
             AND parent IS NULL AND turn=?5 AND ((turn>0 AND turn>?2 AND created_ms>?3)
                                                 OR (turn<0 AND created_ms>=?4)))",
            params![project.as_str(), goal, typed, start, key],
            |row| row.get(0),
        )
        .map_err(error)
}

/// The hand-off key of a live master's turn: its turn number negated, so it never equals
/// the reply message id that keys a master reply's hand-off. Input: the turn number, from
/// one up, as `master::turn_ended` counts them. Output: the key, below zero.
pub(crate) fn live_turn_key(turn: i64) -> i64 {
    assert!(turn > 0);
    -turn
}

/// The hand-off key of a project's live master's open turn: the turn that ends next, one
/// past the turns ended so far. Takes a connection and the project. Returns the key, below
/// zero, or any SQLite failure.
pub(crate) fn live_key(connection: &Connection, project: &ProjectId) -> Result<i64, EngineError> {
    let ended: i64 = connection
        .query_row(
            "SELECT turns FROM team_live_masters WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?
        .unwrap_or(0);
    assert!(ended >= 0);
    Ok(live_turn_key(ended.saturating_add(1)))
}

/// Where the owner's current goal starts: the id of the owner's last team message that is
/// not keep going, zero for none, the owner's last key in the live master's terminal and
/// the later of the two times. Takes a connection and the project. Returns the three, or
/// any SQLite failure.
fn goal_start(
    connection: &Connection,
    project: &ProjectId,
) -> Result<(i64, i64, i64), EngineError> {
    let (goal, sent): (i64, i64) = connection
        .query_row(
            "SELECT id,created_ms FROM team_messages WHERE project=?1 AND author='owner'
             AND lower(trim(text,?2))<>?3 ORDER BY id DESC LIMIT 1",
            params![project.as_str(), KEEP_GOING_TRIM, KEEP_GOING],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?
        .unwrap_or((0, 0));
    let typed = typed_ms(connection, project)?;
    assert!(goal >= 0 && typed >= 0);
    Ok((goal, typed, sent.max(typed)))
}

/// Starts the clock of a project's live master at its pane's start, on a fresh launch and on
/// an adopt alike, so no restart carries an earlier start. Takes a connection, the project,
/// the master's agent id and the time in milliseconds. Returns any SQLite failure.
pub(crate) fn clock_started(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
    now: i64,
) -> Result<(), EngineError> {
    assert!(agent > 0 && now >= 0);
    connection
        .execute(
            "INSERT INTO team_master_clocks(project,agent,since_ms) VALUES (?1,?2,?3)
             ON CONFLICT(project) DO UPDATE SET agent=excluded.agent, since_ms=excluded.since_ms",
            params![project.as_str(), agent, now],
        )
        .map_err(error)?;
    Ok(())
}

/// Renews a project's live master run time on an owner team action, such as an answer to
/// an agent's question: its clock counts from now when that is later than its pane's start
/// or the owner's last action. Takes a connection, the project and the time in
/// milliseconds. Returns any SQLite failure.
pub(crate) fn owner_acted(
    connection: &Connection,
    project: &ProjectId,
    now: i64,
) -> Result<(), EngineError> {
    assert!(now >= 0);
    let renewed = connection
        .execute(
            "UPDATE team_master_clocks SET since_ms=MAX(since_ms,?2) WHERE project=?1",
            params![project.as_str(), now],
        )
        .map_err(error)?;
    assert!(renewed <= 1);
    Ok(())
}

/// Removes the clock of a live master whose pane ended; another agent's clock is left alone.
/// Takes a connection, the project and the agent id. Returns any SQLite failure.
pub(crate) fn clock_ended(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<(), EngineError> {
    assert!(agent > 0);
    connection
        .execute(
            "DELETE FROM team_master_clocks WHERE project=?1 AND agent=?2",
            params![project.as_str(), agent],
        )
        .map_err(error)?;
    Ok(())
}

/// When a project's live master reaches its run time: the latest of its pane's start, the
/// owner's last team action, the owner's last team message and the owner's last key in its
/// terminal, plus the owner's
/// Master run time. The runtime's stop and the time left the clients show both read it.
/// Takes a connection and the project. Returns the deadline in milliseconds, none under No
/// limit or while no live master pane runs, or any SQLite failure.
pub fn run_deadline(
    connection: &Connection,
    project: &ProjectId,
) -> Result<Option<i64>, EngineError> {
    let Some(secs) = read(connection, project)?.run_secs else {
        return Ok(None);
    };
    let typed = typed_ms(connection, project)?;
    let since: Option<i64> = connection
        .query_row(
            "SELECT MAX(since_ms,
                 COALESCE((SELECT MAX(created_ms) FROM team_messages
                           WHERE project=?1 AND author='owner'),0),
                 ?2)
             FROM team_master_clocks WHERE project=?1",
            params![project.as_str(), typed],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    Ok(since.map(|since| {
        assert!(since >= 0);
        since.saturating_add(i64::from(secs).saturating_mul(1000))
    }))
}

/// Reads the owner's last key in a project's live master terminal. Takes a connection and
/// the project. Returns the time in milliseconds, zero when none was seen, or any SQLite
/// failure.
fn typed_ms(connection: &Connection, project: &ProjectId) -> Result<i64, EngineError> {
    let typed: Option<Option<i64>> = connection
        .query_row(
            "SELECT typed_ms FROM team_live_masters WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    Ok(typed.flatten().unwrap_or(0).max(0))
}

/// Words for a Sub-agent rounds per message choice. Input: the choice, none for Until the
/// goal is done. Output: for example "Until the goal is done" or "3 sub-agent rounds".
pub fn rounds_words(max: Option<u8>) -> String {
    match max {
        None => "Until the goal is done".to_owned(),
        Some(1) => "1 sub-agent round".to_owned(),
        Some(max) => format!("{max} sub-agent rounds"),
    }
}

/// Words for a Master run time choice. Input: the choice in seconds, none for No limit.
/// Output: for example "No limit", "1 day" or "7 days".
pub fn run_words(secs: Option<u32>) -> String {
    match secs {
        None => "No limit".to_owned(),
        Some(secs) if secs.is_multiple_of(SECS_PER_DAY) && secs / SECS_PER_DAY == 1 => {
            "1 day".to_owned()
        }
        Some(secs) if secs.is_multiple_of(SECS_PER_DAY) => {
            format!("{} days", secs / SECS_PER_DAY)
        }
        Some(secs) => super::span_words(u64::from(secs)),
    }
}

/// The line a live master stopped at its run time leaves: it ran that long without the
/// owner and continues the same conversation on its next start. Input: the run time in
/// seconds. Output: the line.
pub(crate) fn run_stopped_line(secs: u32) -> String {
    let line = format!(
        "{}{RUN_WHY}{} without a message from you. It continues the same conversation when you start it again.",
        super::master::LIVE_STOPPED,
        run_words(Some(secs))
    );
    assert!(run_stopped(&line));
    line
}

/// Whether a problem line is the one `run_stopped_line` leaves, so the master view offers
/// Start again as it does after an idle stop. Input: the line. Output: the answer.
pub(crate) fn run_stopped(problem: &str) -> bool {
    problem
        .strip_prefix(super::master::LIVE_STOPPED)
        .is_some_and(|rest| rest.starts_with(RUN_WHY))
}

#[cfg(test)]
mod tests;
