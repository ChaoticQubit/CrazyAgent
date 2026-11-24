use crate::protocol::ProjectId;
use kernel_sandbox::policy::{AgentAccess, AgentNetwork, IdeAgentFiles};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where a project lives for its IDE terminal: its canonical folder, the extra read-only
/// folders the owner saved for its agents, and the level, network choice and IDE agent
/// folders choice it saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub folder: PathBuf,
    pub extra_reads: Vec<PathBuf>,
    pub access: AgentAccess,
    pub network: AgentNetwork,
    pub agent_files: IdeAgentFiles,
}

/// Reads the project's folder, its extra read-only folders and the level, network and agent
/// folders choice it saved from the engine database, opened read-only so a terminal launch never writes engine
/// state; a saved policy that cannot be read is an error, so no level is guessed. Inputs: the engine
/// data root and the project. Output: the place, or an error naming what is missing.
pub fn project_place(root: &Path, project: &ProjectId) -> Result<Place, String> {
    use rusqlite::OpenFlags;
    let db = rusqlite::Connection::open_with_flags(
        root.join("engine.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| "the engine database could not be opened".to_owned())?;
    db.busy_timeout(Duration::from_secs(5))
        .map_err(|_| "the engine database is busy".to_owned())?;
    let setup = {
        let tx = db
            .unchecked_transaction()
            .map_err(|_| "the engine database could not be read".to_owned())?;
        crate::project_setup::load_setup(&tx, project).map_err(|error| error.to_string())?
    }
    .ok_or("the project is unknown")?;
    let folder = setup
        .project_folder()
        .canonicalize()
        .map(kernel_sandbox::paths::one_spelling)
        .map_err(|_| "the project folder is missing".to_owned())?;
    let extra_reads = super::access_store::read::load(&db, project.as_str())
        .map_err(|_| "the project's read-only folders could not be read".to_owned())?;
    let policy = crate::sandbox::policy_store::load(&db, project.as_str())
        .map_err(|_| "the project's saved access could not be read".to_owned())?;
    assert!(folder.is_absolute());
    Ok(Place {
        folder,
        extra_reads,
        access: policy.access,
        network: policy.network,
        agent_files: policy.ide_agent_files,
    })
}
