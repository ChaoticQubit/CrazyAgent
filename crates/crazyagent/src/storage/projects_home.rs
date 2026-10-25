use super::*;
use crate::projects_home::{
    self as home, AddMode, AddPlan, FoundProject, HomeAction, HomeEffect, HomeWatch, MovePlan,
    ProjectsHome, Registered,
};
use std::path::PathBuf;

const FOUND_REPLY_BYTES: usize = 48 * 1024;
const REGISTERED_MAX: usize = 256;

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

/// Lists the set-up projects for the projects home: each project's own
/// folder and whether its onboarding is still under way, read from the
/// onboarding table. An onboarded project is Ready once both its graph and
/// mapping steps are done; a project made with New Project has no
/// onboarding row and is Ready at once.
fn registered(tx: &Transaction<'_>) -> Result<Vec<Registered>, EngineError> {
    let done = crate::onboarding::Step::Done.columns().0;
    let mut statement = tx
        .prepare(
            "SELECT s.body, o.graph_state, o.mapping_state FROM project_setups s
             LEFT JOIN onboarding o ON o.project=s.project
             ORDER BY s.project LIMIT ?1",
        )
        .map_err(error)?;
    let rows = statement
        .query_map([REGISTERED_MAX as i64], |row| {
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
    assert!(projects.len() <= REGISTERED_MAX);
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

/// Plans a MoveProjectsHome that moves the project folders, before any disk work.
/// Input: the connection, database path, owner admission, the revision the
/// owner saw and the new home. Output: the checked plan or the refusal code.
pub(super) fn move_plan(
    db: &mut Connection,
    path: &Path,
    a: &Admission,
    expected: &Cursor,
    picked: &str,
) -> Result<MovePlan, EngineError> {
    let tx = db.transaction().map_err(error)?;
    check_owner_admission(&tx, a)?;
    if owner_revision(&tx)? != expected.value() {
        return Err(error("stale_revision"));
    }
    let saved = home::load(&tx)?.ok_or_else(|| error("projects_home_unset"))?;
    home::plan_move(&saved, Path::new(picked), &engine_root(path)?)
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

/// Points every set-up project whose folder sat in the old home at the same
/// folder name in the new home, after the transport moved them, keeping an
/// onboarded project's recorded `.crazyagent/` folder and onboarding folder.
fn rewrite_folders(tx: &Transaction<'_>, from: &Path, to: &Path) -> Result<(), EngineError> {
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
        if folder.parent() != Some(from) {
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
/// Input: the transaction, database path, the mutation's command and the
/// transport's effect. Output: Rejected with the transport's refusal code;
/// for an add, Applied once the real folder the plan named is confirmed in the
/// home (and, for Move, gone from its old place); for a home move, Applied once
/// the new home is saved and set-up projects point at their moved folders.
pub(super) fn record(
    tx: &Transaction<'_>,
    path: &Path,
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
            path: picked,
            move_projects: true,
        } if real.is_none() => {
            let outcome = switch_home(tx, path, picked)?;
            if outcome == Outcome::Applied {
                let next = home::load(tx)?.ok_or_else(|| error("projects_home_unset"))?;
                rewrite_folders(tx, &saved, &next)?;
            }
            Ok(outcome)
        }
        _ => Err(error("home_record_invalid")),
    }
}
