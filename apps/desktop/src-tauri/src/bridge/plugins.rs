use super::{BridgeError, NativeBridge, Unlocked, failure};
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::AccountSecret;
use crazyagent::plugins::{EffectivePlugins, Scope, ServerSpec, ServerView, SkillView};
use crazyagent::protocol::ProjectId;

fn plugin_failure(error: EngineError) -> BridgeError {
    failure(crazyagent::plugins::known_code(&error.0).unwrap_or("plugins_failed"))
}

fn project(id: Option<String>) -> Result<Option<ProjectId>, BridgeError> {
    id.map(|id| ProjectId::new(id).map_err(|_| failure("plugin_project_unknown")))
        .transpose()
}

impl NativeBridge {
    fn plugins_ready(&self) -> Result<(), BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        Ok(())
    }

    /// Lists skills for global settings (no project) or one project's settings.
    /// Output: each skill's name, description, scope, On flags and folders.
    pub async fn plugin_skills(
        &mut self,
        project_id: Option<String>,
    ) -> Result<Vec<SkillView>, BridgeError> {
        self.plugins_ready()?;
        let project = project(project_id)?;
        self.controller()?
            .plugin_skills(project)
            .await
            .map_err(plugin_failure)
    }

    /// Lists MCP servers for global settings or one project's settings.
    /// Output: how each runs, its plain status, saved secret names and tools.
    pub async fn plugin_servers(
        &mut self,
        project_id: Option<String>,
    ) -> Result<Vec<ServerView>, BridgeError> {
        self.plugins_ready()?;
        let project = project(project_id)?;
        self.controller()?
            .plugin_servers(project)
            .await
            .map_err(plugin_failure)
    }

    /// Adds a skill from an owner-picked folder that holds SKILL.md.
    /// Inputs: scope and the absolute folder. Output: the stored skill.
    pub async fn add_skill(
        &mut self,
        scope: Scope,
        folder: String,
    ) -> Result<SkillView, BridgeError> {
        self.plugins_ready()?;
        self.controller()?
            .add_skill(scope, folder)
            .await
            .map_err(plugin_failure)
    }

    /// Turns a skill on or off where it was added. Output: the skill.
    pub async fn set_skill_enabled(
        &mut self,
        id: String,
        enabled: bool,
    ) -> Result<SkillView, BridgeError> {
        self.plugins_ready()?;
        self.controller()?
            .set_skill_enabled(id, enabled)
            .await
            .map_err(plugin_failure)
    }

    /// Reads one skill's detail: the skill with the project's switch when a project is
    /// given, its full SKILL.md text and its files. Inputs: skill id and optional project id.
    pub async fn skill_detail(
        &mut self,
        id: String,
        project_id: Option<String>,
    ) -> Result<crazyagent::skills::SkillDetail, BridgeError> {
        self.plugins_ready()?;
        let project = project(project_id)?;
        self.controller()?
            .skill_detail(id, project)
            .await
            .map_err(plugin_failure)
    }

    /// Removes a skill and the engine's copy of its folder.
    pub async fn remove_skill(&mut self, id: String) -> Result<(), BridgeError> {
        self.plugins_ready()?;
        self.controller()?
            .remove_skill(id)
            .await
            .map_err(plugin_failure)
    }

    /// Adds an MCP server that waits for approval, then saves each secret through its own
    /// private frame. Inputs: scope, name, command or URL, and secrets by name.
    pub async fn add_mcp_server(
        &mut self,
        scope: Scope,
        name: String,
        spec: ServerSpec,
        secrets: Vec<(String, String)>,
    ) -> Result<ServerView, BridgeError> {
        self.plugins_ready()?;
        let mut values = Vec::with_capacity(secrets.len());
        for (secret, value) in secrets {
            let value = AccountSecret::new(value.into_bytes())
                .map_err(|_| failure("server_secret_invalid"))?;
            values.push((secret, value));
        }
        self.controller()?
            .add_mcp_server(scope, name, spec, values)
            .await
            .map_err(plugin_failure)
    }

    /// Changes how an MCP server runs; it then waits for approval again.
    pub async fn update_mcp_server(
        &mut self,
        id: String,
        spec: ServerSpec,
    ) -> Result<ServerView, BridgeError> {
        self.plugins_ready()?;
        self.controller()?
            .update_mcp_server(id, spec)
            .await
            .map_err(plugin_failure)
    }

    /// Approves an MCP server exactly as shown. Inputs: id and the shown command or URL.
    pub async fn approve_mcp_server(
        &mut self,
        id: String,
        spec: ServerSpec,
    ) -> Result<ServerView, BridgeError> {
        self.plugins_ready()?;
        self.controller()?
            .approve_mcp_server(id, spec)
            .await
            .map_err(plugin_failure)
    }

    /// Turns an MCP server on or off where it was added.
    pub async fn set_mcp_server_enabled(
        &mut self,
        id: String,
        enabled: bool,
    ) -> Result<ServerView, BridgeError> {
        self.plugins_ready()?;
        self.controller()?
            .set_mcp_server_enabled(id, enabled)
            .await
            .map_err(plugin_failure)
    }

    /// Removes an MCP server and its saved secrets.
    pub async fn remove_mcp_server(&mut self, id: String) -> Result<(), BridgeError> {
        self.plugins_ready()?;
        self.controller()?
            .remove_mcp_server(id)
            .await
            .map_err(plugin_failure)
    }

    /// Switches a global skill or MCP server on or off for one project.
    pub async fn switch_plugin(
        &mut self,
        project_id: String,
        id: String,
        on: bool,
    ) -> Result<(), BridgeError> {
        self.plugins_ready()?;
        let project =
            project(Some(project_id))?.ok_or_else(|| failure("plugin_project_unknown"))?;
        self.controller()?
            .switch_plugin(project, id, on)
            .await
            .map_err(plugin_failure)
    }

    /// Saves or replaces one MCP server secret. Inputs: server id, name and value.
    pub async fn set_mcp_secret(
        &mut self,
        id: String,
        name: String,
        value: String,
    ) -> Result<ServerView, BridgeError> {
        self.plugins_ready()?;
        let value =
            AccountSecret::new(value.into_bytes()).map_err(|_| failure("server_secret_invalid"))?;
        self.controller()?
            .set_mcp_secret(id, name, value)
            .await
            .map_err(plugin_failure)
    }

    /// Deletes one MCP server secret. Inputs: server id and the secret's name.
    pub async fn remove_mcp_secret(
        &mut self,
        id: String,
        name: String,
    ) -> Result<ServerView, BridgeError> {
        self.plugins_ready()?;
        self.controller()?
            .remove_mcp_secret(id, name)
            .await
            .map_err(plugin_failure)
    }

    /// Starts a command MCP server, lists its tools and stops it.
    /// Output: the server with its tools or the plain reason it can't start.
    pub fn check_mcp_server(&mut self, id: String) -> Result<Unlocked<ServerView>, BridgeError> {
        self.plugins_ready()?;
        let client = self.controller()?.shared_client().map_err(plugin_failure)?;
        Ok(Box::pin(async move {
            client.check_mcp_server(id).await.map_err(plugin_failure)
        }))
    }

    /// Reads what agents launched for a project receive: skill folders and approved servers.
    pub async fn project_plugins(
        &mut self,
        project_id: String,
    ) -> Result<EffectivePlugins, BridgeError> {
        self.plugins_ready()?;
        let project =
            project(Some(project_id))?.ok_or_else(|| failure("plugin_project_unknown"))?;
        self.controller()?
            .project_plugins(project)
            .await
            .map_err(plugin_failure)
    }
}
