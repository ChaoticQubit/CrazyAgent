use super::*;
use crate::engine::owner::{AccountSecret, OwnerClient};
use crate::plugins::{EffectivePlugins, Scope, ServerSpec, ServerView, SkillView};
use crate::protocol::ProjectId;

const PAGES_MAX: usize = 64;

impl OwnerController {
    fn plugins_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_ref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Lists every skill for global settings (no project) or for one project's settings,
    /// following the engine's pages. Output: skills with scope and On flags.
    pub async fn plugin_skills(
        &self,
        project: Option<ProjectId>,
    ) -> Result<Vec<SkillView>, EngineError> {
        let client = self.plugins_client()?;
        let mut skills = Vec::new();
        let mut offset = 0;
        for _ in 0..PAGES_MAX {
            let page = client.plugin_skills(project.clone(), offset).await?;
            skills.extend(page.skills);
            match page.next {
                Some(next) if next > offset => offset = next,
                Some(_) => return Err(EngineError("plugins_page_invalid".into())),
                None => return Ok(skills),
            }
        }
        Err(EngineError("plugins_page_invalid".into()))
    }

    /// Lists every MCP server for global settings or one project's settings.
    /// Output: servers with how they run, plain status, saved secret names and tools.
    pub async fn plugin_servers(
        &self,
        project: Option<ProjectId>,
    ) -> Result<Vec<ServerView>, EngineError> {
        let client = self.plugins_client()?;
        let mut servers = Vec::new();
        let mut offset = 0;
        for _ in 0..PAGES_MAX {
            let page = client.plugin_servers(project.clone(), offset).await?;
            servers.extend(page.servers);
            match page.next {
                Some(next) if next > offset => offset = next,
                Some(_) => return Err(EngineError("plugins_page_invalid".into())),
                None => return Ok(servers),
            }
        }
        Err(EngineError("plugins_page_invalid".into()))
    }

    /// Adds a skill from a folder the owner picked. Inputs: scope and the absolute folder.
    pub async fn add_skill(&self, scope: Scope, folder: String) -> Result<SkillView, EngineError> {
        self.plugins_client()?.add_skill(scope, folder).await
    }

    /// Turns a skill on or off where it was added. Inputs: skill id and the new state.
    pub async fn set_skill_enabled(
        &self,
        id: String,
        enabled: bool,
    ) -> Result<SkillView, EngineError> {
        self.plugins_client()?.set_skill_enabled(id, enabled).await
    }

    /// Removes a skill and the engine's copy of its folder. Input: the skill id.
    pub async fn remove_skill(&self, id: String) -> Result<(), EngineError> {
        self.plugins_client()?.remove_skill(id).await
    }

    /// Adds an MCP server, then saves each secret through its own private frame.
    /// Inputs: scope, name, command or URL, and secrets by name. Output: the server,
    /// waiting for approval, with its secrets shown as saved.
    pub async fn add_mcp_server(
        &self,
        scope: Scope,
        name: String,
        spec: ServerSpec,
        secrets: Vec<(String, AccountSecret)>,
    ) -> Result<ServerView, EngineError> {
        for (secret, _) in &secrets {
            crate::plugins::validate_secret_name(secret)?;
        }
        if secrets.len() > crate::plugins::SECRETS_PER_SERVER_MAX {
            return Err(EngineError("server_secret_limit".into()));
        }
        let client = self.plugins_client()?;
        let mut server = client.add_mcp_server(scope, name, spec).await?;
        for (secret, value) in secrets {
            server = client
                .set_mcp_secret(server.id.clone(), secret, value)
                .await?;
        }
        Ok(server)
    }

    /// Changes an MCP server's command or URL; it then waits for approval again.
    pub async fn update_mcp_server(
        &self,
        id: String,
        spec: ServerSpec,
    ) -> Result<ServerView, EngineError> {
        self.plugins_client()?.update_mcp_server(id, spec).await
    }

    /// Approves an MCP server exactly as shown to the owner. Inputs: id and the shown spec.
    pub async fn approve_mcp_server(
        &self,
        id: String,
        spec: ServerSpec,
    ) -> Result<ServerView, EngineError> {
        self.plugins_client()?.approve_mcp_server(id, spec).await
    }

    /// Turns an MCP server on or off where it was added. Inputs: id and the new state.
    pub async fn set_mcp_server_enabled(
        &self,
        id: String,
        enabled: bool,
    ) -> Result<ServerView, EngineError> {
        self.plugins_client()?
            .set_mcp_server_enabled(id, enabled)
            .await
    }

    /// Removes an MCP server and its saved secrets. Input: the server id.
    pub async fn remove_mcp_server(&self, id: String) -> Result<(), EngineError> {
        self.plugins_client()?.remove_mcp_server(id).await
    }

    /// Switches a global skill or MCP server on or off for one project.
    pub async fn switch_plugin(
        &self,
        project: ProjectId,
        id: String,
        on: bool,
    ) -> Result<(), EngineError> {
        self.plugins_client()?.switch_plugin(project, id, on).await
    }

    /// Saves or replaces one MCP server secret. Inputs: server id, name and value.
    pub async fn set_mcp_secret(
        &self,
        id: String,
        name: String,
        value: AccountSecret,
    ) -> Result<ServerView, EngineError> {
        self.plugins_client()?.set_mcp_secret(id, name, value).await
    }

    /// Deletes one MCP server secret. Inputs: server id and the secret's name.
    pub async fn remove_mcp_secret(
        &self,
        id: String,
        name: String,
    ) -> Result<ServerView, EngineError> {
        self.plugins_client()?.remove_mcp_secret(id, name).await
    }

    /// Starts a command MCP server, lists its tools and stops it. Input: the server id.
    /// Output: the server with its tools or the plain reason it can't start.
    pub async fn check_mcp_server(&self, id: String) -> Result<ServerView, EngineError> {
        self.plugins_client()?.check_mcp_server(id).await
    }

    /// Reads what agents launched for a project receive. Input: the project.
    pub async fn project_plugins(
        &self,
        project: ProjectId,
    ) -> Result<EffectivePlugins, EngineError> {
        self.plugins_client()?.project_plugins(project).await
    }
}
