use super::{
    AddMode, AddOverride, AddPlan, GitLink, LINKED_WORKTREES_MAX, Placed, Registered, Tally,
    git_link, in_use, linked_worktree_paths, linked_worktrees, plan_add, refused,
};
use crate::engine::EngineError;
use crate::landing::RepositoryHolds;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a project move waits for the project's agents to stop after Stop all, before
/// it refuses with nothing moved.
pub const MOVE_STOP_WAIT: Duration = super::DELETE_STOP_WAIT;

const MOVE_POLL: Duration = Duration::from_millis(100);
const LANDING_REPOSITORIES_MAX: usize = 16;
const COPIES_MAX: i64 = 4096;

/// Every stored column that holds an absolute path inside a project's folder and must
/// follow the folder when it moves: the onboarding records keyed by the folder, each agent
/// copy's repository, worktree and git folder, the landing records of those copies, the
/// folders the owner listed for the sandbox, and where a recovery run worked.
pub(super) const PATH_COLUMNS: [(&str, &str); 20] = [
    ("onboarding", "repo"),
    ("onboarding_saved_drafts", "repo"),
    ("onboarding_saved_overviews", "repo"),
    ("team_worktrees", "repository"),
    ("team_worktrees", "worktree"),
    ("team_worktrees", "git_dir"),
    ("team_agents", "worktree"),
    ("team_retiring", "repository"),
    ("team_retiring", "worktree"),
    ("team_retiring", "git_dir"),
    ("landing_change_sets", "repository"),
    ("landing_change_sets", "worktree"),
    ("landing_change_sets", "git_dir"),
    ("landing_bases", "worktree"),
    ("landing_agent_branches", "worktree"),
    ("landing_agent_branches", "repository"),
    ("landing_owner_edits", "worktree"),
    ("recovery_runs", "cwd"),
    ("project_read_paths", "path"),
    ("project_hidden_paths", "path"),
];

/// The tables that record an agent copy with its repository, worktree and git folder.
const COPY_TABLES: [&str; 3] = ["team_worktrees", "team_retiring", "landing_change_sets"];

fn sql(error: rusqlite::Error) -> EngineError {
    EngineError(error.to_string())
}

fn present(connection: &Connection, table: &str) -> rusqlite::Result<bool> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// The SQL test that a path column names `?1` or a path inside it, compared by whole
/// path parts. Input: the column. Output: the condition text.
fn under(column: &str) -> String {
    format!("({column}=?1 OR substr({column},1,length(?1)+1)=?1||'/')")
}

/// Says where a path inside the old project folder is after the move. Inputs: the path and
/// both folders. Output: the same place inside the new folder, or None for a path outside
/// the old one.
fn moved(path: &Path, origin: &Path, target: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(origin).ok()?;
    Some(if rest.as_os_str().is_empty() {
        target.to_path_buf()
    } else {
        target.join(rest)
    })
}

fn text(path: &Path) -> Result<&str, EngineError> {
    path.to_str()
        .ok_or_else(|| refused("projects_home_path_invalid"))
}

/// One owner request to move a registered project into the projects home: the project,
/// Move or Copy, the soft refusals the owner chose to go past, and the request and
/// transport exchange that run it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Asked<'a> {
    pub project: &'a ProjectId,
    pub mode: AddMode,
    pub overrides: &'a [AddOverride],
    pub request: &'a str,
    pub exchange: i64,
}

/// A checked move of one registered project into the projects home, recorded as pending
/// before any disk work: the engine database, the project and the folder plan. Built only
/// by [`plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectMove {
    database: PathBuf,
    project: ProjectId,
    add: AddPlan,
}

/// What a project move's disk work leaves for its caller: what to clear once the move is
/// recorded, and the landing holds to keep until then.
#[derive(Debug)]
pub struct Moved {
    placed: Placed,
    landing: RepositoryHolds,
}

impl Moved {
    /// Splits the outcome. Output: what is left to clear after the record, and the landing
    /// holds, which the caller drops only once the move is recorded or given up.
    pub(crate) fn into_parts(self) -> (Placed, RepositoryHolds) {
        (self.placed, self.landing)
    }
}

impl ProjectMove {
    /// The project being moved.
    pub fn project(&self) -> &ProjectId {
        &self.project
    }

    /// The real folder the project is in now.
    pub fn source(&self) -> &Path {
        self.add.source()
    }

    /// Where the folder lands in the projects home.
    pub fn destination(&self) -> &Path {
        self.add.destination()
    }

    /// Waits for the project's agents to stop, then puts the folder in the home as an add
    /// does, deleting nothing. Output: what is left to clear once the move is recorded (the
    /// whole old folder of a move that went by copy), or the refusal code.
    pub fn run(self) -> Result<Moved, EngineError> {
        self.run_with(MOVE_STOP_WAIT)
    }

    /// Runs the plan with a bounded wait: nothing moves while an agent of the project
    /// runs (`move_project_busy` past the wait), then the folder is placed by
    /// [`AddPlan::place`], which leaves the source whole. Input: the wait. Output: the
    /// outcome.
    pub(crate) fn run_with(self, wait: Duration) -> Result<Moved, EngineError> {
        let landing = self.ready(wait)?;
        let placed = self.add.place(&Tally::default())?;
        Ok(Moved { placed, landing })
    }

    /// Runs the plan as a move across disks or out of iCloud Drive does, by a checked
    /// copy, on any disk. Input: the wait. Output: the old folder to clear, or the
    /// refusal code.
    #[cfg(all(test, unix))]
    pub(crate) fn run_by_copy(self, wait: Duration) -> Result<Moved, EngineError> {
        let landing = self.ready(wait)?;
        let placed = self.add.place_by_copy(&Tally::default())?;
        Ok(Moved { placed, landing })
    }

    /// Waits until no agent of the project runs and no landing work runs in its
    /// repositories, takes the landing holds, then asks again, under the move's hold, what
    /// [`busy`] asked at the plan: work that started since (a master session's working
    /// copy, a change set step, an onboarding step, research) refuses the move before
    /// anything moves. Landing work that began before the plan has ended by then, and none
    /// starts until the caller drops the holds, after the record. Input: the wait. Output:
    /// the landing holds when the folder may move now, `move_project_busy` past the wait,
    /// or the code of the work found.
    fn ready(&self, wait: Duration) -> Result<RepositoryHolds, EngineError> {
        let unavailable = |_| refused("move_project_unavailable");
        let db =
            Connection::open_with_flags(&self.database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(unavailable)?;
        db.busy_timeout(Duration::from_secs(5))
            .map_err(unavailable)?;
        let deadline = Instant::now() + wait;
        while active_agents(&db, &self.project).map_err(unavailable)? > 0 {
            if Instant::now() >= deadline {
                return Err(refused("move_project_busy"));
            }
            std::thread::sleep(MOVE_POLL);
        }
        let origin = self
            .add
            .source()
            .to_str()
            .ok_or_else(|| refused("move_project_unavailable"))?;
        assert!(self.add.source().is_absolute());
        let landing = crate::landing::hold_repositories(
            &landing_repositories(&db, &self.project, self.add.source())?,
            deadline.saturating_duration_since(Instant::now()),
        )
        .ok_or_else(|| refused("move_project_busy"))?;
        match busy(&db, &self.project, origin)? {
            Some(code) => Err(refused(code)),
            None => Ok(landing),
        }
    }
}

fn active_agents(connection: &Connection, project: &ProjectId) -> rusqlite::Result<i64> {
    if !present(connection, "team_agents")? {
        return Ok(0);
    }
    connection.query_row(
        "SELECT COUNT(*) FROM team_agents WHERE project=?1 AND status NOT IN ('stopped','failed')",
        [project.as_str()],
        |row| row.get(0),
    )
}

/// A project move recorded before its folder moves: its request, the transport exchange
/// running it, the project, the folder it is in, the folder it gets in the home and
/// whether it is moved or copied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pending {
    pub request: String,
    pub exchange: i64,
    pub project: ProjectId,
    pub origin: PathBuf,
    pub target: PathBuf,
    pub mode: AddMode,
}

/// Reads the pending project move. Input: an engine connection. Output: the move, or None
/// when none is recorded or the engine has not made the table.
pub(crate) fn pending(connection: &Connection) -> Result<Option<Pending>, EngineError> {
    if !present(connection, "projects_home_project_move").map_err(sql)? {
        return Ok(None);
    }
    let row: Option<(String, i64, String, String, String, String)> = connection
        .query_row(
            "SELECT request, exchange, project, origin, target, mode
             FROM projects_home_project_move WHERE singleton=1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(sql)?;
    row.map(|(request, exchange, project, origin, target, mode)| {
        Ok(Pending {
            request,
            exchange,
            project: ProjectId::new(project).map_err(|e| EngineError(e.to_string()))?,
            origin: PathBuf::from(origin),
            target: PathBuf::from(target),
            mode: match mode.as_str() {
                "move" => AddMode::Move,
                "copy" => AddMode::Copy,
                _ => return Err(refused("move_project_record_invalid")),
            },
        })
    })
    .transpose()
}

/// The owner repositories landing work of the project runs in: the project's folder, and
/// each repository one of its change sets names that is that folder or inside it, under
/// the spelling the change set stores, since landing work takes its hold by that. Inputs:
/// an engine connection, the project and its real folder. Output: the roots, or
/// `move_project_unavailable` (also for more than [`LANDING_REPOSITORIES_MAX`]).
fn landing_repositories(
    db: &Connection,
    project: &ProjectId,
    origin: &Path,
) -> Result<BTreeSet<PathBuf>, EngineError> {
    let mut roots = BTreeSet::from([origin.to_path_buf()]);
    if !present(db, "landing_change_sets").map_err(sql)? {
        return Ok(roots);
    }
    let mut statement = db
        .prepare("SELECT DISTINCT repository FROM landing_change_sets WHERE project=?1 LIMIT ?2")
        .map_err(sql)?;
    let stored = statement
        .query_map(
            params![project.as_str(), LANDING_REPOSITORIES_MAX as i64 + 1],
            |row| row.get::<_, String>(0),
        )
        .map_err(sql)?;
    for repository in stored {
        let repository = PathBuf::from(repository.map_err(sql)?);
        let inside = repository.starts_with(origin)
            || std::fs::canonicalize(&repository).is_ok_and(|real| real.starts_with(origin));
        if inside {
            roots.insert(repository);
        }
    }
    if roots.len() > LANDING_REPOSITORIES_MAX {
        return Err(refused("move_project_unavailable"));
    }
    Ok(roots)
}

/// Tells whether a pending project move holds `folder`: the folder is the project's old
/// folder or inside it, as given or as it resolves now. No agent or research run starts
/// there until the move is finished or forgotten. Inputs: an engine connection and the
/// folder. Output: the answer, or any SQLite failure.
pub(crate) fn holds(connection: &Connection, folder: &Path) -> rusqlite::Result<bool> {
    if !present(connection, "projects_home_project_move")? {
        return Ok(false);
    }
    let origin: Option<String> = connection
        .query_row(
            "SELECT origin FROM projects_home_project_move WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(origin.is_some_and(|origin| {
        let origin = Path::new(&origin);
        folder.starts_with(origin)
            || std::fs::canonicalize(folder).is_ok_and(|real| real.starts_with(origin))
    }))
}

/// Forgets a pending project move whose disk work never ran. Inputs: the transaction and
/// the transport exchange that planned it. Output: Ok; a move of another exchange stays.
pub(crate) fn forget(tx: &Transaction<'_>, exchange: i64) -> Result<(), EngineError> {
    tx.execute(
        "DELETE FROM projects_home_project_move WHERE singleton=1 AND exchange=?1",
        [exchange],
    )
    .map_err(sql)?;
    Ok(())
}

fn count(
    tx: &Connection,
    table: &str,
    query: &str,
    parameters: impl rusqlite::Params,
) -> Result<i64, EngineError> {
    if !present(tx, table).map_err(sql)? {
        return Ok(0);
    }
    let found: i64 = tx
        .query_row(query, parameters, |row| row.get(0))
        .map_err(sql)?;
    assert!(found >= 0);
    Ok(found)
}

/// Names what keeps a project from moving now, as its refusal code: `move_project_busy`
/// for an agent that runs while the team is not stopped, research that runs (research
/// still waiting starts only after the move, which holds the folder), an onboarding step
/// that runs or a change set step in flight;
/// `move_project_master_session` for a master session that holds its own working copy of
/// the repository, which lives outside the folder and would lose it. It is asked when the
/// move is planned and again right before the folder moves. Inputs: an engine connection,
/// the project and its folder. Output: the code, or None.
fn busy(
    tx: &Connection,
    project: &ProjectId,
    origin: &str,
) -> Result<Option<&'static str>, EngineError> {
    let id = project.as_str();
    let agents = active_agents(tx, project).map_err(sql)?;
    if agents > 0 && !crate::team::paused(tx, project)? {
        return Ok(Some("move_project_busy"));
    }
    let working = count(
        tx,
        "research_jobs",
        "SELECT COUNT(*) FROM research_jobs
         WHERE project=?1 AND status='running'",
        [id],
    )? + count(
        tx,
        "onboarding",
        "SELECT COUNT(*) FROM onboarding
         WHERE project=?1 AND (graph_state='running' OR mapping_state='running')",
        [id],
    )? + count(
        tx,
        "landing_change_sets",
        "SELECT COUNT(*) FROM landing_change_sets
         WHERE project=?1 AND (state='updating' OR intent IS NOT NULL)",
        [id],
    )?;
    if working > 0 {
        return Ok(Some("move_project_busy"));
    }
    let leased = count(
        tx,
        "worktree_leases",
        &format!(
            "SELECT COUNT(*) FROM worktree_leases WHERE {}
             AND json_extract(body,'$.lease.state') IN ('Reserved','Ready')",
            under("repository")
        ),
        [origin],
    )?;
    Ok((leased > 0).then_some("move_project_master_session"))
}

/// Tells whether an agent of the project works in a linked git worktree of its
/// repository, which a Copy leaves with the original. Inputs: the transaction and the
/// project folder. Output: the answer.
fn linked_agent_copies(tx: &Transaction<'_>, origin: &Path) -> Result<bool, EngineError> {
    let admin = origin.join(".git").join("worktrees");
    let admin = text(&admin)?;
    for table in ["team_worktrees", "team_retiring"] {
        let query = format!("SELECT COUNT(*) FROM {table} WHERE {}", under("git_dir"));
        if count(tx, table, &query, [admin])? > 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Plans moving a registered project kept in place into the projects home and records the
/// move as pending in the caller's write, before any disk work. The pending record is also
/// the hold: no agent or research run starts in the folder until the move is finished or
/// forgotten ([`holds`]). Refusals, with nothing written: another project move pending
/// (`move_project_in_progress`), no projects home, a project without setup, a folder
/// already in the home (`move_project_already_home`), a folder reached through a link
/// (`move_project_folder_link`), another project using the folder (`home_move_project`),
/// what [`busy`] names, a Copy while an agent works in a linked worktree
/// (`move_project_copy_worktrees`), a Move that breaks git worktrees kept outside the
/// folder unless the owner went past it (`home_move_worktrees`), and every refusal of
/// [`plan_add`]. Inputs: the transaction, the engine database path, the engine data
/// folder, the guarded projects and the request. Output: the plan.
pub(crate) fn plan(
    tx: &Transaction<'_>,
    database: &Path,
    engine_root: &Path,
    registered: &[Registered],
    asked: &Asked<'_>,
) -> Result<ProjectMove, EngineError> {
    assert!(asked.exchange > 0);
    if pending(tx)?.is_some() {
        return Err(refused("move_project_in_progress"));
    }
    let home = super::load(tx)?.ok_or_else(|| refused("projects_home_unset"))?;
    let found = registered
        .iter()
        .find(|known| &known.project_id == asked.project)
        .ok_or_else(|| refused("invalid_project"))?;
    let set_up = count(
        tx,
        "project_setups",
        "SELECT COUNT(*) FROM project_setups WHERE project=?1",
        [asked.project.as_str()],
    )?;
    if set_up == 0 {
        return Err(refused("project_setup_missing"));
    }
    let real = std::fs::canonicalize(&found.folder).map_err(|_| refused("home_source_missing"))?;
    if real.starts_with(&home) {
        return Err(refused("move_project_already_home"));
    }
    if real != found.folder {
        return Err(refused("move_project_folder_link"));
    }
    let source = text(&real)?;
    let others: Vec<Registered> = registered
        .iter()
        .filter(|known| &known.project_id != asked.project)
        .cloned()
        .collect();
    if others.iter().any(|other| in_use(&real, other)) {
        return Err(refused("home_move_project"));
    }
    if let Some(code) = busy(tx, asked.project, source)? {
        return Err(refused(code));
    }
    let mut overrides = asked.overrides.to_vec();
    match asked.mode {
        AddMode::Copy => {
            if linked_agent_copies(tx, &real)? {
                return Err(refused("move_project_copy_worktrees"));
            }
        }
        AddMode::Move if !overrides.contains(&AddOverride::Worktrees) => {
            let outside = match git_link(&real) {
                GitLink::None => false,
                GitLink::Worktrees => {
                    let paths = linked_worktree_paths(&real, LINKED_WORKTREES_MAX);
                    paths.len() < linked_worktrees(&real)
                        || paths.iter().any(|path| !path.starts_with(&real))
                }
                _ => true,
            };
            if outside {
                return Err(refused("home_move_worktrees"));
            }
            overrides.push(AddOverride::Worktrees);
        }
        AddMode::Move => {}
    }
    let add = plan_add(&home, engine_root, source, asked.mode, &overrides, &others)?;
    assert_eq!(add.source(), real);
    assert!(add.destination().starts_with(&home));
    tx.execute(
        "INSERT INTO projects_home_project_move VALUES (1,?1,?2,?3,?4,?5,?6)",
        params![
            asked.request,
            asked.exchange,
            asked.project.as_str(),
            source,
            text(add.destination())?,
            match asked.mode {
                AddMode::Move => "move",
                AddMode::Copy => "copy",
            }
        ],
    )
    .map_err(sql)?;
    Ok(ProjectMove {
        database: database.to_path_buf(),
        project: asked.project.clone(),
        add,
    })
}

/// What finishing a pending project move found on disk: the project now lives in the home
/// with this many agent copies re-pointed, or its folder never got there and the project
/// stays where it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Finished {
    Moved { repointed: usize },
    Left,
}

/// Finishes the pending project move from what is on disk now and clears it. A Move whose
/// disk work reported success (`verified`) counts once the folder is a real folder in the
/// home: a move that went by copy still has its whole old folder, which is cleared only
/// after this record. Without that report a Move counts only when the old folder is gone
/// (a rename), since a copy cut short looks the same on disk; a Copy counts only when
/// verified. A finished move re-points the project's agent copies and rewrites every
/// stored path of the folder in the caller's write. Inputs: the transaction, which pending
/// move to finish and whether its disk work reported success. Output: None when no such
/// move is pending, else what was found.
pub(crate) fn finish(
    tx: &Transaction<'_>,
    belongs: impl Fn(&Pending) -> bool,
    verified: bool,
) -> Result<Option<Finished>, EngineError> {
    let Some(pending) = pending(tx)? else {
        return Ok(None);
    };
    if !belongs(&pending) {
        return Ok(None);
    }
    tx.execute(
        "DELETE FROM projects_home_project_move WHERE singleton=1",
        [],
    )
    .map_err(sql)?;
    let landed = std::fs::symlink_metadata(&pending.target)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink());
    let left = std::fs::symlink_metadata(&pending.origin).is_ok();
    let done = match pending.mode {
        AddMode::Move => landed && (verified || !left),
        AddMode::Copy => verified && landed && left,
    };
    if !done {
        return Ok(Some(Finished::Left));
    }
    let repointed = repoint(tx, &pending.origin, &pending.target)?;
    rewrite(tx, &pending.project, &pending.origin, &pending.target)?;
    Ok(Some(Finished::Moved { repointed }))
}

/// Re-points the agent copies the store records inside the old folder, now at the same
/// places inside the new one: an agent's own clone gets the new project object store as
/// its one alternate, and a linked worktree gets both of its git pointer files rewritten.
/// A copy whose files are not exactly as the engine wrote them is left alone and logged;
/// git then refuses it instead of reading another store. Inputs: the transaction and both
/// folders. Output: how many copies were rewritten.
fn repoint(tx: &Transaction<'_>, origin: &Path, target: &Path) -> Result<usize, EngineError> {
    let from = text(origin)?;
    let mut copies: Vec<(String, String, String)> = Vec::new();
    for table in COPY_TABLES {
        if !present(tx, table).map_err(sql)? {
            continue;
        }
        let query = format!(
            "SELECT DISTINCT repository, worktree, git_dir FROM {table} WHERE {} LIMIT ?2",
            under("git_dir")
        );
        let mut statement = tx.prepare(&query).map_err(sql)?;
        let rows = statement
            .query_map(params![from, COPIES_MAX], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(sql)?;
        for row in rows {
            let copy = row.map_err(sql)?;
            if !copies.contains(&copy) {
                copies.push(copy);
            }
        }
    }
    let mut repointed = 0;
    for (repository, worktree, git_dir) in &copies {
        let (repository, worktree, git_dir) = (
            Path::new(repository),
            Path::new(worktree),
            Path::new(git_dir),
        );
        let (Some(new_repository), Some(new_worktree), Some(new_git)) = (
            moved(repository, origin, target),
            moved(worktree, origin, target),
            moved(git_dir, origin, target),
        ) else {
            continue;
        };
        let own = std::fs::symlink_metadata(new_git.join("objects").join("info")).is_ok();
        let done = if own {
            crate::landing::repoint_agent_clone(&new_git, repository, &new_repository)
        } else {
            crate::landing::repoint_linked_worktree(&new_git, &new_worktree, git_dir, worktree)
        };
        match done {
            Ok(true) => repointed += 1,
            Ok(false) => {}
            Err(e) => eprintln!(
                "move_project_copy_not_repointed copy={} cause={}",
                new_worktree
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
                e.0
            ),
        }
    }
    assert!(repointed <= copies.len());
    Ok(repointed)
}

/// Rewrites every stored path of the project's old folder to the same place in the new
/// one, by whole path parts: the setup's folder, the repositories the project declares
/// and each column of [`PATH_COLUMNS`]. A stale row already at the new path gives way to
/// the moved one. Records of what was said or done (prompts, messages, events) and of the
/// master's identity-pinned working copies keep the old path. Inputs: the transaction,
/// the project and both folders. Output: Ok, or the bound a moved path breaks.
fn rewrite(
    tx: &Transaction<'_>,
    project: &ProjectId,
    origin: &Path,
    target: &Path,
) -> Result<(), EngineError> {
    let (from, to) = (text(origin)?, text(target)?);
    let json = |e: serde_json::Error| EngineError(e.to_string());
    let body: Option<String> = tx
        .query_row(
            "SELECT body FROM project_setups WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)?;
    if let Some(body) = body {
        let mut stored: crate::project_setup::StoredSetup =
            serde_json::from_str(&body).map_err(json)?;
        if let Some(place) = moved(Path::new(&stored.folder), origin, target) {
            stored.folder = text(&place)?.to_owned();
            assert!(Path::new(&stored.folder).starts_with(target));
            tx.execute(
                "UPDATE project_setups SET body=?1 WHERE project=?2",
                params![
                    serde_json::to_string(&stored).map_err(json)?,
                    project.as_str()
                ],
            )
            .map_err(sql)?;
        }
    }
    let body: Option<String> = tx
        .query_row(
            "SELECT body FROM projects WHERE id=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)?;
    if let Some(body) = body {
        let mut stored: crate::protocol::StoredProject =
            serde_json::from_str(&body).map_err(json)?;
        let mut changed = 0;
        for repository in &mut stored.spec.repositories {
            if let Some(place) = moved(Path::new(repository.as_str()), origin, target) {
                *repository = text(&place)?.to_owned();
                changed += 1;
            }
        }
        if changed > 0 {
            stored
                .spec
                .validate()
                .map_err(|e| EngineError(e.to_string()))?;
            tx.execute(
                "UPDATE projects SET body=?1 WHERE id=?2",
                params![
                    serde_json::to_string(&stored).map_err(json)?,
                    project.as_str()
                ],
            )
            .map_err(sql)?;
        }
    }
    for (table, column) in PATH_COLUMNS {
        if !present(tx, table).map_err(sql)? {
            continue;
        }
        tx.execute(
            &format!(
                "UPDATE OR REPLACE {table} SET {column}=?2||substr({column},length(?1)+1)
                 WHERE {}",
                under(column)
            ),
            params![from, to],
        )
        .map_err(sql)?;
    }
    Ok(())
}
