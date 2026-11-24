use crate::protocol::ProjectId;
use rusqlite::Connection;
use sdk::Refusal;
use sdk::generated::editor::{self, EditorLaunch};

/// The project id a plugin named does not parse as a project id.
pub(crate) const PROJECT_INVALID: &str = "editor_project_invalid";

/// No IDE server runs confined on this system, so no editor starts here.
pub(crate) const PLATFORM_UNQUALIFIED: &str = "ide_platform_unqualified";

/// The project id names no project the owner set up in the engine.
pub(crate) const PROJECT_UNKNOWN: &str = "editor_project_unknown";

/// An editor call that reached no spawner: the engine has none registered for plugins.
pub(crate) const SPAWNER_MISSING: &str = "editor_spawner_missing";

/// Starts the editor server of one project the engine knows, through the engine's IDE
/// register, which keeps the server's ports, paths and command. Input: the project.
/// Output: the server's process id, or the run's code.
pub(crate) type Spawner = fn(&ProjectId) -> Result<u32, &'static str>;

/// The `editor.spawn` provider: it takes only a project id from the plugin, refuses one that
/// does not parse, a system where the IDE never runs confined and a project the engine does
/// not know, then hands the project to the engine's spawner and returns only the pid. With
/// no spawner registered every call is refused.
pub(crate) struct EditorCaps<'t> {
    projects: &'t Connection,
    spawner: Option<Spawner>,
}

impl<'t> EditorCaps<'t> {
    /// The provider the engine registers: no spawner, so every call fails closed. Input:
    /// the engine database the projects are read from. Output: the provider.
    pub(crate) fn unregistered(projects: &'t Connection) -> Self {
        Self {
            projects,
            spawner: None,
        }
    }

    /// A provider over one spawner. Inputs: the engine database and the spawner. Output:
    /// the provider.
    #[cfg(test)]
    pub(crate) fn with(projects: &'t Connection, spawner: Spawner) -> Self {
        Self {
            projects,
            spawner: Some(spawner),
        }
    }
}

impl editor::Provide for EditorCaps<'_> {
    fn spawn(&self, _: &'static str, launch: EditorLaunch) -> Result<u32, Refusal> {
        let rejected = |code: &str| Refusal::Rejected {
            code: code.to_owned(),
        };
        let failed = |code: &str| Refusal::Failed {
            code: code.to_owned(),
        };
        let project = ProjectId::new(launch.project).map_err(|_| rejected(PROJECT_INVALID))?;
        if !qualified() {
            return Err(failed(PLATFORM_UNQUALIFIED));
        }
        match crate::project_setup::load_setup(self.projects, &project) {
            Ok(Some(_)) => {}
            Ok(None) => return Err(rejected(PROJECT_UNKNOWN)),
            Err(_) => return Err(failed(PROJECT_UNKNOWN)),
        }
        let spawner = self.spawner.ok_or_else(|| failed(SPAWNER_MISSING))?;
        let pid = spawner(&project).map_err(failed)?;
        assert!(pid > 0);
        Ok(pid)
    }
}

/// Whether the IDE server runs confined on this system, as its own open requires.
/// Output: true only on a qualified Unix system.
fn qualified() -> bool {
    #[cfg(unix)]
    return crate::ide::sandbox::qualified();
    #[cfg(not(unix))]
    return false;
}

#[cfg(test)]
mod tests;
