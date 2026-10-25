use super::*;
use crate::agents::{AgentView, LIST_MAX};
use crate::engine::owner::{AGENTS_PAGE, AgentPage};
use crate::protocol::ProjectId;

const PAGES_MAX: usize = LIST_MAX.div_ceil(AGENTS_PAGE);

impl OwnerController {
    fn agents_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_ref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads one page of launched agents. Inputs: a project, or none for every project,
    /// and the previous page's `next`. Output: agents with plain status and controls.
    pub async fn agents_page(
        &self,
        project: Option<ProjectId>,
        offset: u16,
    ) -> Result<AgentPage, EngineError> {
        self.agents_client()?.agents(project, offset).await
    }

    /// Reads every launched agent, newest first, up to `LIST_MAX`.
    /// Input: a project, or none for every project. Output: the agents.
    pub async fn agents(&self, project: Option<ProjectId>) -> Result<Vec<AgentView>, EngineError> {
        let mut agents = Vec::new();
        let mut offset = 0;
        for _ in 0..PAGES_MAX {
            let page = self.agents_page(project.clone(), offset).await?;
            agents.extend(page.agents);
            match page.next {
                Some(next) if next > offset && agents.len() < LIST_MAX => offset = next,
                _ => break,
            }
        }
        agents.truncate(LIST_MAX);
        Ok(agents)
    }

    /// Stops one agent and its whole process group, leaving every other agent running.
    /// Input: the agent id. Output: the stopped agent.
    pub async fn stop_agent(&self, id: String) -> Result<AgentView, EngineError> {
        self.agents_client()?.stop_agent(id).await
    }

    /// Interrupts the current turn of one agent, which keeps running. Input: the agent id.
    pub async fn interrupt_agent(&self, id: String) -> Result<(), EngineError> {
        self.agents_client()?.interrupt_agent(id).await
    }

    /// Restarts one agent with its first task. Input: the agent id. Output: the agent.
    pub async fn restart_agent(&self, id: String) -> Result<AgentView, EngineError> {
        self.agents_client()?.restart_agent(id).await
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::agents::AgentState;
    use crate::agents::tests::{alive, install_fixture, pids, spec, until};
    use crate::engine::bootstrap::EngineService;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn agent_controls_reach_the_engine_and_touch_only_one_agent() {
        let root = tempfile::Builder::new()
            .prefix("ca-cagent-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let home = root.path().canonicalize().unwrap();
        install_fixture(&home);
        let global = home.join("global.toml");
        std::fs::write(&global, "version = 1\ntimeout_ms = 5000\n").unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            &home,
            "e.sock",
            &global,
            &home.join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let agents = service.owner_runtime().unwrap().agents().unwrap();
        let first = agents
            .launch(spec(&home, "first", Some("echo one")), None)
            .unwrap();
        let second = agents.launch(spec(&home, "second", None), None).unwrap();
        let (first_pid, first_grandchild) = pids(&home, "first");
        let (second_pid, second_grandchild) = pids(&home, "second");
        let (_, controller) = OwnerController::attach_configured(
            Client::new(home.join("e.sock"), Duration::from_secs(10)).unwrap(),
            OwnerClientKind::Desktop,
        )
        .await
        .unwrap();

        let listed = controller.agents(None).await.unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().all(|agent| agent.state == AgentState::Running
            && agent.status != "Stopped when CrazyAgent closed"));

        let stopped = controller.stop_agent(first.id.clone()).await.unwrap();
        assert_eq!(stopped.state, AgentState::Stopped);
        assert!(stopped.can_restart && !stopped.can_stop);
        until("the stopped group to be gone", || {
            !alive(first_pid) && !alive(first_grandchild)
        });
        assert!(alive(second_pid) && alive(second_grandchild));

        assert_eq!(
            controller.interrupt_agent(second.id.clone()).await,
            Err(EngineError("agent_not_busy".into()))
        );
        assert_eq!(
            controller.stop_agent("0".repeat(32)).await,
            Err(EngineError("agent_not_running".into()))
        );

        std::fs::remove_file(home.join("work").join("first").join("pids.json")).unwrap();
        let restarted = controller.restart_agent(first.id.clone()).await.unwrap();
        assert_eq!(restarted.id, first.id);
        assert_eq!(restarted.state, AgentState::Running);
        assert_eq!(restarted.attempt, 2);
        let (restarted_pid, _) = pids(&home, "first");
        assert_ne!(restarted_pid, first_pid);
        assert!(alive(second_pid));

        agents.stop(&first.id).unwrap();
        agents.stop(&second.id).unwrap();
        until("every fixture to be gone", || {
            !alive(restarted_pid) && !alive(second_pid) && !alive(second_grandchild)
        });
    }
}
