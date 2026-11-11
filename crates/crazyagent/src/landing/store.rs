use super::git::failure;
use super::{AgentKind, ChangeAgent, ChangeSetId, ChangeSetView, ChangeState, TurnOutcome};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS landing_change_sets (
    id TEXT PRIMARY KEY,
    project TEXT NOT NULL,
    agent_kind TEXT NOT NULL CHECK(agent_kind IN ('master','sub')),
    agent_id TEXT NOT NULL,
    label TEXT NOT NULL,
    repository TEXT NOT NULL,
    worktree TEXT NOT NULL,
    git_dir TEXT NOT NULL,
    base TEXT NOT NULL,
    head TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('pending','conflict','updating','accepted','discarded')),
    last_outcome TEXT NOT NULL CHECK(last_outcome IN ('succeeded','failed','cancelled')),
    turn TEXT NOT NULL,
    prompt_line TEXT NOT NULL,
    files INTEGER NOT NULL,
    added INTEGER NOT NULL,
    removed INTEGER NOT NULL,
    conflicts TEXT NOT NULL,
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL,
    restorable_until_ms INTEGER,
    pruned INTEGER NOT NULL DEFAULT 0 CHECK(pruned IN (0,1)),
    landed TEXT,
    intent TEXT,
    message TEXT NOT NULL DEFAULT '',
    waiting INTEGER NOT NULL DEFAULT 0 CHECK(waiting IN (0,1)),
    outside INTEGER NOT NULL DEFAULT 0 CHECK(outside IN (0,1)),
    made INTEGER NOT NULL DEFAULT 0 CHECK(made IN (0,1)),
    undone TEXT,
    landed_on TEXT,
    restored INTEGER NOT NULL DEFAULT 0 CHECK(restored IN (0,1)),
    regenerated_head TEXT,
    regenerated_files TEXT NOT NULL DEFAULT '[]',
    accepted_in TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS landing_change_sets_open
    ON landing_change_sets(worktree) WHERE state IN ('pending','conflict','updating');
CREATE INDEX IF NOT EXISTS landing_change_sets_project
    ON landing_change_sets(project, updated_ms);
CREATE TABLE IF NOT EXISTS landing_bases (
    worktree TEXT PRIMARY KEY,
    base TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS landing_agent_branches (
    worktree TEXT PRIMARY KEY,
    repository TEXT NOT NULL,
    branch TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS landing_agent_branches_repository
    ON landing_agent_branches(repository, branch);
CREATE TABLE IF NOT EXISTS landing_change_set_numbers (
    change_set TEXT PRIMARY KEY REFERENCES landing_change_sets(id),
    project TEXT NOT NULL,
    number INTEGER NOT NULL CHECK(number >= 1),
    UNIQUE(project, number)
);";

const NUMBER_PASS_MAX: i64 = 10_000;

const COLUMNS: &str = "id,project,agent_kind,agent_id,label,repository,worktree,git_dir,base,head,\
    state,last_outcome,turn,prompt_line,files,added,removed,conflicts,created_ms,updated_ms,\
    restorable_until_ms,pruned,landed,intent,message,waiting,outside,made,undone,landed_on,\
    restored,regenerated_head,regenerated_files,accepted_in";
const ADDED_COLUMNS: [(&str, &str); 10] = [
    ("message", "message TEXT NOT NULL DEFAULT ''"),
    (
        "waiting",
        "waiting INTEGER NOT NULL DEFAULT 0 CHECK(waiting IN (0,1))",
    ),
    (
        "outside",
        "outside INTEGER NOT NULL DEFAULT 0 CHECK(outside IN (0,1))",
    ),
    (
        "made",
        "made INTEGER NOT NULL DEFAULT 0 CHECK(made IN (0,1))",
    ),
    ("undone", "undone TEXT"),
    ("landed_on", "landed_on TEXT"),
    (
        "restored",
        "restored INTEGER NOT NULL DEFAULT 0 CHECK(restored IN (0,1))",
    ),
    ("regenerated_head", "regenerated_head TEXT"),
    (
        "regenerated_files",
        "regenerated_files TEXT NOT NULL DEFAULT '[]'",
    ),
    ("accepted_in", "accepted_in TEXT"),
];

/// A step of owner-checkout or worktree work the engine recorded before starting it, so
/// an engine start can finish or undo it. `Land` moves the owner's `reference` from
/// `old` to the squash commit `new`, `master` when it is the master landing its own set;
/// `Reset` points the agent's worktree at `target` only while its HEAD is still `from`
/// (an intent stored before `from` existed moves it only to finish a step whose files
/// already moved), and `earlier` is what an open set goes back to when it does not happen;
/// `Undo` moves `reference` from `old` to the commit `new` that reverts the set's landed
/// commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Intent {
    Land {
        reference: String,
        old: String,
        new: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        master: bool,
    },
    Reset {
        #[serde(default)]
        from: Option<String>,
        target: String,
        #[serde(default)]
        earlier: Option<Earlier>,
    },
    Undo {
        reference: String,
        old: String,
        new: String,
    },
}

/// What a change set held before a reset that may never happen: the fields an update, a
/// restore or a regeneration changes, so an engine start can put them back as the step's
/// own refusal does. An open state means the set's pending ref named `head`, any other
/// state that it had none. A copy of a set never regenerated is written without the
/// regenerated fields, so an older engine still reads it after a rollback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Earlier {
    state: ChangeState,
    waiting: bool,
    base: String,
    head: String,
    conflicts: Vec<String>,
    files: u32,
    added: u32,
    removed: u32,
    updated_ms: u64,
    restorable_until_ms: Option<u64>,
    #[serde(default)]
    restored: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    regenerated_head: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    regenerated_files: Vec<String>,
}

impl Earlier {
    /// Copies what a change set holds now. Input: the set. Output: the copy.
    pub(super) fn of(record: &Record) -> Self {
        Self {
            state: record.state,
            waiting: record.waiting,
            base: record.base.clone(),
            head: record.head.clone(),
            conflicts: record.conflicts.clone(),
            files: record.files,
            added: record.added,
            removed: record.removed,
            updated_ms: record.updated_ms,
            restorable_until_ms: record.restorable_until_ms,
            restored: record.restored,
            regenerated_head: record.regenerated_head.clone(),
            regenerated_files: record.regenerated_files.clone(),
        }
    }

    /// Tells whether the copy was of a discarded set, as a restore's is. Output: the answer.
    pub(super) fn was_discarded(&self) -> bool {
        self.state == ChangeState::Discarded
    }

    /// Writes the copy back onto a change set, for the caller to save. Input: the set.
    /// Output: none.
    pub(super) fn put_back(self, record: &mut Record) {
        record.state = self.state;
        record.waiting = self.waiting;
        record.base = self.base;
        record.head = self.head;
        record.conflicts = self.conflicts;
        record.files = self.files;
        record.added = self.added;
        record.removed = self.removed;
        record.updated_ms = self.updated_ms;
        record.restorable_until_ms = self.restorable_until_ms;
        record.restored = self.restored;
        record.regenerated_head = self.regenerated_head;
        record.regenerated_files = self.regenerated_files;
    }
}

/// One stored change set with everything the engine needs to act on it. `made` says the
/// set's own Accept committed `landed`, so an undo may revert it, on `landed_on`, the ref
/// that Accept moved; `undone` is the commit that reverted it; `accepted_in` is where the
/// owner pressed that Accept, None for anyone else's, one stored before places were kept, or
/// a place a later build named, which this build reads as None and keeps as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Record {
    pub(super) id: ChangeSetId,
    pub(super) project: ProjectId,
    pub(super) agent: ChangeAgent,
    pub(super) repository: PathBuf,
    pub(super) worktree: PathBuf,
    pub(super) git_dir: PathBuf,
    pub(super) base: String,
    pub(super) head: String,
    pub(super) state: ChangeState,
    pub(super) last_outcome: TurnOutcome,
    pub(super) turn: String,
    pub(super) prompt_line: String,
    pub(super) files: u32,
    pub(super) added: u32,
    pub(super) removed: u32,
    pub(super) conflicts: Vec<String>,
    pub(super) created_ms: u64,
    pub(super) updated_ms: u64,
    pub(super) restorable_until_ms: Option<u64>,
    pub(super) pruned: bool,
    pub(super) landed: Option<String>,
    pub(super) intent: Option<Intent>,
    pub(super) message: String,
    pub(super) waiting: bool,
    pub(super) outside: bool,
    pub(super) made: bool,
    pub(super) undone: Option<String>,
    pub(super) landed_on: Option<String>,
    pub(super) restored: bool,
    pub(super) regenerated_head: Option<String>,
    pub(super) regenerated_files: Vec<String>,
    pub(super) accepted_in: Option<crate::activity::AcceptPlace>,
}

impl Record {
    /// The change set as the owner's Changes shows it. Input: its number, None while an
    /// engine without numbers stored it. Output: the view.
    pub(super) fn view(&self, number: Option<u64>) -> ChangeSetView {
        ChangeSetView {
            id: self.id.clone(),
            agent: self.agent.clone(),
            state: self.state,
            base: self.base.clone(),
            head: self.head.clone(),
            files: self.files,
            added: self.added,
            removed: self.removed,
            last_outcome: self.last_outcome,
            checked: false,
            created_ms: self.created_ms,
            updated_ms: self.updated_ms,
            restorable_until_ms: self.restorable_until_ms,
            conflicts: self.conflicts.clone(),
            message: self.message(),
            waiting: self.waiting && self.state == ChangeState::Updating,
            outside: self.outside,
            number,
            regenerated: if self.regenerated_head.as_ref() == Some(&self.head) {
                self.regenerated_files.clone()
            } else {
                Vec::new()
            },
        }
    }

    /// The commit message an Accept proposes: the one made from the agent's own commits,
    /// else the prompt line, else a line naming the agent, with every character a reviewer
    /// cannot see shown as an escape, so one stored before subjects were escaped is shown
    /// and landed visibly too. Output: the message.
    pub(super) fn message(&self) -> String {
        super::change_set_message(&self.message, &self.prompt_line, &self.agent.label)
    }

    pub(super) fn is_open(&self) -> bool {
        matches!(
            self.state,
            ChangeState::Pending | ChangeState::Conflict | ChangeState::Updating
        )
    }
}

fn storage(_: impl std::fmt::Display) -> EngineError {
    failure("changes_unavailable")
}

/// Creates the store's tables, and adds the columns in `ADDED_COLUMNS` to a store made
/// before them. Input: the store. Output: Ok once the store has every column.
pub(super) fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(storage)?;
    for (name, definition) in ADDED_COLUMNS {
        let has = || {
            db.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('landing_change_sets') WHERE name=?1",
                [name],
                |row| row.get::<_, i64>(0),
            )
            .map_err(storage)
        };
        if has()? == 0
            && db
                .execute_batch(&format!(
                    "ALTER TABLE landing_change_sets ADD COLUMN {definition}"
                ))
                .is_err()
            && has()? == 0
        {
            return Err(failure("changes_unavailable"));
        }
    }
    Ok(())
}

fn kind_name(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Master => "master",
        AgentKind::Sub => "sub",
    }
}

pub(super) fn state_name(state: ChangeState) -> &'static str {
    match state {
        ChangeState::Pending => "pending",
        ChangeState::Conflict => "conflict",
        ChangeState::Updating => "updating",
        ChangeState::Accepted => "accepted",
        ChangeState::Discarded => "discarded",
    }
}

pub(super) fn outcome_name(outcome: TurnOutcome) -> &'static str {
    match outcome {
        TurnOutcome::Succeeded => "succeeded",
        TurnOutcome::Failed => "failed",
        TurnOutcome::Cancelled => "cancelled",
    }
}

fn invalid(index: usize) -> rusqlite::Error {
    rusqlite::Error::InvalidColumnType(
        index,
        "landing_change_sets".into(),
        rusqlite::types::Type::Text,
    )
}

fn read(row: &Row<'_>) -> rusqlite::Result<Record> {
    let text = |index: usize| row.get::<_, String>(index);
    let count = |index: usize| {
        row.get::<_, i64>(index)
            .and_then(|value| u32::try_from(value).map_err(|_| invalid(index)))
    };
    let millis = |index: usize| {
        row.get::<_, i64>(index)
            .and_then(|value| u64::try_from(value).map_err(|_| invalid(index)))
    };
    let state = match text(10)?.as_str() {
        "pending" => ChangeState::Pending,
        "conflict" => ChangeState::Conflict,
        "updating" => ChangeState::Updating,
        "accepted" => ChangeState::Accepted,
        "discarded" => ChangeState::Discarded,
        _ => return Err(invalid(10)),
    };
    let last_outcome = match text(11)?.as_str() {
        "succeeded" => TurnOutcome::Succeeded,
        "failed" => TurnOutcome::Failed,
        "cancelled" => TurnOutcome::Cancelled,
        _ => return Err(invalid(11)),
    };
    let kind = match text(2)?.as_str() {
        "master" => AgentKind::Master,
        "sub" => AgentKind::Sub,
        _ => return Err(invalid(2)),
    };
    let intent = row
        .get::<_, Option<String>>(23)?
        .map(|body| serde_json::from_str(&body).map_err(|_| invalid(23)))
        .transpose()?;
    Ok(Record {
        id: ChangeSetId::new(text(0)?).map_err(|_| invalid(0))?,
        project: ProjectId::new(text(1)?).map_err(|_| invalid(1))?,
        agent: ChangeAgent {
            kind,
            id: text(3)?,
            label: text(4)?,
        },
        repository: PathBuf::from(text(5)?),
        worktree: PathBuf::from(text(6)?),
        git_dir: PathBuf::from(text(7)?),
        base: text(8)?,
        head: text(9)?,
        state,
        last_outcome,
        turn: text(12)?,
        prompt_line: text(13)?,
        files: count(14)?,
        added: count(15)?,
        removed: count(16)?,
        conflicts: serde_json::from_str(&text(17)?).map_err(|_| invalid(17))?,
        created_ms: millis(18)?,
        updated_ms: millis(19)?,
        restorable_until_ms: row
            .get::<_, Option<i64>>(20)?
            .map(|value| u64::try_from(value).map_err(|_| invalid(20)))
            .transpose()?,
        pruned: row.get::<_, bool>(21)?,
        landed: row.get(22)?,
        intent,
        message: text(24)?,
        waiting: row.get::<_, bool>(25)?,
        outside: row.get::<_, bool>(26)?,
        made: row.get::<_, bool>(27)?,
        undone: row.get(28)?,
        landed_on: row.get(29)?,
        restored: row.get::<_, bool>(30)?,
        regenerated_head: row.get(31)?,
        regenerated_files: serde_json::from_str(&text(32)?).map_err(|_| invalid(32))?,
        accepted_in: row
            .get::<_, Option<String>>(33)?
            .and_then(|name| crate::activity::AcceptPlace::parse(&name)),
    })
}

fn path_text(path: &std::path::Path) -> Result<&str, EngineError> {
    path.to_str()
        .ok_or_else(|| failure("changes_git_unsupported"))
}

fn signed(value: u64) -> Result<i64, EngineError> {
    i64::try_from(value).map_err(|_| failure("changes_unavailable"))
}

/// Writes a change set, adding it or updating every column of the row with its id, except
/// that a record with no place keeps the place stored, and gives a set with no number the
/// next number of its project in the same savepoint.
/// Inputs: the store and the record. Output: Ok once stored; a second open change set
/// for the same worktree is refused with `changes_agent_has_pending` and both rows stay.
pub(super) fn save(db: &Connection, record: &Record) -> Result<(), EngineError> {
    within(db, "landing_save", || {
        write(db, record)?;
        number(db, &record.id, &record.project)?;
        Ok(())
    })
}

fn write(db: &Connection, record: &Record) -> Result<(), EngineError> {
    let intent = record
        .intent
        .as_ref()
        .map(|intent| serde_json::to_string(intent).map_err(storage))
        .transpose()?;
    let conflicts = serde_json::to_string(&record.conflicts).map_err(storage)?;
    let regenerated_files = serde_json::to_string(&record.regenerated_files).map_err(storage)?;
    let updates = COLUMNS
        .split(',')
        .filter(|column| *column != "id")
        .map(|column| match column {
            "accepted_in" => "accepted_in=COALESCE(excluded.accepted_in,accepted_in)".to_owned(),
            _ => format!("{column}=excluded.{column}"),
        })
        .collect::<Vec<_>>()
        .join(",");
    db.execute(
        &format!(
            "INSERT INTO landing_change_sets({COLUMNS}) VALUES \
             (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32,?33,?34) \
             ON CONFLICT(id) DO UPDATE SET {updates}"
        ),
        params![
            record.id.as_str(),
            record.project.as_str(),
            kind_name(record.agent.kind),
            record.agent.id,
            record.agent.label,
            path_text(&record.repository)?,
            path_text(&record.worktree)?,
            path_text(&record.git_dir)?,
            record.base,
            record.head,
            state_name(record.state),
            outcome_name(record.last_outcome),
            record.turn,
            record.prompt_line,
            record.files,
            record.added,
            record.removed,
            conflicts,
            signed(record.created_ms)?,
            signed(record.updated_ms)?,
            record.restorable_until_ms.map(signed).transpose()?,
            record.pruned,
            record.landed,
            intent,
            record.message,
            record.waiting,
            record.outside,
            record.made,
            record.undone,
            record.landed_on,
            record.restored,
            record.regenerated_head,
            regenerated_files,
            record.accepted_in.map(crate::activity::AcceptPlace::name),
        ],
    )
    .map_err(|error| match error.sqlite_error_code() {
        Some(rusqlite::ErrorCode::ConstraintViolation) => failure("changes_agent_has_pending"),
        _ => storage(error),
    })?;
    Ok(())
}

/// Gives a change set the next number of its project when it has none yet: the project's
/// highest number plus one. A set that has one keeps it. Inputs: the store, the set and its
/// project. Output: the set's number, at least 1 and held by no other set of the project.
fn number(db: &Connection, id: &ChangeSetId, project: &ProjectId) -> Result<u64, EngineError> {
    db.execute(
        "INSERT INTO landing_change_set_numbers(change_set,project,number) \
         SELECT ?1, ?2, COALESCE(MAX(number), 0) + 1 FROM landing_change_set_numbers \
         WHERE project=?2 ON CONFLICT(change_set) DO NOTHING",
        params![id.as_str(), project.as_str()],
    )
    .map_err(storage)?;
    let (number, holders): (i64, i64) = db
        .query_row(
            "SELECT number, (SELECT COUNT(*) FROM landing_change_set_numbers other \
             WHERE other.project=own.project AND other.number=own.number) \
             FROM landing_change_set_numbers own WHERE change_set=?1",
            [id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(storage)?;
    assert!(number >= 1, "a change set number starts at 1");
    assert_eq!(
        holders, 1,
        "a change set number is held by one set of its project"
    );
    Ok(number.unsigned_abs())
}

/// Reads the number a change set carries. Inputs: the store and the set. Output: the
/// number, or None for a set an engine without numbers stored since the last start.
pub(super) fn number_of(db: &Connection, id: &ChangeSetId) -> Result<Option<u64>, EngineError> {
    let number: Option<i64> = db
        .query_row(
            "SELECT number FROM landing_change_set_numbers WHERE change_set=?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    Ok(number.map(|number| {
        assert!(number >= 1, "a change set number starts at 1");
        number.unsigned_abs()
    }))
}

/// Numbers the change sets of the projects the engine has that carry no number yet, such
/// as the ones an engine without numbers wrote, after each project's highest number, in the
/// order they were made, the id breaking a tie, at most `NUMBER_PASS_MAX` a pass. Input: the
/// store. Output: how many sets got a number.
pub(super) fn number_unnumbered(db: &Connection) -> Result<usize, EngineError> {
    let sets = db
        .prepare(
            "SELECT id, project FROM landing_change_sets \
             WHERE id NOT IN (SELECT change_set FROM landing_change_set_numbers) \
             AND project IN (SELECT id FROM projects) ORDER BY created_ms, id LIMIT ?1",
        )
        .and_then(|mut statement| {
            statement
                .query_map([NUMBER_PASS_MAX], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(storage)?;
    within(db, "landing_numbers", || {
        for (id, project) in &sets {
            let id = ChangeSetId::new(id.clone()).map_err(|_| failure("changes_unavailable"))?;
            let project =
                ProjectId::new(project.clone()).map_err(|_| failure("changes_unavailable"))?;
            number(db, &id, &project)?;
        }
        Ok(sets.len())
    })
}

/// Runs `work` inside a savepoint, kept when it succeeds and undone when it fails, so it
/// nests inside a caller's transaction. Inputs: the store, the savepoint's name and the
/// work. Output: the work's result.
fn within<T>(
    db: &Connection,
    name: &str,
    work: impl FnOnce() -> Result<T, EngineError>,
) -> Result<T, EngineError> {
    db.execute_batch(&format!("SAVEPOINT {name}"))
        .map_err(storage)?;
    match work() {
        Ok(value) => {
            db.execute_batch(&format!("RELEASE {name}"))
                .map_err(storage)?;
            Ok(value)
        }
        Err(error) => {
            let _ = db.execute_batch(&format!("ROLLBACK TO {name}; RELEASE {name}"));
            Err(error)
        }
    }
}

/// Stores the commit the next change set of a worktree opens above. Inputs: the store,
/// the worktree and the commit. Output: Ok once stored.
pub(super) fn set_base(
    db: &Connection,
    worktree: &std::path::Path,
    base: &str,
) -> Result<(), EngineError> {
    assert!(super::git::is_object_id(base));
    db.execute(
        "INSERT OR REPLACE INTO landing_bases(worktree,base) VALUES (?1,?2)",
        params![path_text(worktree)?, base],
    )
    .map_err(storage)?;
    Ok(())
}

/// Reads the commit the next change set of a worktree opens above. Inputs: the store
/// and the worktree path. Output: the commit, None when none is recorded, or
/// `changes_unavailable` for a stored value that is not an object id.
pub(super) fn base_of(db: &Connection, worktree: &str) -> Result<Option<String>, EngineError> {
    let base: Option<String> = db
        .query_row(
            "SELECT base FROM landing_bases WHERE worktree=?1",
            [worktree],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    match base {
        Some(base) if !super::git::is_object_id(&base) => Err(failure("changes_unavailable")),
        base => Ok(base),
    }
}

/// Stores the task branch the engine named for an agent worktree, which its clone or
/// linked worktree then commits on in place of the branch its folder names. Inputs: the
/// store, the owner checkout root, the worktree and the branch, one
/// `agent_git::is_agent_branch` accepts. Output: Ok once stored.
pub(super) fn set_agent_branch(
    db: &Connection,
    repository: &std::path::Path,
    worktree: &std::path::Path,
    branch: &str,
) -> Result<(), EngineError> {
    assert!(crate::agent_git::is_agent_branch(branch));
    db.execute(
        "INSERT OR REPLACE INTO landing_agent_branches(worktree,repository,branch) VALUES (?1,?2,?3)",
        params![path_text(worktree)?, path_text(repository)?, branch],
    )
    .map_err(storage)?;
    Ok(())
}

/// Reads the task branch stored for an agent worktree. Inputs: the store and the worktree
/// path. Output: the branch, None when none is stored, or `changes_unavailable` for a
/// stored value that is not a branch the engine names.
pub(super) fn agent_branch(db: &Connection, worktree: &str) -> Result<Option<String>, EngineError> {
    let branch: Option<String> = db
        .query_row(
            "SELECT branch FROM landing_agent_branches WHERE worktree=?1",
            [worktree],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    match branch {
        Some(branch) if !crate::agent_git::is_agent_branch(&branch) => {
            Err(failure("changes_unavailable"))
        }
        branch => Ok(branch),
    }
}

/// Says whether another agent worktree of the same owner repository already holds a task
/// branch name; a row left for the asking worktree itself does not count. Inputs: the
/// store, the owner checkout root, the asking worktree and the branch. Output: the answer.
pub(super) fn agent_branch_taken(
    db: &Connection,
    repository: &std::path::Path,
    worktree: &std::path::Path,
    branch: &str,
) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT COUNT(*) FROM landing_agent_branches WHERE repository=?1 AND branch=?2 AND worktree<>?3",
        params![path_text(repository)?, branch, path_text(worktree)?],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count > 0)
    .map_err(storage)
}

/// Forgets the task branch of an agent worktree that is gone. Inputs: the store and the
/// worktree path. Output: Ok.
pub(super) fn forget_agent_branch(db: &Connection, worktree: &str) -> Result<(), EngineError> {
    db.execute(
        "DELETE FROM landing_agent_branches WHERE worktree=?1",
        [worktree],
    )
    .map_err(storage)?;
    Ok(())
}

/// Reads one change set. Inputs: the store and the id. Output: the record, or
/// `changes_not_found`.
pub(super) fn load(db: &Connection, id: &ChangeSetId) -> Result<Record, EngineError> {
    find(db, id)?.ok_or_else(|| failure("changes_not_found"))
}

/// Reads one change set when it exists. Inputs: the store and the id. Output: the
/// record, or None when no change set has that id.
pub(super) fn find(db: &Connection, id: &ChangeSetId) -> Result<Option<Record>, EngineError> {
    db.query_row(
        &format!("SELECT {COLUMNS} FROM landing_change_sets WHERE id=?1"),
        [id.as_str()],
        read,
    )
    .optional()
    .map_err(storage)
}

/// Reads the commit the engine landed for the newest accepted change set of a project's
/// repository whose head is the given commit, so a later set starting there is known to
/// start from work already landed. Inputs: the store, the project, the repository and the
/// head. Output: the landed commit, None when no accepted set has that head.
pub(super) fn landed_head(
    db: &Connection,
    project: &ProjectId,
    repository: &std::path::Path,
    head: &str,
) -> Result<Option<String>, EngineError> {
    let landed: Option<Option<String>> = db
        .query_row(
            "SELECT landed FROM landing_change_sets WHERE project=?1 AND repository=?2 \
             AND head=?3 AND state='accepted' AND landed IS NOT NULL \
             ORDER BY updated_ms DESC LIMIT 1",
            params![project.as_str(), path_text(repository)?, head],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    match landed.flatten() {
        Some(landed) if !super::git::is_object_id(&landed) => Err(failure("changes_unavailable")),
        landed => Ok(landed),
    }
}

/// Reads the open change set of one worktree. Inputs: the store and the worktree path.
/// Output: the record, or None when nothing is open there.
pub(super) fn open_for(db: &Connection, worktree: &str) -> Result<Option<Record>, EngineError> {
    db.query_row(
        &format!(
            "SELECT {COLUMNS} FROM landing_change_sets WHERE worktree=?1 \
             AND state IN ('pending','conflict','updating')"
        ),
        [worktree],
        read,
    )
    .optional()
    .map_err(storage)
}

/// The most open change-set heads one project's read returns.
const OPEN_HEADS_MAX: usize = 256;

/// The heads of the project's open change sets, the ones still Pending, Conflict or
/// Updating. Inputs: the store and the project. Output: the heads, at most
/// `OPEN_HEADS_MAX`, or `changes_unavailable` on a SQLite failure.
pub(super) fn open_heads(db: &Connection, project: &ProjectId) -> Result<Vec<String>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT head FROM landing_change_sets WHERE project=?1 \
             AND state IN ('pending','conflict','updating') ORDER BY id LIMIT ?2",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map(params![project.as_str(), OPEN_HEADS_MAX as i64], |row| {
            row.get(0)
        })
        .map_err(storage)?;
    let heads: Vec<String> = rows.collect::<Result<_, _>>().map_err(storage)?;
    assert!(heads.len() <= OPEN_HEADS_MAX);
    Ok(heads)
}

/// Reads the worktrees of the change sets open directly inside one folder, such as a
/// lead's teammates' folder, in path order. Inputs: the store, the folder path ending in
/// `/` and the most worktrees to read. Output: the worktree paths.
pub(super) fn open_inside(
    db: &Connection,
    folder: &str,
    max: usize,
) -> Result<Vec<String>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT worktree FROM landing_change_sets \
             WHERE substr(worktree,1,length(?1))=?1 AND length(worktree)>length(?1) \
             AND instr(substr(worktree,length(?1)+1),'/')=0 \
             AND state IN ('pending','conflict','updating') ORDER BY worktree LIMIT ?2",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map(params![folder, max as i64], |row| row.get(0))
        .map_err(storage)?;
    rows.collect::<Result<_, _>>().map_err(storage)
}

/// Reads the change set of one team sub-agent of a project changed last. Inputs: the
/// store, the project and the agent id. Output: the record, None when it has none.
pub(super) fn newest_of(
    db: &Connection,
    project: &ProjectId,
    agent_id: &str,
) -> Result<Option<Record>, EngineError> {
    db.query_row(
        &format!(
            "SELECT {COLUMNS} FROM landing_change_sets WHERE project=?1 AND agent_kind='sub' \
             AND agent_id=?2 ORDER BY updated_ms DESC LIMIT 1"
        ),
        params![project.as_str(), agent_id],
        read,
    )
    .optional()
    .map_err(storage)
}

/// Reads one project's change sets, open ones first, then newest first.
/// Inputs: the store, the project and the most to read. Output: the records.
pub(super) fn of_project(
    db: &Connection,
    project: &ProjectId,
    limit: usize,
) -> Result<Vec<Record>, EngineError> {
    let mut statement = db
        .prepare(&format!(
            "SELECT {COLUMNS} FROM landing_change_sets WHERE project=?1 \
             ORDER BY state IN ('pending','conflict','updating') DESC, updated_ms DESC, id \
             LIMIT ?2"
        ))
        .map_err(storage)?;
    let limit = i64::try_from(limit).map_err(storage)?;
    statement
        .query_map(params![project.as_str(), limit], read)
        .map_err(storage)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage)
}

/// Counts the change sets ready to review or conflicting, in projects the engine still has,
/// and among them the given project's, in one read. Inputs: the store and the project, if
/// any. Output: both counts; `here` is None without a project.
pub(super) fn waiting(
    db: &Connection,
    project: Option<&ProjectId>,
) -> Result<super::WaitingChanges, EngineError> {
    let (all, here): (i64, i64) = db
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(project = ?1), 0) FROM landing_change_sets \
             WHERE state IN ('pending','conflict') AND project IN (SELECT id FROM projects)",
            [project.map(ProjectId::as_str)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(storage)?;
    let all = u32::try_from(all).map_err(storage)?;
    let here = u32::try_from(here).map_err(storage)?;
    Ok(super::WaitingChanges {
        all,
        here: project.map(|_| here),
    })
}

/// Reads every change set with owner-checkout or worktree work in flight.
/// Input: the store. Output: the records, oldest first.
pub(super) fn in_flight(db: &Connection) -> Result<Vec<Record>, EngineError> {
    collect(db, "intent IS NOT NULL", rusqlite::params![])
}

/// Reads the open change sets, pending, conflicting or updating, of one project or of
/// every project. Inputs: the store and the project, None for every project. Output: the
/// records, oldest first.
pub(super) fn open_of(
    db: &Connection,
    project: Option<&ProjectId>,
) -> Result<Vec<Record>, EngineError> {
    let open = "state IN ('pending','conflict','updating')";
    match project {
        Some(project) => collect(db, &format!("{open} AND project=?1"), [project.as_str()]),
        None => collect(db, open, rusqlite::params![]),
    }
}

/// Reads the other change sets of a project accepted with no commit of their own: a
/// master's merge closes the sets its head holds that way, and an Accept of
/// work the branch already held records whatever commit was the tip. Inputs: the store, the
/// project and the set being undone. Output: the sets.
pub(super) fn closed_without_commit(
    db: &Connection,
    project: &ProjectId,
    maker: &ChangeSetId,
) -> Result<Vec<Record>, EngineError> {
    let closed = collect(
        db,
        "project=?1 AND id<>?2 AND state='accepted' AND made=0",
        [project.as_str(), maker.as_str()],
    )?;
    assert!(closed.iter().all(|record| record.id != *maker));
    Ok(closed)
}

/// Reads the discarded change sets whose restore window ended by `now` and whose ref
/// is not deleted yet. Inputs: the store and the time now. Output: the records.
pub(super) fn discarded_due(db: &Connection, now: u64) -> Result<Vec<Record>, EngineError> {
    collect(
        db,
        "state='discarded' AND pruned=0 AND restorable_until_ms<=?1",
        [signed(now)?],
    )
}

fn collect<P: rusqlite::Params>(
    db: &Connection,
    condition: &str,
    values: P,
) -> Result<Vec<Record>, EngineError> {
    let mut statement = db
        .prepare(&format!(
            "SELECT {COLUMNS} FROM landing_change_sets WHERE {condition} ORDER BY created_ms, id"
        ))
        .map_err(storage)?;
    statement
        .query_map(values, read)
        .map_err(storage)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage)
}
