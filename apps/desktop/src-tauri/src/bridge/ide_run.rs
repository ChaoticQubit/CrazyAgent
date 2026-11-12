use super::ide_reveal::ide_error;
use super::*;
use crazyagent::ide::bridge::run_command::{self, RunMode};
use crazyagent::protocol::ProjectId;

fn project(project_id: String) -> Result<ProjectId, BridgeError> {
    ProjectId::new(project_id).map_err(|_| failure("ide_request_invalid"))
}

impl NativeBridge {
    /// Types one command the owner confirmed into a new terminal of the project's open
    /// IDE, run without the bridge lock. The single-command rule is checked here before
    /// the engine is asked, and the engine checks it again with the project's Run setting.
    /// Inputs: the project id and the command as confirmed. Output: done once the IDE
    /// typed it, or the refusal.
    pub fn ide_run(
        &self,
        project_id: String,
        command: String,
    ) -> Result<Unlocked<()>, BridgeError> {
        let project = project(project_id)?;
        if let Some(code) = run_command::problem(&command) {
            return Err(failure(code));
        }
        let client = self.reveal_client()?;
        Ok(Box::pin(async move {
            assert!(run_command::problem(&command).is_none());
            let request = client.ide_run(project, command).await.map_err(ide_error)?;
            assert!(request.as_str().starts_with("run-"));
            Ok(())
        }))
    }

    /// Reads a project's "Run in project terminal" setting, run without the bridge lock.
    /// Input: the project id. Output: the mode, Type and run when none is saved.
    pub fn ide_run_mode(&self, project_id: String) -> Result<Unlocked<RunMode>, BridgeError> {
        let project = project(project_id)?;
        let client = self.reveal_client()?;
        Ok(Box::pin(async move {
            client.ide_run_mode(project).await.map_err(ide_error)
        }))
    }

    /// Saves a project's "Run in project terminal" setting, run without the bridge lock.
    /// Inputs: the project id and the mode. Output: the mode as saved.
    pub fn ide_set_run_mode(
        &self,
        project_id: String,
        mode: RunMode,
    ) -> Result<Unlocked<RunMode>, BridgeError> {
        let project = project(project_id)?;
        let client = self.reveal_client()?;
        Ok(Box::pin(async move {
            client
                .ide_set_run_mode(project, mode)
                .await
                .map_err(ide_error)
        }))
    }
}
