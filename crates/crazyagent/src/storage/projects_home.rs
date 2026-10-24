use super::*;
use crate::projects_home::{
    self as home, AddMode, AddPlan, FoundProject, HomeAction, HomeWatch, ProjectsHome, Registered,
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

fn registered(tx: &Transaction<'_>) -> Result<Vec<Registered>, EngineError> {
    let mut statement = tx
        .prepare("SELECT body FROM project_setups ORDER BY project LIMIT ?1")
        .map_err(error)?;
    let bodies = statement
        .query_map([REGISTERED_MAX as i64], |row| row.get::<_, String>(0))
        .map_err(error)?;
    let mut projects = Vec::new();
    for body in bodies {
        let stored: crate::project_setup::StoredSetup =
            serde_json::from_str(&body.map_err(error)?).map_err(error)?;
        projects.push(Registered {
            project_id: stored.project_id,
            name: stored.name,
            folder: PathBuf::from(stored.folder),
            onboarding: false,
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
                (Some(watch), _) => watch.folders().1,
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
/// Input: the connection, database path, owner admission, source and mode.
/// Output: the checked plan, or the refusal code; nothing on disk changes.
pub(super) fn add_plan(
    db: &mut Connection,
    path: &Path,
    a: &Admission,
    source: &str,
    mode: AddMode,
) -> Result<AddPlan, EngineError> {
    let tx = db.transaction().map_err(error)?;
    check_owner_admission(&tx, a)?;
    let saved = home::load(&tx)?.ok_or_else(|| error("projects_home_unset"))?;
    home::plan_add(&saved, &engine_root(path)?, source, mode)
}

/// Applies a recorded projects home action inside the owner transaction.
/// Input: the transaction, database path and action. Output: Applied after the
/// home is saved or a finished add is confirmed on disk, else Rejected.
pub(super) fn execute(
    tx: &Transaction<'_>,
    path: &Path,
    action: &HomeAction,
) -> Result<Outcome, EngineError> {
    let rejected =
        |e: EngineError| -> Result<Outcome, EngineError> { Ok(Outcome::Rejected { code: e.0 }) };
    match action {
        HomeAction::SetHome { path: picked } => {
            match home::check_home(Path::new(picked), &engine_root(path)?) {
                Ok(canonical) => {
                    home::save(tx, &canonical)?;
                    Ok(Outcome::Applied)
                }
                Err(e) => rejected(e),
            }
        }
        HomeAction::Added { source, mode } => {
            let Some(saved) = home::load(tx)? else {
                return rejected(error("projects_home_unset"));
            };
            match home::verify_added(&saved, source, *mode) {
                Ok(()) => Ok(Outcome::Applied),
                Err(e) => rejected(e),
            }
        }
    }
}
