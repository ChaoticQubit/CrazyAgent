use super::*;
use crate::coord::tool_switches::Switch;
use crate::engine::owner::{AccountSecret, OwnerClient};
use crate::plugins::{
    EffectivePlugins, HookView, Scope, ServerSpec, ServerView, SkillView, ToolView,
};
use crate::protocol::ProjectId;

const PAGES_MAX: usize = 64;
const DETAIL_PAGES_MAX: usize = 320;

impl OwnerController {
    fn plugins_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_deref()
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

    /// Lists every hook for global settings (no project) or for one project's settings,
    /// following the engine's pages. Output: hooks with scope, On flags and the item each
    /// follows.
    pub async fn plugin_hooks(
        &self,
        project: Option<ProjectId>,
    ) -> Result<Vec<HookView>, EngineError> {
        let client = self.plugins_client()?;
        let mut hooks = Vec::new();
        let mut offset = 0;
        for _ in 0..PAGES_MAX {
            let page = client.plugin_hooks(project.clone(), offset).await?;
            hooks.extend(page.hooks);
            match page.next {
                Some(next) if next > offset => offset = next,
                Some(_) => return Err(EngineError("plugins_page_invalid".into())),
                None => return Ok(hooks),
            }
        }
        Err(EngineError("plugins_page_invalid".into()))
    }

    /// Adds the command hooks a plugin folder defines. Inputs: global or project scope and
    /// the absolute folder path. Output: the added hooks, each turned on.
    pub async fn add_hooks(
        &self,
        scope: Scope,
        folder: String,
    ) -> Result<Vec<HookView>, EngineError> {
        Ok(self.plugins_client()?.add_hooks(scope, folder).await?.hooks)
    }

    /// Turns a hook on or off where it was added. Inputs: the hook id and the new state.
    /// Output: the hook.
    pub async fn set_hook_enabled(
        &self,
        id: String,
        enabled: bool,
    ) -> Result<HookView, EngineError> {
        self.plugins_client()?.set_hook_enabled(id, enabled).await
    }

    /// Reads one skill's whole detail for its detail view, following the engine's pages of
    /// SKILL.md. Inputs: the skill id and the project whose switch to show, or none for
    /// global settings. Output: the skill, its full SKILL.md text and its files, or
    /// `skill_page_invalid` when the pages do not join up.
    pub async fn skill_detail(
        &self,
        id: String,
        project: Option<ProjectId>,
    ) -> Result<crate::skills::SkillDetail, EngineError> {
        let client = self.plugins_client()?;
        let invalid = || EngineError("skill_page_invalid".into());
        let first = client.skill_page(id.clone(), project.clone(), 0).await?;
        let mut next = first.next;
        let mut detail = crate::skills::SkillDetail {
            skill: first.skill,
            text: first.text,
            files: first.files,
            files_more: first.files_more,
        };
        for _ in 0..DETAIL_PAGES_MAX {
            let Some(offset) = next else {
                assert!(detail.text.len() <= crate::skills::TEXT_BYTES_MAX);
                return Ok(detail);
            };
            if offset as usize != detail.text.len() {
                return Err(invalid());
            }
            let page = client
                .skill_page(id.clone(), project.clone(), offset)
                .await?;
            if page.skill.id != detail.skill.id
                || page.text.is_empty()
                || !page.files.is_empty()
                || detail.text.len() + page.text.len() > crate::skills::TEXT_BYTES_MAX
            {
                return Err(invalid());
            }
            detail.text.push_str(&page.text);
            next = page.next;
        }
        Err(invalid())
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

    /// Looks for skills in the owner's Claude Code and Codex skills folders, only when asked.
    /// Output: each skill found with its state against the app's skills.
    pub async fn skill_import_scan(
        &self,
    ) -> Result<crate::skills::import::ImportList, EngineError> {
        self.plugins_client()?.skill_import_scan().await
    }

    /// Imports the skills the owner ticked, one at a time, so one failure leaves the rest.
    /// Input: each ticked folder with the id to replace for a skill whose files differ.
    /// Output: each folder with its imported skill or the plain problem code, in order.
    pub async fn import_skills(
        &self,
        picks: Vec<(String, Option<String>)>,
    ) -> Result<Vec<(String, Result<crate::skills::import::Imported, String>)>, EngineError> {
        let client = self.plugins_client()?;
        let mut done = Vec::with_capacity(picks.len());
        for (folder, replace) in picks {
            let result = client
                .import_skill(folder.clone(), replace)
                .await
                .map_err(|problem| problem.0);
            done.push((folder, result));
        }
        Ok(done)
    }

    /// Turns a skill on or off where it was added. Inputs: skill id, the new state and the
    /// version the screen showed. Output: the skill, or `stale_revision` after another change.
    pub async fn set_skill_enabled(
        &self,
        id: String,
        enabled: bool,
        version: u64,
    ) -> Result<SkillView, EngineError> {
        self.plugins_client()?
            .set_skill_enabled(id, enabled, version)
            .await
    }

    /// Removes a skill and the engine's copy of its folder. Inputs: the skill id and the
    /// version the screen showed. Output: done, or `stale_revision` after another change.
    pub async fn remove_skill(&self, id: String, version: u64) -> Result<(), EngineError> {
        self.plugins_client()?.remove_skill(id, version).await
    }

    /// Adds an MCP server with its secrets in one request, so the engine keeps the server
    /// with every secret or keeps none of them. Inputs: scope, name, command or URL, and
    /// secrets by name. Output: the server, waiting for approval, with its secrets shown
    /// as saved.
    pub async fn add_mcp_server(
        &self,
        scope: Scope,
        name: String,
        spec: ServerSpec,
        secrets: Vec<(String, AccountSecret)>,
    ) -> Result<ServerView, EngineError> {
        self.plugins_client()?
            .add_mcp_server(scope, name, spec, secrets)
            .await
    }

    /// Changes an MCP server's command or URL; it then waits for approval again.
    /// Inputs: id, the new spec and the version the screen showed.
    pub async fn update_mcp_server(
        &self,
        id: String,
        spec: ServerSpec,
        version: u64,
    ) -> Result<ServerView, EngineError> {
        self.plugins_client()?
            .update_mcp_server(id, spec, version)
            .await
    }

    /// Approves an MCP server exactly as shown to the owner. Inputs: id and the shown spec.
    pub async fn approve_mcp_server(
        &self,
        id: String,
        spec: ServerSpec,
    ) -> Result<ServerView, EngineError> {
        self.plugins_client()?.approve_mcp_server(id, spec).await
    }

    /// Turns an MCP server on or off where it was added. Inputs: id, the new state and the
    /// version the screen showed. Output: the server, or `stale_revision` after another change.
    pub async fn set_mcp_server_enabled(
        &self,
        id: String,
        enabled: bool,
        version: u64,
    ) -> Result<ServerView, EngineError> {
        self.plugins_client()?
            .set_mcp_server_enabled(id, enabled, version)
            .await
    }

    /// Removes an MCP server and its saved secrets. Inputs: the server id and the version
    /// the screen showed. Output: done, or `stale_revision` after another change.
    pub async fn remove_mcp_server(&self, id: String, version: u64) -> Result<(), EngineError> {
        self.plugins_client()?.remove_mcp_server(id, version).await
    }

    /// Switches a global skill or MCP server on or off for one project. Inputs: the project,
    /// the item id, the new state and the item's version the screen showed.
    pub async fn switch_plugin(
        &self,
        project: ProjectId,
        id: String,
        on: bool,
        version: u64,
    ) -> Result<(), EngineError> {
        self.plugins_client()?
            .switch_plugin(project, id, on, version)
            .await
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

    /// Reads one server's tools with their switches. Inputs: project (none for every
    /// project) and server id (none for the CrazyAgent built-in tools). Output: the tools.
    pub async fn tool_switches(
        &self,
        project: Option<ProjectId>,
        server: Option<String>,
    ) -> Result<Vec<ToolView>, EngineError> {
        self.plugins_client()?.tool_switches(project, server).await
    }

    /// Sets one tool's switch in that scope. Inputs: project, server id, tool and switch.
    /// Output: the server's tools after the change.
    pub async fn set_tool_switch(
        &self,
        project: Option<ProjectId>,
        server: Option<String>,
        tool: String,
        switch: Switch,
    ) -> Result<Vec<ToolView>, EngineError> {
        self.plugins_client()?
            .set_tool_switch(project, server, tool, switch)
            .await
    }

    /// Clears one server's stored switches in that scope. Inputs: project and server id.
    /// Output: the server's tools.
    pub async fn restore_tool_switches(
        &self,
        project: Option<ProjectId>,
        server: Option<String>,
    ) -> Result<Vec<ToolView>, EngineError> {
        self.plugins_client()?
            .restore_tool_switches(project, server)
            .await
    }
}
