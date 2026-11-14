use super::*;
use crate::projects_home::{
    self as home, AddMode, AddOverride, AddPlan, FoundProject, HomeAction, HomeEffect, HomeWatch,
    MovePlan, ProjectsHome, Registered,
};
use std::ffi::OsStr;
use std::path::{Component, PathBuf};

const FOUND_REPLY_BYTES: usize = 48 * 1024;

/// The storage actor's watch of the saved projects home. The OS watch starts, and the
/// one it replaces stops, on a thread of their own, so no owner request ever waits on
/// the folder watch setup; a read takes a finished start without waiting for one.
#[derive(Default)]
pub(super) struct WatchSlot {
    live: Option<HomeWatch>,
    starting: Option<(PathBuf, std::sync::mpsc::Receiver<Option<HomeWatch>>)>,
}

impl WatchSlot {
    /// Replaces the watch with one of the home saved now, started on its own thread.
    /// Input: the engine connection. Output: none; with no saved home, or when the
    /// thread cannot start, there is no watch and reads scan the folder.
    pub(super) fn restart(&mut self, connection: &Connection) {
        let stale = (self.live.take(), self.starting.take());
        let saved = home::load(connection).ok().flatten();
        if saved.is_none() && stale.0.is_none() && stale.1.is_none() {
            return;
        }
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let home = saved.clone();
        let spawned = std::thread::Builder::new()
            .name("projects-home-watch".into())
            .spawn(move || {
                drop(stale);
                if let Some(saved) = saved {
                    let _ = send.send(start_watch(&saved));
                }
            });
        match (spawned, home) {
            (Ok(_), Some(home)) => self.starting = Some((home, receive)),
            (Ok(_), None) => {}
            (Err(_), _) => {
                eprintln!("projects_home_watch_not_started cause=projects_home_watch_thread");
            }
        }
        assert!(self.live.is_none());
    }

    /// The watch of the saved home as a read sees it, after taking a start that has
    /// finished. Input: the home saved now. Output: its live watch, and whether its
    /// watch is still starting; a watch of another home counts as neither.
    fn observe(&mut self, saved: Option<&Path>) -> (Option<&HomeWatch>, bool) {
        if let Some((_, receive)) = &self.starting {
            match receive.try_recv() {
                Ok(watch) => {
                    self.live = watch;
                    self.starting = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.starting = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        assert!(self.live.is_none() || self.starting.is_none());
        let starting = self
            .starting
            .as_ref()
            .is_some_and(|(home, _)| saved == Some(home.as_path()));
        let live = self
            .live
            .as_ref()
            .filter(|watch| saved == Some(watch.home()) && watch.watching());
        (live, starting)
    }
}

/// Starts the OS watch of a saved projects home. Input: the home folder. Output: the
/// running watch, or None when the OS refuses it; reads then scan the folder.
fn start_watch(saved: &Path) -> Option<HomeWatch> {
    match HomeWatch::start(saved) {
        Ok(watch) => Some(watch),
        Err(e) => {
            eprintln!("projects_home_watch_not_started cause={}", e.0);
            None
        }
    }
}

fn engine_root(path: &Path) -> Result<PathBuf, EngineError> {
    let parent = path
        .parent()
        .ok_or_else(|| error("runtime_parent_missing"))?;
    std::fs::canonicalize(parent).map_err(error)
}

/// Lists every set-up project, none left out, so the trash guard and the
/// Found list see them all: each project's folder, the repositories it
/// declares and whether onboarding is still under way. An onboarded project
/// is Ready once the owner saved its setup, or its graph step is done or off
/// and its mapping is done; a project made with New Project has no onboarding
/// row and is Ready at once.
fn registered(tx: &Transaction<'_>) -> Result<Vec<Registered>, EngineError> {
    let done = crate::onboarding::Step::Done.columns().0;
    let off = crate::onboarding::Step::Off.columns().0;
    let mut statement = tx
        .prepare(
            "SELECT s.body, o.graph_state, o.mapping_state, p.body FROM project_setups s
             LEFT JOIN onboarding o ON o.project=s.project
             LEFT JOIN projects p ON p.id=s.project
             ORDER BY s.project",
        )
        .map_err(error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(error)?;
    let mut projects = Vec::new();
    for row in rows {
        let (body, graph, mapping, project) = row.map_err(error)?;
        let stored: crate::project_setup::StoredSetup =
            serde_json::from_str(&body).map_err(error)?;
        let repositories = match project {
            Some(project) => declared(&project)?.1,
            None => Vec::new(),
        };
        let onboarding = !stored.saved()
            && graph
                .zip(mapping)
                .is_some_and(|(graph, mapping)| (graph != done && graph != off) || mapping != done);
        projects.push(Registered {
            folder: stored.project_folder(),
            project_id: stored.project_id,
            name: stored.name,
            repositories,
            onboarding,
        });
    }
    Ok(projects)
}

/// Reads a project row's body. Input: the stored body. Output: the project
/// and the repositories it declares as paths, or the parse failure.
fn declared(body: &str) -> Result<(crate::protocol::StoredProject, Vec<PathBuf>), EngineError> {
    let stored: crate::protocol::StoredProject = serde_json::from_str(body).map_err(error)?;
    let repositories = stored.spec.repositories.iter().map(PathBuf::from).collect();
    Ok((stored, repositories))
}

/// The projects whose folders the trash and move guards keep: every set-up
/// project, then each project made without setup that declares a repository,
/// its first repository standing in for its folder. Input: the transaction.
/// Output: the projects with the repositories each declares.
fn guarded(tx: &Transaction<'_>) -> Result<Vec<Registered>, EngineError> {
    let mut projects = registered(tx)?;
    let mut statement = tx
        .prepare(
            "SELECT p.body FROM projects p
             WHERE NOT EXISTS (SELECT 1 FROM project_setups s WHERE s.project=p.id)
             ORDER BY p.id",
        )
        .map_err(error)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(error)?;
    for row in rows {
        let (stored, repositories) = declared(&row.map_err(error)?)?;
        let Some(folder) = repositories.first().cloned() else {
            continue;
        };
        projects.push(Registered {
            project_id: stored.project_id,
            name: stored.spec.name,
            folder,
            repositories,
            onboarding: false,
        });
    }
    Ok(projects)
}

/// Answers the ProjectsHome and FoundProjects owner reads.
/// Input: the connection, config, owner admission, query and live watch.
/// Output: the saved home with its watch state, or the found folders and
/// missing projects in name order, cut to fit one owner reply.
pub(super) fn read(
    db: &mut Connection,
    config: &EffectiveConfig,
    a: &Admission,
    query: &OwnerRead,
    watch: &mut WatchSlot,
) -> Result<OwnerReadReply, EngineError> {
    let tx = db.transaction().map_err(error)?;
    check_owner_admission(&tx, a)?;
    let revision = owner_revision(&tx)?;
    let saved = home::load(&tx)?;
    let (live, starting) = watch.observe(saved.as_deref());
    let data = match query {
        OwnerRead::ProjectsHome => OwnerData::ProjectsHome {
            home: ProjectsHome {
                path: saved.map(|path| path.to_string_lossy().into_owned()),
                watching: live.is_some() || starting,
            },
        },
        OwnerRead::FoundProjects => {
            let folders = match (live, &saved) {
                (Some(watch), _) => {
                    watch.refresh();
                    watch.folders().1
                }
                (None, Some(path)) => home::scan(path).unwrap_or_default(),
                (None, None) => Vec::new(),
            };
            let mut projects: Vec<FoundProject> = Vec::new();
            let mut bytes = 0usize;
            for project in home::found(saved.as_deref(), &folders, &registered(&tx)?) {
                bytes += serde_json::to_vec(&project).map_err(error)?.len() + 1;
                if bytes > FOUND_REPLY_BYTES {
                    break;
                }
                projects.push(project);
            }
            OwnerData::FoundProjects { projects }
        }
        _ => return Err(error("owner_read_required")),
    };
    Ok(OwnerReadReply {
        revision: Cursor::new(revision),
        config_id: config.id.clone(),
        data,
    })
}

/// Plans an AddExistingFolder before the owner transport moves or copies it.
/// Input: the connection, database path, owner admission, the revision the
/// owner saw, source and mode. Output: the checked plan, or the refusal code
/// (`stale_revision` when the engine moved on); nothing on disk changes.
#[allow(clippy::too_many_arguments)]
pub(super) fn add_plan(
    db: &mut Connection,
    path: &Path,
    a: &Admission,
    expected: &Cursor,
    request: &RequestId,
    source: &str,
    mode: AddMode,
    overrides: &[AddOverride],
) -> Result<AddPlan, EngineError> {
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    check_owner_admission(&tx, a)?;
    if owner_revision(&tx)? != expected.value() {
        return Err(error("stale_revision"));
    }
    if pending_move(&tx)?.is_some() {
        return Err(error("home_move_in_progress"));
    }
    let saved = home::load(&tx)?.ok_or_else(|| error("projects_home_unset"))?;
    let plan = home::plan_add(
        &saved,
        &engine_root(path)?,
        source,
        mode,
        overrides,
        &guarded(&tx)?,
    )?;
    if another_add_running(&tx, request)? {
        return Err(error("home_add_in_progress"));
    }
    let mode = match mode {
        AddMode::Move => "move",
        AddMode::Copy => "copy",
    };
    tx.execute(
        "INSERT OR REPLACE INTO projects_home_add(request, owner, source, destination, mode)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            request.as_str(),
            a.owner_id,
            text(plan.source())?,
            text(plan.destination())?,
            mode
        ],
    )
    .map_err(error)?;
    assert!(adding(&tx)?);
    assert!(!another_add_running(&tx, request)?);
    tx.commit().map_err(error)?;
    Ok(plan)
}

/// Checks an AddExistingFolder before an intake job downloads its folder: the refusals of
/// [`add_plan`] that read nothing inside the folder ([`home::check_add`], the pending home
/// move, an unset home, another running add), with no revision check and no
/// `projects_home_add` row written. Inputs: the connection, database path, owner admission,
/// the job's request, source, mode and overrides. Output: Ok, or the refusal code.
#[allow(clippy::too_many_arguments)]
pub(super) fn add_check(
    db: &mut Connection,
    path: &Path,
    a: &Admission,
    request: &RequestId,
    source: &str,
    mode: AddMode,
    overrides: &[AddOverride],
) -> Result<(), EngineError> {
    let tx = db.transaction().map_err(error)?;
    check_owner_admission(&tx, a)?;
    if pending_move(&tx)?.is_some() {
        return Err(error("home_move_in_progress"));
    }
    let saved = home::load(&tx)?.ok_or_else(|| error("projects_home_unset"))?;
    let root = engine_root(path)?;
    let plan = home::check_add(&saved, &root, source, mode, overrides, &guarded(&tx)?)?;
    if another_add_running(&tx, request)? {
        return Err(error("home_add_in_progress"));
    }
    assert!(plan.destination().starts_with(&saved));
    assert!(!plan.source().starts_with(&saved));
    tx.rollback().map_err(error)
}

/// Whether an add other than `request` is running. One add runs at a time: two adds find a
/// destination free on disk until one lands, an engine start reads each cut add's end from
/// its destination folder, and names the volume treats as one folder (case, Unicode form)
/// differ as text. The asking add's own row, as a resend finds it, does not count. Inputs:
/// the connection and the asking add's request. Output: the answer.
fn another_add_running(db: &Connection, request: &RequestId) -> Result<bool, EngineError> {
    assert!(!request.as_str().is_empty());
    let running: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projects_home_add WHERE ended IS NULL AND request<>?1)",
            [request.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    assert!(!running || adding(db)?);
    Ok(running)
}

/// Whether an add is running: planned, and neither recorded nor settled yet. While one runs
/// the projects home stays where its plan saw it. Input: the connection. Output: the answer.
fn adding(db: &Connection) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects_home_add WHERE ended IS NULL)",
        [],
        |row| row.get(0),
    )
    .map_err(error)
}

/// The most running adds one engine start reads; planning admits one running add at a time
/// ([`another_add_running`]), and the bound holds for any other rows the table has.
const ADDS_MAX: usize = 64;

/// How an add a record never reached is settled. With a request (after its exchange
/// replied): its running row is forgotten, its disk work already settled in the exchange.
/// With none (engine start, before any add plans, so no staging is live): each running row
/// gets its staging folders removed and the end its disk shows, nothing else deleted, and
/// only the newest ended row is kept for the owner to read. Inputs: the connection and the
/// request. Output: how many running rows were settled.
pub(super) fn add_settle(
    db: &mut Connection,
    request: Option<&RequestId>,
) -> Result<usize, EngineError> {
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let settled = match request {
        Some(request) => tx
            .execute(
                "DELETE FROM projects_home_add WHERE request=?1 AND ended IS NULL",
                [request.as_str()],
            )
            .map_err(error)?,
        None => restart_adds(&tx)?,
    };
    assert!(request.is_some() || !adding(&tx)?);
    tx.commit().map_err(error)?;
    Ok(settled)
}

fn restart_adds(tx: &Transaction<'_>) -> Result<usize, EngineError> {
    let mut statement = tx
        .prepare(
            "SELECT request, source, destination, mode FROM projects_home_add
             WHERE ended IS NULL ORDER BY rowid LIMIT ?1",
        )
        .map_err(error)?;
    let rows: Vec<(String, String, String, String)> = statement
        .query_map([ADDS_MAX as i64], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    drop(statement);
    for (request, source, destination, mode) in &rows {
        let destination = Path::new(destination);
        if let Some(parent) = destination.parent() {
            let removed = home::sweep_staging(parent);
            eprintln!("home_add_restart_staging_removed count={removed}");
        }
        let ended = home::add_left(Path::new(source), destination, mode == "move");
        eprintln!("home_add_restart ended={ended}");
        tx.execute(
            "UPDATE projects_home_add SET ended=?1 WHERE request=?2",
            params![ended, request],
        )
        .map_err(error)?;
    }
    tx.execute(
        "DELETE FROM projects_home_add WHERE ended IS NOT NULL AND rowid <>
         (SELECT max(rowid) FROM projects_home_add WHERE ended IS NOT NULL)",
        [],
    )
    .map_err(error)?;
    assert!(rows.len() <= ADDS_MAX);
    Ok(rows.len())
}

/// An add an engine stop cut short, as the engine start found it: its request, owner,
/// source, mode and end code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddEnded {
    pub request: RequestId,
    pub owner: String,
    pub source: String,
    pub mode: AddMode,
    pub ended: String,
}

/// The newest add an engine stop cut short and the owner has not dismissed. Input: the
/// connection. Output: it, or None.
pub(super) fn add_ended(db: &Connection) -> Result<Option<AddEnded>, EngineError> {
    let row: Option<(String, String, String, String, String)> = db
        .query_row(
            "SELECT request, owner, source, mode, ended FROM projects_home_add
             WHERE ended IS NOT NULL ORDER BY rowid DESC LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(error)?;
    row.map(|(request, owner, source, mode, ended)| {
        let mode = match mode.as_str() {
            "move" => AddMode::Move,
            _ => AddMode::Copy,
        };
        Ok(AddEnded {
            request: RequestId::new(request).map_err(|_| error("home_add_row_invalid"))?,
            owner,
            source,
            mode,
            ended,
        })
    })
    .transpose()
}

/// Forgets an ended add the owner dismissed or a new job replaced. Inputs: the connection
/// and its request. Output: Ok.
pub(super) fn add_forget(db: &Connection, request: &RequestId) -> Result<(), EngineError> {
    db.execute(
        "DELETE FROM projects_home_add WHERE request=?1 AND ended IS NOT NULL",
        [request.as_str()],
    )
    .map_err(error)?;
    Ok(())
}

/// A checked Delete everything: the plan with the project's research jobs, or the refusal code.
pub(crate) type DeletePlanned = Result<(home::DeletePlan, Vec<i64>), String>;

/// Why a Delete everything of a project is refused before anything stops: the refusal code
/// and the sentence the remove dialog shows, which names the other project or the research.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeleteRefusal {
    pub code: String,
    pub reason: String,
}

impl DeleteRefusal {
    /// A refusal shown with the plain sentence of its code. Input: the code. Output: the
    /// refusal.
    fn plain(code: &str) -> Self {
        Self {
            code: code.to_owned(),
            reason: home::plain_problem(code).unwrap_or(code).to_owned(),
        }
    }
}

/// What a Delete everything of a project would take and whether it may go, read in one
/// transaction: the project's name, its folder followed by every repository it declares, each
/// once, and either the canonical projects home with the fewest folders to move or the refusal.
#[derive(Debug)]
pub(crate) struct DeleteChecked {
    pub name: String,
    pub folders: Vec<PathBuf>,
    pub planned: Result<(PathBuf, Vec<PathBuf>), DeleteRefusal>,
}

/// Checks a Delete everything of a project in one transaction, for the remove preview and
/// the engine's plan alike: the platform must qualify (`delete_platform_unqualified`), a
/// projects home must be set, every folder must be a real folder strictly inside it, and
/// nothing [`delete_blocked`] names may hold it back. Inputs: the transaction and the
/// project. Output: None for an unknown project, else what the delete would take and
/// whether it may go.
pub(crate) fn delete_check(
    tx: &Transaction<'_>,
    project: &ProjectId,
) -> Result<Option<DeleteChecked>, EngineError> {
    let projects = guarded(tx)?;
    let Some(found) = projects
        .iter()
        .find(|registered| &registered.project_id == project)
    else {
        return Ok(None);
    };
    let mut folders = vec![found.folder.clone()];
    for repository in &found.repositories {
        if !folders.contains(repository) {
            folders.push(repository.clone());
        }
    }
    let home = home::load(tx)?.and_then(|saved| std::fs::canonicalize(saved).ok());
    let planned = match home {
        _ if !home::delete_offered() => Err(DeleteRefusal::plain("delete_platform_unqualified")),
        None => Err(DeleteRefusal::plain("projects_home_unset")),
        Some(home) => match home::plan_delete(&home, &folders) {
            Err(refusal) => Err(DeleteRefusal::plain(&refusal.0)),
            Ok(moved) => match delete_blocked(tx, project, &projects, &moved)? {
                Some(refusal) => Err(refusal),
                None => Ok((home, moved)),
            },
        },
    };
    Ok(Some(DeleteChecked {
        name: found.name.clone(),
        folders,
        planned,
    }))
}

/// Names what holds a Delete everything back that can change while it waits: a projects
/// home move still pending (`home_move_in_progress`), another registered project using one
/// of the folders, its own folder or a repository it declares, nested either way
/// (`delete_shared_folder`, naming that project), or research of the project queued,
/// running or waiting for a pool (`delete_research_running`). Inputs: the transaction, the
/// project, every guarded project and the folders the delete would move. Output: the
/// refusal, or None.
fn delete_blocked(
    tx: &Transaction<'_>,
    project: &ProjectId,
    projects: &[Registered],
    moved: &[PathBuf],
) -> Result<Option<DeleteRefusal>, EngineError> {
    if pending_move(tx)?.is_some() {
        return Ok(Some(DeleteRefusal::plain("home_move_in_progress")));
    }
    if home::pending(tx)?.is_some() {
        return Ok(Some(DeleteRefusal::plain("move_project_in_progress")));
    }
    let shared = projects.iter().find(|other| {
        &other.project_id != project && moved.iter().any(|folder| home::in_use(folder, other))
    });
    if let Some(other) = shared {
        return Ok(Some(DeleteRefusal {
            code: "delete_shared_folder".into(),
            reason: format!(
                "{} uses a folder of this project, so deleting it would delete that project's files too. Remove it from the dashboard instead.",
                other.name
            ),
        }));
    }
    let research: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM research_jobs
             WHERE project=?1 AND status IN ('queued','running','needs_pool')",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    assert!(research >= 0);
    if research > 0 {
        let jobs = if research == 1 {
            "1 job".to_owned()
        } else {
            format!("{research} jobs")
        };
        return Ok(Some(DeleteRefusal {
            code: "delete_research_running".into(),
            reason: format!(
                "Its research is still queued or running ({jobs}). Cancel it or let it finish, then delete the project."
            ),
        }));
    }
    Ok(None)
}

/// Checks again, after the agent wait and just before the first folder moves, what can
/// change while a Delete everything waits. Inputs: the transaction, the project and the
/// folders the delete would move. Output: the refusal code, or None.
pub(crate) fn delete_recheck(
    tx: &Transaction<'_>,
    project: &ProjectId,
    moved: &[PathBuf],
) -> Result<Option<String>, EngineError> {
    let projects = guarded(tx)?;
    Ok(delete_blocked(tx, project, &projects, moved)?.map(|refusal| refusal.code))
}

/// Forgets a Delete everything's plan once its exchange ended, whatever its outcome, so a
/// projects home move may go again; with no exchange, as the engine starts, it forgets every
/// plan an earlier engine process left. Inputs: the connection and the exchange. Output: Ok,
/// or any SQLite failure.
pub(super) fn delete_settle(db: &Connection, exchange: Option<u64>) -> Result<(), EngineError> {
    match exchange {
        Some(exchange) => db.execute(
            "DELETE FROM projects_delete_pending WHERE exchange=?1",
            [i64::try_from(exchange).map_err(error)?],
        ),
        None => db.execute("DELETE FROM projects_delete_pending", []),
    }
    .map_err(error)?;
    Ok(())
}

/// The tool caches (T8) of a project removed from the dashboard, for the record its row
/// removal writes, so a different project added later at one of its folders starts with
/// none (T8 r1 secreview L2): those of its folder and the repositories it declares, each
/// left out while another project still names that folder, whose agents use the cache.
/// Inputs: the transaction, the database path and the project. Output: the records, none
/// for an unknown project.
pub(super) fn forgotten_caches(
    tx: &Transaction<'_>,
    path: &Path,
    project: &ProjectId,
) -> Result<Vec<home::RecordedFile>, EngineError> {
    let projects = guarded(tx)?;
    let Some(found) = projects.iter().find(|other| &other.project_id == project) else {
        return Ok(Vec::new());
    };
    let kept: Vec<&PathBuf> = projects
        .iter()
        .filter(|other| &other.project_id != project)
        .flat_map(|other| std::iter::once(&other.folder).chain(&other.repositories))
        .collect();
    let folders: Vec<PathBuf> = std::iter::once(&found.folder)
        .chain(&found.repositories)
        .filter(|folder| !kept.contains(folder))
        .cloned()
        .collect();
    Ok(home::tool_cache_files(path, &folders))
}

/// Records engine files in the caller's write, for the live and the start-time finish to
/// remove. Inputs: the transaction and the records. Output: Ok, or any SQLite failure.
pub(super) fn record_files(
    tx: &Transaction<'_>,
    files: &[home::RecordedFile],
) -> Result<(), EngineError> {
    for (name, inode) in files {
        tx.execute(
            "INSERT OR IGNORE INTO projects_delete_files(name,inode) VALUES (?1,?2)",
            params![name, inode],
        )
        .map_err(error)?;
    }
    Ok(())
}

/// Finishes the engine files a delete or a Remove from dashboard recorded and never removed,
/// at engine start and right after a Remove from dashboard, on a thread of its own so no
/// owner job waits on a folder, and clears through the job the live path uses: the record's
/// names starting with `prefix` only, at most `DELETE_FILES_PER_START` a call. A tool cache
/// record naming the cache of a folder a project still names, such as one added again at
/// that folder since, removes nothing and is finished, so it never takes a cache that
/// project's agents use (T8-R2-N3). Inputs: the connection, the database path, the storage
/// queue and the prefix. Output: Ok once the thread started, or at once with nothing
/// recorded.
pub(super) fn delete_files_finish(
    db: &Connection,
    path: &Path,
    jobs: mpsc::Sender<Job>,
    prefix: &str,
) -> Result<(), EngineError> {
    let mut query = db
        .prepare(
            "SELECT name, inode FROM projects_delete_files
             WHERE substr(name, 1, length(?2)) = ?2 ORDER BY name LIMIT ?1",
        )
        .map_err(error)?;
    let files = query
        .query_map(params![DELETE_FILES_PER_START, prefix], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    if files.is_empty() {
        return Ok(());
    }
    let in_use = caches_in_use(db, path)?;
    let (kept, files): (Vec<_>, Vec<_>) = files
        .into_iter()
        .partition(|(name, _)| in_use.contains(name));
    let path = path.to_path_buf();
    std::thread::Builder::new()
        .name("crazyagent-delete-files".into())
        .spawn(move || {
            let mut done = home::remove_engine_files(&path, &files);
            for (name, _) in kept {
                eprintln!("project_files_kept name={name} kind=tool_cache_in_use");
                done.push(name);
            }
            let (reply, answer) = oneshot::channel();
            let cleared = jobs
                .blocking_send(Job::DeleteFilesClear(done, reply))
                .is_ok()
                && matches!(answer.blocking_recv(), Ok(Ok(())));
            if !cleared {
                eprintln!("project_delete_files_not_cleared cause=storage_unavailable");
            }
        })
        .map_err(error)?;
    Ok(())
}

/// The record names of the tool caches (T8) whose folders a project the guards keep names
/// now, its folder or a repository it declares (T8-R2-N3). Inputs: the connection and the
/// database path. Output: the names, none for a cache that is not there.
fn caches_in_use(db: &Connection, path: &Path) -> Result<Vec<String>, EngineError> {
    let tx = db.unchecked_transaction().map_err(error)?;
    let folders: Vec<PathBuf> = guarded(&tx)?
        .into_iter()
        .flat_map(|project| std::iter::once(project.folder).chain(project.repositories))
        .collect();
    Ok(home::tool_cache_files(path, &folders)
        .into_iter()
        .map(|(name, _)| name)
        .collect())
}

/// Clears the record of engine files that are finished, the last step of a delete. Inputs:
/// the connection and the finished names. Output: Ok once none of them is recorded.
pub(super) fn delete_files_clear(db: &Connection, names: &[String]) -> Result<(), EngineError> {
    let tx = db.unchecked_transaction().map_err(error)?;
    for name in names {
        tx.execute("DELETE FROM projects_delete_files WHERE name=?1", [name])
            .map_err(error)?;
    }
    tx.commit().map_err(error)
}

const DELETE_ROWS_MAX: i64 = 100_000;
const DELETE_FILES_PER_START: i64 = 1024;

/// Plans a Delete everything before the owner transport moves anything: the owner's grant
/// and revision, the project, its exact name and [`delete_check`] are checked, then Stop all
/// and the plan's hold on projects home moves are recorded in the same write, so the runtime
/// stops the project's agents while the transport waits and no move starts until the
/// exchange settles. Inputs: the connection, database path, admission, the DeleteProject
/// request and the transport exchange running it. Output: the plan with the project's
/// research jobs, or the refusal code, with nothing written on a refusal.
pub(super) fn delete_plan(
    db: &mut Connection,
    path: &Path,
    a: &Admission,
    request: &OwnerRequest,
    exchange: u64,
) -> Result<DeletePlanned, EngineError> {
    let exchange = i64::try_from(exchange).map_err(error)?;
    assert!(exchange > 0);
    let OwnerOperation::Apply {
        mutation: OwnerMutation::DeleteProject { project_id, name },
    } = &request.operation
    else {
        return Err(error("delete_plan_invalid"));
    };
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    check_owner_admission(&tx, a)?;
    if owner_revision(&tx)? != request.expected_revision.value() {
        return Ok(Err("stale_revision".into()));
    }
    let Some(checked) = delete_check(&tx, project_id)? else {
        return Ok(Err("invalid_project".into()));
    };
    if &checked.name != name {
        return Ok(Err("delete_name_mismatch".into()));
    }
    let home = match checked.planned {
        Ok((home, _)) => home,
        Err(refusal) => return Ok(Err(refusal.code)),
    };
    let ids = |sql: &str| -> Result<Vec<i64>, EngineError> {
        let mut statement = tx.prepare(sql).map_err(error)?;
        let rows = statement
            .query_map(params![project_id.as_str(), DELETE_ROWS_MAX], |row| {
                row.get(0)
            })
            .map_err(error)?;
        rows.collect::<Result<_, _>>().map_err(error)
    };
    let agents = ids("SELECT id FROM team_agents WHERE project=?1 ORDER BY id LIMIT ?2")?;
    let research = ids("SELECT id FROM research_jobs WHERE project=?1 ORDER BY id LIMIT ?2")?;
    crate::team::pause(&tx, project_id, crate::team::now_ms())?;
    tx.execute(
        "INSERT INTO projects_delete_pending(exchange) VALUES (?1)",
        [exchange],
    )
    .map_err(error)?;
    tx.commit().map_err(error)?;
    let plan = home::DeletePlan {
        database: path.to_path_buf(),
        project: project_id.clone(),
        home,
        folders: checked.folders,
        agents,
    };
    Ok(Ok((plan, research)))
}

/// Plans a MoveProjectsHome that moves the project folders and records the
/// move as pending before any disk work, so a move whose result is never
/// recorded is still finished from what landed on disk. The pending record is
/// also the hold: no agent or research run starts in the old home until the move
/// is finished or forgotten (`crate::projects_home::move_holds`). The check and
/// the record are one write transaction, so a launch recorded first refuses the
/// move and a launch recorded later sees the hold.
/// Input: the connection, database path, owner admission, the revision the
/// owner saw, the new home, the request and the transport exchange that will
/// run the move. Output: the checked plan, or the refusal code
/// (`home_move_in_progress` while another move is pending,
/// `home_move_delete_in_progress` while a Delete everything is planned, or the code
/// `agent_work_inside` names while agent work is inside the home).
pub(super) fn move_plan(
    db: &mut Connection,
    path: &Path,
    a: &Admission,
    expected: &Cursor,
    picked: &str,
    request: &RequestId,
    exchange: u64,
) -> Result<MovePlan, EngineError> {
    let exchange = i64::try_from(exchange).map_err(error)?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    check_owner_admission(&tx, a)?;
    if owner_revision(&tx)? != expected.value() {
        return Err(error("stale_revision"));
    }
    if pending_move(&tx)?.is_some() {
        return Err(error("home_move_in_progress"));
    }
    if home::pending(&tx)?.is_some() {
        return Err(error("move_project_in_progress"));
    }
    if delete_planned(&tx)? {
        return Err(error("home_move_delete_in_progress"));
    }
    if adding(&tx)? {
        return Err(error("home_add_in_progress"));
    }
    let saved = home::load(&tx)?.ok_or_else(|| error("projects_home_unset"))?;
    if let Some(code) = agent_work_inside(&tx, &saved)? {
        return Err(error(code));
    }
    let plan = home::plan_move(&saved, Path::new(picked), &engine_root(path)?)?;
    tx.execute(
        "INSERT INTO projects_home_move VALUES (1, ?1, ?2, ?3, ?4, ?5)",
        params![
            request.as_str(),
            exchange,
            text(&saved)?,
            text(plan.destination())?,
            serde_json::to_string(plan.folders()).map_err(error)?
        ],
    )
    .map_err(error)?;
    tx.commit().map_err(error)?;
    Ok(plan)
}

/// Plans a MoveProjectHome before the owner transport moves or copies the project's folder
/// and records the move as pending in the same write, which is also the hold on launches
/// in that folder (`crate::projects_home::move_holds`). Input: the connection, database
/// path, owner admission, the revision the owner saw, the project, Move or Copy with the
/// owner's overrides, the request and the transport exchange that will run the move.
/// Output: the checked plan, or the refusal code (`stale_revision` when the engine moved
/// on, `home_move_in_progress` or `home_move_delete_in_progress` while the projects home
/// itself is being changed, else what `crate::projects_home::plan` names); nothing on disk
/// changes.
#[allow(clippy::too_many_arguments)]
pub(super) fn project_move_plan(
    db: &mut Connection,
    path: &Path,
    a: &Admission,
    expected: &Cursor,
    project: &ProjectId,
    mode: AddMode,
    overrides: &[AddOverride],
    request: &RequestId,
    exchange: u64,
) -> Result<home::ProjectMove, EngineError> {
    let exchange = i64::try_from(exchange).map_err(error)?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    check_owner_admission(&tx, a)?;
    if owner_revision(&tx)? != expected.value() {
        return Err(error("stale_revision"));
    }
    if pending_move(&tx)?.is_some() {
        return Err(error("home_move_in_progress"));
    }
    if delete_planned(&tx)? {
        return Err(error("home_move_delete_in_progress"));
    }
    let plan = home::plan(
        &tx,
        path,
        &engine_root(path)?,
        &guarded(&tx)?,
        &home::Asked {
            project,
            mode,
            overrides,
            request: request.as_str(),
            exchange,
        },
    )?;
    tx.commit().map_err(error)?;
    Ok(plan)
}

/// Finishes or forgets a pending project move in its own transaction. Input: the
/// connection, the transport exchange the move must belong to (None at engine start,
/// before any exchange, which finishes any pending move from disk) and whether that
/// exchange ran its disk work. A move whose work never ran is forgotten; any other is
/// finished as `crate::projects_home::finish` does, a Copy never from disk alone; a move
/// another exchange planned is left alone. Output: Ok, or any SQLite failure.
pub(super) fn project_move_settle(
    db: &mut Connection,
    exchange: Option<u64>,
    ran: bool,
) -> Result<(), EngineError> {
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let finished = match exchange {
        None => home::finish(&tx, |_| true, false)?,
        Some(exchange) => {
            let exchange = i64::try_from(exchange).map_err(error)?;
            if ran {
                home::finish(&tx, |pending| pending.exchange == exchange, false)?
            } else {
                home::forget(&tx, exchange)?;
                None
            }
        }
    };
    tx.commit().map_err(error)?;
    if let Some(home::Finished::Moved { repointed }) = finished {
        eprintln!("project_move_settled copies_repointed={repointed}");
    }
    Ok(())
}

/// Checks again, under the hold of the pending move, that no agent work appeared in the old
/// home since the plan: the last check before the first folder moves. Inputs: the
/// connection and the transport exchange running the move. Output: the refusal code when
/// agent work is there now, or `home_move_not_pending` when this exchange has no move.
pub(super) fn recheck(
    db: &mut Connection,
    exchange: u64,
) -> Result<Result<(), String>, EngineError> {
    let exchange = i64::try_from(exchange).map_err(error)?;
    let tx = db.transaction().map_err(error)?;
    let pending = pending_move(&tx)?
        .filter(|pending| pending.exchange == exchange)
        .ok_or_else(|| error("home_move_not_pending"))?;
    let found = agent_work_inside(&tx, &pending.origin)?;
    tx.commit().map_err(error)?;
    Ok(found.map_or(Ok(()), |code| Err(code.to_owned())))
}

/// Names the agent work inside the home that a home move would cut off, as its refusal
/// code: `home_move_agent_work` for a team agent's worktree or own clone, an agent starting
/// there, one waiting for removal, or a change set that is open or has a step in flight;
/// `home_move_research_running` for a research job running for a project in the home,
/// whose worktree is a linked worktree of the project's repository; otherwise
/// `home_move_changes_restorable` for a discarded change set that can still be restored,
/// for `crate::landing::RESTORE_WINDOW_MS` after the discard. Their records, each own
/// clone's alternates line and each linked worktree's git files name absolute paths inside
/// the home. A finished research job's worktree is never used again, so it holds no move.
/// Inputs: the transaction and the saved home. Output: the code, or None.
fn agent_work_inside(
    tx: &Transaction<'_>,
    home: &Path,
) -> Result<Option<&'static str>, EngineError> {
    if held_inside(
        tx,
        home,
        "team_worktrees",
        "SELECT repository, worktree FROM team_worktrees",
        params![],
    )? || held_inside(
        tx,
        home,
        "team_retiring",
        "SELECT repository, worktree FROM team_retiring",
        params![],
    )? || held_inside(
        tx,
        home,
        "team_agents",
        "SELECT json_extract(s.body, '$.folder') FROM team_agents a
         JOIN project_setups s ON s.project=a.project WHERE a.status='starting'",
        params![],
    )? || held_inside(
        tx,
        home,
        "landing_change_sets",
        "SELECT repository, worktree FROM landing_change_sets
         WHERE state IN ('pending','conflict','updating') OR intent IS NOT NULL",
        params![],
    )? {
        return Ok(Some("home_move_agent_work"));
    }
    if held_inside(
        tx,
        home,
        "research_jobs",
        "SELECT json_extract(s.body, '$.folder') FROM research_jobs j
         JOIN project_setups s ON s.project=j.project WHERE j.status='running'",
        params![],
    )? {
        return Ok(Some("home_move_research_running"));
    }
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(error)?
            .as_millis(),
    )
    .map_err(error)?;
    Ok(held_inside(
        tx,
        home,
        "landing_change_sets",
        "SELECT repository, worktree FROM landing_change_sets
         WHERE state='discarded' AND pruned=0 AND restorable_until_ms>?1",
        params![now],
    )?
    .then_some("home_move_changes_restorable"))
}

/// Tells whether a table of agent work records holds one with a path inside the home, as
/// recorded or as the path resolves now. A table the engine has not made yet holds none.
/// Inputs: the transaction, the home, the table, the query whose every column is a path
/// and that query's parameters. Output: the answer.
fn held_inside(
    tx: &Transaction<'_>,
    home: &Path,
    table: &str,
    query: &str,
    parameters: impl rusqlite::Params,
) -> Result<bool, EngineError> {
    let present = tx
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |_| Ok(()),
        )
        .optional()
        .map_err(error)?
        .is_some();
    if !present {
        return Ok(false);
    }
    let inside = |text: &str| {
        let path = Path::new(text);
        path.starts_with(home)
            || std::fs::canonicalize(path).is_ok_and(|real| real.starts_with(home))
    };
    let mut statement = tx.prepare(query).map_err(error)?;
    let columns = statement.column_count();
    let mut rows = statement.query(parameters).map_err(error)?;
    while let Some(row) = rows.next().map_err(error)? {
        for column in 0..columns {
            let path: String = row.get(column).map_err(error)?;
            if inside(&path) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// A projects home move recorded before its folders moved: its request, the
/// transport exchange running it, the old home, the new home and the
/// top-level folders the plan moves.
struct PendingMove {
    request: String,
    exchange: i64,
    origin: PathBuf,
    target: PathBuf,
    folders: Vec<String>,
}

fn text(path: &Path) -> Result<&str, EngineError> {
    path.to_str()
        .ok_or_else(|| error("projects_home_path_invalid"))
}

fn pending_move(tx: &Transaction<'_>) -> Result<Option<PendingMove>, EngineError> {
    let row: Option<(String, i64, String, String, String)> = tx
        .query_row(
            "SELECT request, exchange, origin, target, folders FROM projects_home_move
             WHERE singleton=1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(error)?;
    row.map(|(request, exchange, origin, target, folders)| {
        Ok(PendingMove {
            request,
            exchange,
            origin: PathBuf::from(origin),
            target: PathBuf::from(target),
            folders: serde_json::from_str(&folders).map_err(error)?,
        })
    })
    .transpose()
}

/// Finishes the pending projects home move from what is on disk now and
/// clears it. A folder counts as moved once it is a real folder in the new
/// home and gone from the old one. Input: the transaction, database path and
/// which pending move to finish. Output: None when no such move is pending;
/// Applied when every planned folder moved and the new home is saved,
/// otherwise Rejected. Either way exactly the set-up projects whose folders
/// moved are pointed at the new home.
fn settle(
    tx: &Transaction<'_>,
    path: &Path,
    belongs: impl Fn(&PendingMove) -> bool,
) -> Result<Option<Outcome>, EngineError> {
    let Some(pending) = pending_move(tx)? else {
        return Ok(None);
    };
    if !belongs(&pending) {
        return Ok(None);
    }
    tx.execute("DELETE FROM projects_home_move WHERE singleton=1", [])
        .map_err(error)?;
    let arrived: Vec<&OsStr> = pending
        .folders
        .iter()
        .map(OsStr::new)
        .filter(|name| {
            std::fs::symlink_metadata(pending.target.join(name))
                .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
                && std::fs::symlink_metadata(pending.origin.join(name)).is_err()
        })
        .collect();
    let outcome = if arrived.len() == pending.folders.len() {
        switch_home(tx, path, text(&pending.target)?)?
    } else {
        Outcome::Rejected {
            code: "home_move_partial".into(),
        }
    };
    rewrite_folders(tx, &pending.origin, &pending.target, |name| {
        arrived.contains(&name)
    })?;
    assert!(pending_move(tx)?.is_none());
    Ok(Some(outcome))
}

/// Finishes or forgets a pending projects home move in its own transaction.
/// Input: the connection, database path, the transport exchange the move must
/// belong to (None at engine start, before any exchange, which finishes any
/// pending move) and whether that exchange ran its disk work. A move whose
/// work never ran is forgotten; any other is finished as `settle` does; a move
/// another exchange planned is left alone. Output: true when the saved home or
/// set-up projects may have changed, so the caller restarts the home watch.
pub(super) fn settle_pending(
    db: &mut Connection,
    path: &Path,
    exchange: Option<u64>,
    ran: bool,
) -> Result<bool, EngineError> {
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let changed = match exchange {
        None => settle(&tx, path, |_| true)?.is_some(),
        Some(exchange) => {
            let exchange = i64::try_from(exchange).map_err(error)?;
            if ran {
                settle(&tx, path, |pending| pending.exchange == exchange)?.is_some()
            } else {
                tx.execute(
                    "DELETE FROM projects_home_move WHERE singleton=1 AND exchange=?1",
                    [exchange],
                )
                .map_err(error)?;
                false
            }
        }
    };
    tx.commit().map_err(error)?;
    Ok(changed)
}

/// Whether a Delete everything is planned and not yet forgotten. It keeps the projects home
/// where the plan saw it, since the plan trashes folders from that home. Input: the
/// connection. Output: true while any plan is recorded.
fn delete_planned(db: &Connection) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects_delete_pending)",
        [],
        |row| row.get(0),
    )
    .map_err(error)
}

fn switch_home(tx: &Transaction<'_>, path: &Path, picked: &str) -> Result<Outcome, EngineError> {
    match home::check_home(Path::new(picked), &engine_root(path)?) {
        Ok(canonical) => {
            home::save(tx, &canonical)?;
            Ok(Outcome::Applied)
        }
        Err(e) => Ok(Outcome::Rejected { code: e.0 }),
    }
}

/// Points every set-up project whose folder sat anywhere inside a folder of the
/// old home that `arrived` accepts at the same place in the new home, keeping an
/// onboarded project's recorded `.crazyagent/` folder and its onboarding folder,
/// then every project's repository inside such a folder, where its master works.
fn rewrite_folders(
    tx: &Transaction<'_>,
    from: &Path,
    to: &Path,
    arrived: impl Fn(&OsStr) -> bool,
) -> Result<(), EngineError> {
    let rows: Vec<(String, String)> = {
        let mut statement = tx
            .prepare("SELECT project, body FROM project_setups")
            .map_err(error)?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(error)?;
        rows.collect::<Result<_, _>>().map_err(error)?
    };
    for (project, body) in rows {
        let mut stored: crate::project_setup::StoredSetup =
            serde_json::from_str(&body).map_err(error)?;
        let folder = stored.project_folder();
        let Some(moved) = moved_path(&folder, from, to, &arrived) else {
            continue;
        };
        let recorded = if Path::new(&stored.folder) == folder {
            moved.clone()
        } else {
            moved.join(home::CRAZYAGENT_DIRECTORY)
        };
        let text = |path: &Path| {
            path.to_str()
                .map(str::to_owned)
                .ok_or_else(|| error("projects_home_path_invalid"))
        };
        stored.folder = text(&recorded)?;
        tx.execute(
            "UPDATE project_setups SET body=?1 WHERE project=?2",
            params![serde_json::to_string(&stored).map_err(error)?, project],
        )
        .map_err(error)?;
        tx.execute(
            "UPDATE onboarding SET repo=?1 WHERE project=?2",
            params![text(&moved)?, project],
        )
        .map_err(error)?;
    }
    move_repositories(tx, from, to, &arrived)
}

/// Says where a path inside the old home is after a move: the same place in the
/// new home when the home folder holding it arrived, compared component by
/// component. Inputs: the path, both homes and the arrived test. Output: the
/// moved path, or None for a path that stays where it is.
fn moved_path(
    path: &Path,
    from: &Path,
    to: &Path,
    arrived: &impl Fn(&OsStr) -> bool,
) -> Option<PathBuf> {
    let rest = path.strip_prefix(from).ok()?;
    match rest.components().next()? {
        Component::Normal(name) if arrived(name) => Some(to.join(rest)),
        _ => None,
    }
}

/// Rewrites every project's repositories inside a moved folder of the old home to
/// the same place in the new home, leaving the others. Inputs: the transaction,
/// both homes and the arrived test. Output: the project rows agree with the move,
/// or the bound a moved path breaks.
fn move_repositories(
    tx: &Transaction<'_>,
    from: &Path,
    to: &Path,
    arrived: &impl Fn(&OsStr) -> bool,
) -> Result<(), EngineError> {
    let rows: Vec<(String, String)> = {
        let mut statement = tx.prepare("SELECT id, body FROM projects").map_err(error)?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(error)?;
        rows.collect::<Result<_, _>>().map_err(error)?
    };
    for (project, body) in rows {
        let mut stored: crate::protocol::StoredProject =
            serde_json::from_str(&body).map_err(error)?;
        let mut moved = 0;
        for repository in &mut stored.spec.repositories {
            let Some(place) = moved_path(Path::new(repository.as_str()), from, to, arrived) else {
                continue;
            };
            *repository = place
                .to_str()
                .ok_or_else(|| error("projects_home_path_invalid"))?
                .to_owned();
            moved += 1;
        }
        if moved == 0 {
            continue;
        }
        stored.spec.validate().map_err(error)?;
        let changed = tx
            .execute(
                "UPDATE projects SET body=?1 WHERE id=?2",
                params![serde_json::to_string(&stored).map_err(error)?, project],
            )
            .map_err(error)?;
        assert_eq!(changed, 1);
    }
    Ok(())
}

/// Applies a recorded projects home action inside the owner transaction.
/// Input: the transaction, database path and action. Output: Applied after the
/// home is saved or a finished add is confirmed on disk, else Rejected.
pub(super) fn execute(
    tx: &Transaction<'_>,
    path: &Path,
    action: &HomeAction,
) -> Result<Outcome, EngineError> {
    let rejected = |code: &str| -> Result<Outcome, EngineError> {
        Ok(Outcome::Rejected { code: code.into() })
    };
    match action {
        HomeAction::SetHome { path: picked }
        | HomeAction::MoveHome {
            path: picked,
            move_projects: false,
        } => {
            if pending_move(tx)?.is_some() {
                return rejected("home_move_in_progress");
            }
            if adding(tx)? {
                return rejected("home_add_in_progress");
            }
            if home::pending(tx)?.is_some() {
                return rejected("move_project_in_progress");
            }
            if delete_planned(tx)? {
                return rejected("home_move_delete_in_progress");
            }
            switch_home(tx, path, picked)
        }
        HomeAction::MoveHome {
            move_projects: true,
            ..
        } => rejected("home_move_requires_transport"),
        HomeAction::Added { .. } => rejected("home_add_requires_transport"),
        HomeAction::MoveProject { .. } => rejected("move_project_requires_transport"),
        HomeAction::Trash { path: picked } => {
            let Some(saved) = home::load(tx)? else {
                return rejected("projects_home_unset");
            };
            let folder = match home::plan_trash(&saved, picked, &guarded(tx)?) {
                Ok(folder) => folder,
                Err(e) => return rejected(&e.0),
            };
            match home::trash(&folder) {
                Ok(()) => Ok(Outcome::Applied),
                Err(e) => rejected(&e.0),
            }
        }
    }
}

/// Records the result of disk work the owner transport carried out for
/// AddExistingFolder, MoveProjectsHome with `move_projects` or MoveProjectHome.
/// Input: the transaction, database path, the request, the mutation's command
/// and the transport's effect. Output: Rejected with the transport's refusal code;
/// for an add, Applied once the real folder the plan named is confirmed in the
/// home (and, for Move, gone from its old place); for a home move, the move
/// pending under this request finished as `settle` does; for a project move, the
/// move pending under this request finished as `crate::projects_home::finish` does,
/// Applied once the project's stored folder and agent copies point into the home.
/// A move the transport refused or failed stays pending for the transport's settle job.
pub(super) fn record(
    tx: &Transaction<'_>,
    path: &Path,
    request: &RequestId,
    command: &Command,
    effect: &HomeEffect,
) -> Result<Outcome, EngineError> {
    let Command::ProjectsHome { action } = command else {
        return Err(error("home_record_invalid"));
    };
    if let HomeAction::Added { .. } = action {
        tx.execute(
            "DELETE FROM projects_home_add WHERE request=?1 AND ended IS NULL",
            [request.as_str()],
        )
        .map_err(error)?;
    }
    let real = match effect {
        Err(code) => return Ok(Outcome::Rejected { code: code.clone() }),
        Ok(real) => real,
    };
    let Some(saved) = home::load(tx)? else {
        return Ok(Outcome::Rejected {
            code: "projects_home_unset".into(),
        });
    };
    match action {
        HomeAction::Added { mode, .. } => {
            let real = real
                .as_deref()
                .and_then(Path::to_str)
                .ok_or_else(|| error("home_record_invalid"))?;
            Ok(match home::verify_added(&saved, real, *mode) {
                Ok(()) => Outcome::Applied,
                Err(e) => Outcome::Rejected { code: e.0 },
            })
        }
        HomeAction::MoveHome {
            move_projects: true,
            ..
        } if real.is_none() => settle(tx, path, |pending| pending.request == request.as_str())?
            .ok_or_else(|| error("home_record_invalid")),
        HomeAction::MoveProject { .. } if real.is_some() => {
            let finished = home::finish(tx, |pending| pending.request == request.as_str(), true)?
                .ok_or_else(|| error("home_record_invalid"))?;
            Ok(match finished {
                home::Finished::Moved { repointed } => {
                    eprintln!("project_moved copies_repointed={repointed}");
                    Outcome::Applied
                }
                home::Finished::Left => Outcome::Rejected {
                    code: "home_add_unverified".into(),
                },
            })
        }
        _ => Err(error("home_record_invalid")),
    }
}
