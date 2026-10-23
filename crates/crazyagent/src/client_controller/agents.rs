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
