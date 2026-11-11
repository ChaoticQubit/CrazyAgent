use super::{BridgeError, NativeBridge, Unlocked, failure, uncaused};
use crazyagent::client_controller::OwnerController;
use crazyagent::coord::tool_switches::Switch;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::AccountSecret;
use crazyagent::plugins::{
    EffectivePlugins, HookView, Scope, ServerSpec, ServerView, SkillView, ToolView,
};
use crazyagent::protocol::ProjectId;

pub(super) fn plugin_failure(error: EngineError) -> BridgeError {
    uncaused(
        crazyagent::plugins::known_code(&error.0).unwrap_or("plugins_failed"),
        &error.0,
    )
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

    /// A copy of the owner controller on the same connection that holds no bridge state, so
    /// a long plugin call runs while other commands use the bridge. Output: the copy, or
    /// `not_connected`.
    fn detached(&self) -> Result<OwnerController, BridgeError> {
        self.plugins_ready()?;
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .reader()
            .map_err(|_| failure("not_connected"))
    }

    /// Lists skills for global settings (no project) or one project's settings.
    /// Output: each skill's name, description, scope, On flags and folders.
    pub async fn plugin_skills(
        &self,
        project_id: Option<String>,
    ) -> Result<Vec<SkillView>, BridgeError> {
        self.plugins_ready()?;
        let project = project(project_id)?;
        self.reader()?
            .plugin_skills(project)
            .await
            .map_err(plugin_failure)
    }

    /// Lists MCP servers for global settings or one project's settings.
    /// Output: how each runs, its plain status, saved secret names and tools.
    pub async fn plugin_servers(
        &self,
        project_id: Option<String>,
    ) -> Result<Vec<ServerView>, BridgeError> {
        self.plugins_ready()?;
        let project = project(project_id)?;
        self.reader()?
            .plugin_servers(project)
            .await
            .map_err(plugin_failure)
    }

    /// Lists hooks for global settings or one project's settings.
    /// Output: each hook's plugin, event, command, scope, On flags and the item it follows.
    pub async fn plugin_hooks(
        &self,
        project_id: Option<String>,
    ) -> Result<Vec<HookView>, BridgeError> {
        self.plugins_ready()?;
        let project = project(project_id)?;
        self.reader()?
            .plugin_hooks(project)
            .await
            .map_err(plugin_failure)
    }

    /// Adds the command hooks of an owner-picked plugin folder.
    /// Inputs: scope and the absolute folder. Output: the added hooks.
    pub async fn add_hooks(
        &mut self,
        scope: Scope,
        folder: String,
    ) -> Result<Vec<HookView>, BridgeError> {
        self.plugins_ready()?;
        self.controller()?
            .add_hooks(scope, folder)
            .await
            .map_err(plugin_failure)
    }

    /// Turns a hook on or off where it was added. Output: the hook.
    pub async fn set_hook_enabled(
        &mut self,
        id: String,
        enabled: bool,
    ) -> Result<HookView, BridgeError> {
        self.plugins_ready()?;
        self.controller()?
            .set_hook_enabled(id, enabled)
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

    /// Turns a skill on or off where it was added, when the screen showed its current
    /// version. Output: the skill, or `stale_revision` after another change.
    pub async fn set_skill_enabled(
        &mut self,
        id: String,
        enabled: bool,
        version: u64,
    ) -> Result<SkillView, BridgeError> {
        self.plugins_ready()?;
        self.controller()?
            .set_skill_enabled(id, enabled, version)
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

    /// Looks for skills in the owner's Claude Code and Codex skills folders, only when the
    /// owner opens Import my skills. Output: a call that holds no bridge state while the
    /// engine scans, giving each skill found with its state.
    pub fn skill_import_scan(
        &self,
    ) -> Result<Unlocked<crazyagent::skills::import::ImportList>, BridgeError> {
        let plugins = self.detached()?;
        Ok(Box::pin(async move {
            plugins.skill_import_scan().await.map_err(plugin_failure)
        }))
    }

    /// Imports one ticked skill. Inputs: its found folder and, for a skill whose files
    /// differ, the added skill's id to replace. Output: a call that holds no bridge state
    /// while the engine copies the skill, giving the skill and the paths left out.
    pub fn import_skill(
        &self,
        folder: String,
        replace: Option<String>,
    ) -> Result<Unlocked<crazyagent::skills::import::Imported>, BridgeError> {
        let plugins = self.detached()?;
        Ok(Box::pin(async move {
            let mut done = plugins
                .import_skills(vec![(folder, replace)])
                .await
                .map_err(plugin_failure)?;
            assert_eq!(done.len(), 1, "one pick gives one outcome");
            let (_, result) = done.remove(0);
            result.map_err(|code| plugin_failure(EngineError(code)))
        }))
    }

    /// Removes a skill and the engine's copy of its folder, when the screen showed the
    /// skill's current version.
    pub async fn remove_skill(&mut self, id: String, version: u64) -> Result<(), BridgeError> {
        self.plugins_ready()?;
        self.controller()?
            .remove_skill(id, version)
            .await
            .map_err(plugin_failure)
    }

    /// Adds an MCP server that waits for approval, with its secrets in the same request and
    /// each value in a private frame of its own. Inputs: scope, name, command or URL, and
    /// secrets by name. Output: a call that holds no bridge state while the engine saves
    /// them, giving the server.
    pub fn add_mcp_server(
        &self,
        scope: Scope,
        name: String,
        spec: ServerSpec,
        secrets: Vec<(String, String)>,
    ) -> Result<Unlocked<ServerView>, BridgeError> {
        let plugins = self.detached()?;
        let mut values = Vec::with_capacity(secrets.len());
        for (secret, value) in secrets {
            let value = AccountSecret::new(value.into_bytes())
                .map_err(|_| failure("server_secret_invalid"))?;
            values.push((secret, value));
        }
        Ok(Box::pin(async move {
            plugins
                .add_mcp_server(scope, name, spec, values)
                .await
                .map_err(plugin_failure)
        }))
    }

    /// Changes how an MCP server runs; it then waits for approval again. Inputs: id, the new
    /// command or URL and the version the screen showed.
    pub async fn update_mcp_server(
        &mut self,
        id: String,
        spec: ServerSpec,
        version: u64,
    ) -> Result<ServerView, BridgeError> {
        self.plugins_ready()?;
        self.controller()?
            .update_mcp_server(id, spec, version)
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

    /// Turns an MCP server on or off where it was added, when the screen showed its current
    /// version.
    pub async fn set_mcp_server_enabled(
        &mut self,
        id: String,
        enabled: bool,
        version: u64,
    ) -> Result<ServerView, BridgeError> {
        self.plugins_ready()?;
        self.controller()?
            .set_mcp_server_enabled(id, enabled, version)
            .await
            .map_err(plugin_failure)
    }

    /// Removes an MCP server and its saved secrets, when the screen showed the server's
    /// current version.
    pub async fn remove_mcp_server(&mut self, id: String, version: u64) -> Result<(), BridgeError> {
        self.plugins_ready()?;
        self.controller()?
            .remove_mcp_server(id, version)
            .await
            .map_err(plugin_failure)
    }

    /// Switches a global skill or MCP server on or off for one project, when the screen
    /// showed the item's current version.
    pub async fn switch_plugin(
        &mut self,
        project_id: String,
        id: String,
        on: bool,
        version: u64,
    ) -> Result<(), BridgeError> {
        self.plugins_ready()?;
        let project =
            project(Some(project_id))?.ok_or_else(|| failure("plugin_project_unknown"))?;
        self.controller()?
            .switch_plugin(project, id, on, version)
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

    /// Reads one server's tools with their switches: the CrazyAgent built-in tools (no server
    /// id) or one MCP server's checked tools, for every project (no project id) or one.
    pub async fn tool_switches(
        &mut self,
        project_id: Option<String>,
        server_id: Option<String>,
    ) -> Result<Vec<ToolView>, BridgeError> {
        self.plugins_ready()?;
        let project = project(project_id)?;
        self.controller()?
            .tool_switches(project, server_id)
            .await
            .map_err(plugin_failure)
    }

    /// Sets one tool's switch for every project or one project; Hidden deletes a built-in
    /// tool until its defaults are restored. Output: the server's tools after the change.
    pub async fn set_tool_switch(
        &mut self,
        project_id: Option<String>,
        server_id: Option<String>,
        tool: String,
        value: Switch,
    ) -> Result<Vec<ToolView>, BridgeError> {
        self.plugins_ready()?;
        let project = project(project_id)?;
        self.controller()?
            .set_tool_switch(project, server_id, tool, value)
            .await
            .map_err(plugin_failure)
    }

    /// Clears one server's stored switches for every project or one project.
    /// Output: the server's tools.
    pub async fn restore_tool_switches(
        &mut self,
        project_id: Option<String>,
        server_id: Option<String>,
    ) -> Result<Vec<ToolView>, BridgeError> {
        self.plugins_ready()?;
        let project = project(project_id)?;
        self.controller()?
            .restore_tool_switches(project, server_id)
            .await
            .map_err(plugin_failure)
    }
}
