use super::*;
use crate::projects_home::{
    self as home, AddMode, AddPlan, FoundProject, HomeAction, HomeEffect, HomeWatch, MovePlan,
    ProjectsHome, Registered,
};
use std::ffi::OsStr;
use std::path::PathBuf;

const FOUND_REPLY_BYTES: usize = 48 * 1024;

/// Starts the folder watch for the saved projects home, when there is one.
/// Input: the engine connection. Output: the running watch, or None when no
/// home is saved or the OS refuses the watch; reads then scan the folder.
pub(super) fn watch_saved(connection: &Connection) -> Option<HomeWatch> {
    let saved = home::load(connection).ok().flatten()?;
    match HomeWatch::start(&saved) {
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
/// Found list see them all: each project's folder and whether onboarding
/// is still under way (Ready once its graph and mapping steps are done, or
/// at once with no onboarding row). The engine's snapshot bound caps how
/// many projects exist.
fn registered(tx: &Transaction<'_>) -> Result<Vec<Registered>, EngineError> {
    let done = crate::onboarding::Step::Done.columns().0;
    let mut statement = tx
        .prepare(
            "SELECT s.body, o.graph_state, o.mapping_state FROM project_setups s
             LEFT JOIN onboarding o ON o.project=s.project
             ORDER BY s.project",
        )
        .map_err(error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(error)?;
    let mut projects = Vec::new();
    for row in rows {
        let (body, graph, mapping) = row.map_err(error)?;
        let stored: crate::project_setup::StoredSetup =
            serde_json::from_str(&body).map_err(error)?;
        let onboarding = graph
            .zip(mapping)
            .is_some_and(|(graph, mapping)| graph != done || mapping != done);
        projects.push(Registered {
            folder: stored.project_folder(),
            project_id: stored.project_id,
            name: stored.name,
            onboarding,
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
    watch: Option<&HomeWatch>,
) -> Result<OwnerReadReply, EngineError> {
    let tx = db.transaction().map_err(error)?;
    check_owner_admission(&tx, a)?;
    let revision = owner_revision(&tx)?;
    let saved = home::load(&tx)?;
    let live = watch.filter(|watch| saved.as_deref() == Some(watch.home()) && watch.watching());
    let data = match query {
        OwnerRead::ProjectsHome => OwnerData::ProjectsHome {
            home: ProjectsHome {
                path: saved.map(|path| path.to_string_lossy().into_owned()),
                watching: live.is_some(),
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
pub(super) fn add_plan(
    db: &mut Connection,
    path: &Path,
    a: &Admission,
    expected: &Cursor,
    source: &str,
    mode: AddMode,
) -> Result<AddPlan, EngineError> {
    let tx = db.transaction().map_err(error)?;
    check_owner_admission(&tx, a)?;
    if owner_revision(&tx)? != expected.value() {
        return Err(error("stale_revision"));
    }
    let saved = home::load(&tx)?.ok_or_else(|| error("projects_home_unset"))?;
    home::plan_add(&saved, &engine_root(path)?, source, mode)
}

/// Plans a MoveProjectsHome that moves the project folders and records the
/// move as pending before any disk work, so a move whose result is never
/// recorded is still finished from what landed on disk.
/// Input: the connection, database path, owner admission, the revision the
/// owner saw, the new home, the request and the transport exchange that will
/// run the move. Output: the checked plan, or the refusal code
/// (`home_move_in_progress` while another move is pending).
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
    let tx = db.transaction().map_err(error)?;
    check_owner_admission(&tx, a)?;
    if owner_revision(&tx)? != expected.value() {
        return Err(error("stale_revision"));
    }
    if pending_move(&tx)?.is_some() {
        return Err(error("home_move_in_progress"));
    }
    let saved = home::load(&tx)?.ok_or_else(|| error("projects_home_unset"))?;
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

fn switch_home(tx: &Transaction<'_>, path: &Path, picked: &str) -> Result<Outcome, EngineError> {
    match home::check_home(Path::new(picked), &engine_root(path)?) {
        Ok(canonical) => {
            home::save(tx, &canonical)?;
            Ok(Outcome::Applied)
        }
        Err(e) => Ok(Outcome::Rejected { code: e.0 }),
    }
}

/// Points every set-up project whose folder sat in the old home, under a name
/// `arrived` accepts, at the same folder name in the new home, keeping an
/// onboarded project's recorded `.crazyagent/` folder and onboarding folder.
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
        let Some(name) = folder.file_name() else {
            continue;
        };
        if folder.parent() != Some(from) || !arrived(name) {
            continue;
        }
        let moved = to.join(name);
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
        } => switch_home(tx, path, picked),
        HomeAction::MoveHome {
            move_projects: true,
            ..
        } => rejected("home_move_requires_transport"),
        HomeAction::Added { .. } => rejected("home_add_requires_transport"),
        HomeAction::Trash { path: picked } => {
            let Some(saved) = home::load(tx)? else {
                return rejected("projects_home_unset");
            };
            let folder = match home::plan_trash(&saved, picked, &registered(tx)?) {
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
/// AddExistingFolder or MoveProjectsHome with `move_projects`.
/// Input: the transaction, database path, the request, the mutation's command
/// and the transport's effect. Output: Rejected with the transport's refusal code;
/// for an add, Applied once the real folder the plan named is confirmed in the
/// home (and, for Move, gone from its old place); for a home move, the move
/// pending under this request finished as `settle` does. A move the transport
/// refused or failed stays pending for the transport's settle job.
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
        _ => Err(error("home_record_invalid")),
    }
}
