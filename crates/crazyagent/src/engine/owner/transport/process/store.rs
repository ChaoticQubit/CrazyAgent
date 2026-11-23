use super::*;

pub(super) fn read_only(root: &Path) -> Result<rusqlite::Connection, EngineError> {
    let connection = rusqlite::Connection::open_with_flags(
        root.join("engine.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| failure("process_unavailable"))?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|_| failure("process_unavailable"))?;
    Ok(connection)
}

/// Reads what a launch now gives every agent of the project: `agent_home::text`, the
/// render every launch uses, for the project's mode. A launch-size refusal names its larger
/// part in a code the Process page does not answer with, so it reads as
/// `agent_instructions_too_large`. Inputs: the engine root and the project. Output: the
/// text, `process_project_unknown` for a project not set up, or the code of the part the
/// engine cannot read.
pub(super) fn given_text(root: &Path, project: &ProjectId) -> Result<String, EngineError> {
    let connection = read_only(root)?;
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| failure("process_unavailable"))?;
    let agent = crate::launch_context::agent_project(&tx, project)
        .map_err(|_| failure("process_unavailable"))?;
    drop(tx);
    if agent.folder.is_none() {
        return Err(failure("process_project_unknown"));
    }
    crate::agent_home::text(&agent)
        .map(|given| given.text)
        .map_err(|problem| {
            if problem.0.starts_with("agent_instructions_too_large") {
                failure("agent_instructions_too_large")
            } else {
                EngineError(problem.0)
            }
        })
}

/// Finds one project's folder from its stored setup. Inputs: the engine root and the
/// project. Output: the folder, or `process_project_unknown`.
pub(super) fn project_folder(root: &Path, project: &ProjectId) -> Result<PathBuf, EngineError> {
    let connection = read_only(root)?;
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| failure("process_unavailable"))?;
    crate::launch_context::agent_project(&tx, project)
        .map_err(|_| failure("process_unavailable"))?
        .folder
        .ok_or_else(|| failure("process_project_unknown"))
}
