use super::*;
use crate::coord::tool_switches::Switch;
use crate::engine::owner::AccountSecret;
use crate::plugins::{
    EffectivePlugins, HookView, Scope, ServerSpec, ServerView, SkillView, ToolView, check,
};
use crate::protocol::encode_frame;
use crate::storage::plugins::{Command, ResultValue};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};

const SECRET_MAX: usize = 8192;
const CHECKS_MAX: usize = 4;
const CHECK_POLL: Duration = Duration::from_millis(200);
pub(super) const CHECK_FOLDER: &str = crate::storage::plugins::CHECK_FOLDER;
pub(super) const CHECK_PROFILES: &str = "profiles";
pub(super) const IMPORT_TIMEOUT: Duration = Duration::from_secs(60);
const CATALOG_PAGES_MAX: usize = 64;
static CHECKS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Skills {
        project: Option<ProjectId>,
        offset: u16,
    },
    SkillDetail {
        id: String,
        project: Option<ProjectId>,
        offset: u32,
    },
    Servers {
        project: Option<ProjectId>,
        offset: u16,
    },
    Server {
        id: String,
    },
    AddSkill {
        scope: Scope,
        folder: String,
    },
    ImportScan,
    ImportSkill {
        folder: String,
        replace: Option<String>,
    },
    SetSkill {
        id: String,
        enabled: bool,
        version: u64,
    },
    RemoveSkill {
        id: String,
        version: u64,
    },
    Hooks {
        project: Option<ProjectId>,
        offset: u16,
    },
    AddHooks {
        scope: Scope,
        folder: String,
    },
    SetHook {
        id: String,
        enabled: bool,
    },
    AddServer {
        scope: Scope,
        name: String,
        spec: ServerSpec,
        #[serde(default)]
        secrets: Vec<String>,
    },
    UpdateServer {
        id: String,
        spec: ServerSpec,
        version: u64,
    },
    Approve {
        id: String,
        spec: ServerSpec,
    },
    SetServer {
        id: String,
        enabled: bool,
        version: u64,
    },
    RemoveServer {
        id: String,
        version: u64,
    },
    Switch {
        project: ProjectId,
        id: String,
        on: bool,
        version: u64,
    },
    SetSecret {
        id: String,
        name: String,
    },
    RemoveSecret {
        id: String,
        name: String,
    },
    Check {
        id: String,
    },
    Effective {
        project: ProjectId,
    },
    Tools {
        project: Option<ProjectId>,
        server: Option<String>,
    },
    SetTool {
        project: Option<ProjectId>,
        server: Option<String>,
        tool: String,
        switch: Switch,
    },
    RestoreTools {
        project: Option<ProjectId>,
        server: Option<String>,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    plugins: Action,
    secret_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillPage {
    pub skills: Vec<SkillView>,
    pub next: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerPage {
    pub servers: Vec<ServerView>,
    pub next: Option<u16>,
}

/// One page of hooks as Settings lists them, with the offset of the next page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookPage {
    pub hooks: Vec<HookView>,
    pub next: Option<u16>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Skills {
        page: SkillPage,
    },
    SkillDetail {
        page: crate::skills::SkillPage,
    },
    Servers {
        page: ServerPage,
    },
    Hooks {
        page: HookPage,
    },
    Hook {
        hook: HookView,
    },
    Skill {
        skill: SkillView,
    },
    ImportScan {
        list: crate::skills::import::ImportList,
    },
    Imported {
        imported: crate::skills::import::Imported,
    },
    Server {
        server: ServerView,
    },
    Done,
    Effective {
        plugins: EffectivePlugins,
    },
    Tools {
        tools: Vec<ToolView>,
    },
    Rejected {
        code: String,
    },
}

/// Names the requests that only read, for the log of replies this client could not read.
/// Input: the request. Output: its name, None for a request that changes something.
fn read_kind(action: &Action) -> Option<&'static str> {
    match action {
        Action::Skills { .. } => Some("plugins.skills"),
        Action::SkillDetail { .. } => Some("plugins.skill_detail"),
        Action::Servers { .. } => Some("plugins.servers"),
        Action::Server { .. } => Some("plugins.server"),
        Action::ImportScan => Some("plugins.import_scan"),
        Action::Hooks { .. } => Some("plugins.hooks"),
        Action::Effective { .. } => Some("plugins.effective"),
        Action::Tools { .. } => Some("plugins.tools"),
        _ => None,
    }
}

fn unknown() -> EngineError {
    failure("plugins_outcome_unknown")
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn valid_folder(folder: &str) -> bool {
    folder.starts_with('/')
        && folder.len() <= crate::plugins::skill_folder::SOURCE_BYTES_MAX
        && !folder.chars().any(char::is_control)
}

impl Action {
    fn validate(&self) -> Result<(), EngineError> {
        let id = match self {
            Self::Server { id }
            | Self::SkillDetail { id, .. }
            | Self::SetSkill { id, .. }
            | Self::RemoveSkill { id, .. }
            | Self::SetHook { id, .. }
            | Self::UpdateServer { id, .. }
            | Self::Approve { id, .. }
            | Self::SetServer { id, .. }
            | Self::RemoveServer { id, .. }
            | Self::Switch { id, .. }
            | Self::SetSecret { id, .. }
            | Self::RemoveSecret { id, .. }
            | Self::Check { id } => Some(id),
            _ => None,
        };
        if id.is_some_and(|id| !valid_id(id)) {
            return Err(failure("plugin_not_found"));
        }
        if let Self::Tools {
            server: Some(id), ..
        }
        | Self::SetTool {
            server: Some(id), ..
        }
        | Self::RestoreTools {
            server: Some(id), ..
        } = self
            && !valid_id(id)
        {
            return Err(failure("server_not_found"));
        }
        match self {
            Self::SetTool { tool, .. }
                if tool.is_empty()
                    || tool.len() > crate::plugins::TOOL_NAME_BYTES_MAX
                    || tool.chars().any(char::is_control) =>
            {
                Err(failure("tool_unknown"))
            }
            Self::AddSkill { folder, .. }
            | Self::ImportSkill { folder, .. }
            | Self::AddHooks { folder, .. }
                if !valid_folder(folder) =>
            {
                Err(failure("skill_folder_not_found"))
            }
            Self::ImportSkill {
                replace: Some(id), ..
            } if !valid_id(id) => Err(failure("plugin_not_found")),
            Self::AddServer {
                name,
                spec,
                secrets,
                ..
            } => {
                crate::plugins::validate_server_name(name)?;
                spec.validate()?;
                let names: Vec<&str> = secrets.iter().map(String::as_str).collect();
                crate::plugins::validate_secret_names(&names)
            }
            Self::UpdateServer { spec, .. } | Self::Approve { spec, .. } => spec.validate(),
            Self::SetSecret { name, .. } | Self::RemoveSecret { name, .. } => {
                crate::plugins::validate_secret_name(name)
            }
            _ => Ok(()),
        }
    }

    /// Whether this request changes plugin state or starts work, and so runs under the
    /// owner barrier. Output: true for a change, false for a read.
    fn changes(&self) -> bool {
        match self {
            Self::Skills { .. }
            | Self::SkillDetail { .. }
            | Self::Servers { .. }
            | Self::Server { .. }
            | Self::ImportScan
            | Self::Hooks { .. }
            | Self::Effective { .. }
            | Self::Tools { .. } => false,
            Self::AddSkill { .. }
            | Self::ImportSkill { .. }
            | Self::SetSkill { .. }
            | Self::RemoveSkill { .. }
            | Self::AddHooks { .. }
            | Self::SetHook { .. }
            | Self::AddServer { .. }
            | Self::UpdateServer { .. }
            | Self::Approve { .. }
            | Self::SetServer { .. }
            | Self::RemoveServer { .. }
            | Self::Switch { .. }
            | Self::SetSecret { .. }
            | Self::RemoveSecret { .. }
            | Self::Check { .. }
            | Self::SetTool { .. }
            | Self::RestoreTools { .. } => true,
        }
    }
}

impl OwnerClient {
    /// Reads one page of skills. Inputs: a project to include its own skills and its
    /// switches for global ones (or none for global settings) and the page offset.
    /// Output: skills with name, description, scope and On flags, and the next offset.
    pub async fn plugin_skills(
        &self,
        project: Option<ProjectId>,
        offset: u16,
    ) -> Result<SkillPage, EngineError> {
        match self
            .plugins_exchange(Action::Skills { project, offset }, Vec::new(), self.timeout)
            .await?
        {
            Reply::Skills { page } => Ok(page),
            _ => Err(unknown()),
        }
    }

    /// Reads one page of a skill's detail view. Inputs: the skill id, a project to include
    /// that project's switch (or none for global settings) and the SKILL.md byte offset, 0
    /// first and then each page's `next`. Output: the skill, that part of its SKILL.md, the
    /// next offset and on the first page its files; `skill_not_found` for a skill the project
    /// does not see.
    pub async fn skill_page(
        &self,
        id: String,
        project: Option<ProjectId>,
        offset: u32,
    ) -> Result<crate::skills::SkillPage, EngineError> {
        match self
            .plugins_exchange(
                Action::SkillDetail {
                    id,
                    project,
                    offset,
                },
                Vec::new(),
                self.timeout,
            )
            .await?
        {
            Reply::SkillDetail { page } => Ok(page),
            _ => Err(unknown()),
        }
    }

    /// Reads one page of MCP servers, with the same inputs as `plugin_skills`.
    /// Output: servers with how they run, plain status, secrets as saved names and tools.
    pub async fn plugin_servers(
        &self,
        project: Option<ProjectId>,
        offset: u16,
    ) -> Result<ServerPage, EngineError> {
        match self
            .plugins_exchange(
                Action::Servers { project, offset },
                Vec::new(),
                self.timeout,
            )
            .await?
        {
            Reply::Servers { page } => Ok(page),
            _ => Err(unknown()),
        }
    }

    /// Reads one MCP server by id. Output: the server as the lists show it.
    pub async fn plugin_server(&self, id: String) -> Result<ServerView, EngineError> {
        self.server_reply(Action::Server { id }, Vec::new()).await
    }

    /// Adds a skill by copying an owner-picked folder that holds SKILL.md into the engine's
    /// store. Inputs: global or project scope and the absolute folder path. Output: the skill.
    pub async fn add_skill(&self, scope: Scope, folder: String) -> Result<SkillView, EngineError> {
        self.skill_reply(Action::AddSkill { scope, folder }).await
    }

    /// Looks for skills in the owner's `~/.claude/skills` and `~/.codex/skills` folders,
    /// read only now. Output: each skill found with its name, one-line description, folder
    /// and whether the app already has it, the same or with different files.
    pub async fn skill_import_scan(
        &self,
    ) -> Result<crate::skills::import::ImportList, EngineError> {
        match self
            .plugins_exchange(
                Action::ImportScan,
                Vec::new(),
                self.timeout.max(IMPORT_TIMEOUT),
            )
            .await?
        {
            Reply::ImportScan { list } => Ok(list),
            _ => Err(unknown()),
        }
    }

    /// Imports one found skill into the app's skills for every project, turned on, leaving
    /// out dotfiles and credential-like files and never changing the source. Inputs: the
    /// absolute folder and, for a skill the app has with different files, its id to replace
    /// in place keeping its project switches. Output: the skill and the paths left out.
    pub async fn import_skill(
        &self,
        folder: String,
        replace: Option<String>,
    ) -> Result<crate::skills::import::Imported, EngineError> {
        match self
            .plugins_exchange(
                Action::ImportSkill { folder, replace },
                Vec::new(),
                self.timeout.max(IMPORT_TIMEOUT),
            )
            .await?
        {
            Reply::Imported { imported } => Ok(imported),
            _ => Err(unknown()),
        }
    }

    /// Turns a skill on or off where it was added. Inputs: skill id, the new state and the
    /// version the owner's screen showed; a skill changed since is refused `stale_revision`.
    pub async fn set_skill_enabled(
        &self,
        id: String,
        enabled: bool,
        version: u64,
    ) -> Result<SkillView, EngineError> {
        self.skill_reply(Action::SetSkill {
            id,
            enabled,
            version,
        })
        .await
    }

    /// Removes a skill and its stored folder copy. Inputs: the skill id and the version the
    /// screen showed. Output: done, or `stale_revision` after another change.
    pub async fn remove_skill(&self, id: String, version: u64) -> Result<(), EngineError> {
        self.done_reply(Action::RemoveSkill { id, version }).await
    }

    /// Reads one page of hooks, with the same inputs as `plugin_skills`. Output: each hook
    /// with its plugin, event, matcher, command, scope, On flags and the item it follows.
    pub async fn plugin_hooks(
        &self,
        project: Option<ProjectId>,
        offset: u16,
    ) -> Result<HookPage, EngineError> {
        match self
            .plugins_exchange(Action::Hooks { project, offset }, Vec::new(), self.timeout)
            .await?
        {
            Reply::Hooks { page } => Ok(page),
            _ => Err(unknown()),
        }
    }

    /// Adds the command hooks a Claude Code plugin folder defines, turned on, copying the
    /// files they run from into the engine's store; nothing in the folder runs. Inputs:
    /// global or project scope and the absolute plugin folder. Output: the added hooks.
    pub async fn add_hooks(&self, scope: Scope, folder: String) -> Result<HookPage, EngineError> {
        match self
            .plugins_exchange(Action::AddHooks { scope, folder }, Vec::new(), self.timeout)
            .await?
        {
            Reply::Hooks { page } => Ok(page),
            _ => Err(unknown()),
        }
    }

    /// Turns a hook on or off where it was added. Inputs: hook id and the new state.
    pub async fn set_hook_enabled(
        &self,
        id: String,
        enabled: bool,
    ) -> Result<HookView, EngineError> {
        match self
            .plugins_exchange(Action::SetHook { id, enabled }, Vec::new(), self.timeout)
            .await?
        {
            Reply::Hook { hook } => Ok(hook),
            _ => Err(unknown()),
        }
    }

    /// Adds an MCP server that waits for approval, with its secrets in the same request:
    /// each value goes in a private frame of its own, and the engine keeps the server with
    /// every secret or keeps none of them. Inputs: scope, a plain name, the command or URL
    /// and the secrets by name. Output: the server with status "Waiting for approval".
    pub async fn add_mcp_server(
        &self,
        scope: Scope,
        name: String,
        spec: ServerSpec,
        secrets: Vec<(String, AccountSecret)>,
    ) -> Result<ServerView, EngineError> {
        let (names, values): (Vec<String>, Vec<AccountSecret>) = secrets.into_iter().unzip();
        self.server_reply(
            Action::AddServer {
                scope,
                name,
                spec,
                secrets: names,
            },
            values,
        )
        .await
    }

    /// Changes how an MCP server runs. Any change sends it back to "Waiting for approval".
    /// Inputs: id, the new command or URL and the version the owner's screen showed; a server
    /// changed since is refused `stale_revision`.
    pub async fn update_mcp_server(
        &self,
        id: String,
        spec: ServerSpec,
        version: u64,
    ) -> Result<ServerView, EngineError> {
        self.server_reply(Action::UpdateServer { id, spec, version }, Vec::new())
            .await
    }

    /// Approves an MCP server exactly as the owner reviewed it. Inputs: id and the spec the
    /// owner saw; a server that changed since is refused. Output: the approved server.
    pub async fn approve_mcp_server(
        &self,
        id: String,
        spec: ServerSpec,
    ) -> Result<ServerView, EngineError> {
        self.server_reply(Action::Approve { id, spec }, Vec::new())
            .await
    }

    /// Turns an MCP server on or off where it was added. Inputs: id, the new state and the
    /// version the owner's screen showed. Output: the server, or `stale_revision`.
    pub async fn set_mcp_server_enabled(
        &self,
        id: String,
        enabled: bool,
        version: u64,
    ) -> Result<ServerView, EngineError> {
        self.server_reply(
            Action::SetServer {
                id,
                enabled,
                version,
            },
            Vec::new(),
        )
        .await
    }

    /// Removes an MCP server and deletes its saved secrets. Inputs: the server id and the
    /// version the screen showed. Output: done, or `stale_revision` after another change.
    pub async fn remove_mcp_server(&self, id: String, version: u64) -> Result<(), EngineError> {
        self.done_reply(Action::RemoveServer { id, version }).await
    }

    /// Switches a global skill or MCP server on or off for one project.
    /// Inputs: the project, the global item id, the new state and the item's version the
    /// owner's screen showed; an item changed since is refused `stale_revision`.
    pub async fn switch_plugin(
        &self,
        project: ProjectId,
        id: String,
        on: bool,
        version: u64,
    ) -> Result<(), EngineError> {
        self.done_reply(Action::Switch {
            project,
            id,
            on,
            version,
        })
        .await
    }

    /// Saves one secret for an MCP server, sent only as a separate frame and kept by the
    /// engine in a private file. Inputs: server id, the secret's name and its value.
    pub async fn set_mcp_secret(
        &self,
        id: String,
        name: String,
        value: AccountSecret,
    ) -> Result<ServerView, EngineError> {
        self.server_reply(Action::SetSecret { id, name }, vec![value])
            .await
    }

    /// Deletes one secret from an MCP server. Inputs: server id and the secret's name.
    pub async fn remove_mcp_secret(
        &self,
        id: String,
        name: String,
    ) -> Result<ServerView, EngineError> {
        self.server_reply(Action::RemoveSecret { id, name }, Vec::new())
            .await
    }

    /// Checks a command MCP server: the engine starts it, lists its tools and stops it.
    /// Input: the server id. Output: the server after the check, with tools or the plain
    /// reason it can't start.
    pub async fn check_mcp_server(&self, id: String) -> Result<ServerView, EngineError> {
        let before = self.plugin_server(id.clone()).await?.checks;
        self.server_reply(Action::Check { id: id.clone() }, Vec::new())
            .await?;
        let deadline = tokio::time::Instant::now() + check::CHECK_TIME + Duration::from_secs(3);
        loop {
            let server = self.plugin_server(id.clone()).await?;
            if server.checks > before {
                return Ok(server);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(failure("server_check_timeout"));
            }
            tokio::time::sleep(CHECK_POLL).await;
        }
    }

    /// Reads one server's tools with their switches: the CrazyAgent built-in tools (no server
    /// id) or one MCP server's checked tools, for every project (no project) or one project.
    /// Output: each tool with its every-project switch and the project's own switch.
    pub async fn tool_switches(
        &self,
        project: Option<ProjectId>,
        server: Option<String>,
    ) -> Result<Vec<ToolView>, EngineError> {
        self.tools_reply(Action::Tools { project, server }).await
    }

    /// Sets one tool's switch for every project (no project) or one project; Hidden deletes a
    /// built-in tool until its defaults are restored. Inputs: project, server id (none for the
    /// built-in tools), tool and switch. Output: the server's tools after the change.
    pub async fn set_tool_switch(
        &self,
        project: Option<ProjectId>,
        server: Option<String>,
        tool: String,
        switch: Switch,
    ) -> Result<Vec<ToolView>, EngineError> {
        self.tools_reply(Action::SetTool {
            project,
            server,
            tool,
            switch,
        })
        .await
    }

    /// Clears every stored switch of one server in that scope. Inputs: project (none for every
    /// project) and server id (none for the built-in tools). Output: the server's tools.
    pub async fn restore_tool_switches(
        &self,
        project: Option<ProjectId>,
        server: Option<String>,
    ) -> Result<Vec<ToolView>, EngineError> {
        self.tools_reply(Action::RestoreTools { project, server })
            .await
    }

    async fn tools_reply(&self, action: Action) -> Result<Vec<ToolView>, EngineError> {
        match self
            .plugins_exchange(action, Vec::new(), self.timeout)
            .await?
        {
            Reply::Tools { tools } => Ok(tools),
            _ => Err(unknown()),
        }
    }

    /// Reads exactly what agents launched for a project receive. Input: the project.
    /// Output: skill folders and approved MCP servers with secret references.
    pub async fn project_plugins(
        &self,
        project: ProjectId,
    ) -> Result<EffectivePlugins, EngineError> {
        match self
            .plugins_exchange(Action::Effective { project }, Vec::new(), self.timeout)
            .await?
        {
            Reply::Effective { plugins } => Ok(plugins),
            _ => Err(unknown()),
        }
    }

    async fn skill_reply(&self, action: Action) -> Result<SkillView, EngineError> {
        match self
            .plugins_exchange(action, Vec::new(), self.timeout)
            .await?
        {
            Reply::Skill { skill } => Ok(skill),
            _ => Err(unknown()),
        }
    }

    async fn server_reply(
        &self,
        action: Action,
        secrets: Vec<AccountSecret>,
    ) -> Result<ServerView, EngineError> {
        match self.plugins_exchange(action, secrets, self.timeout).await? {
            Reply::Server { server } => Ok(server),
            _ => Err(unknown()),
        }
    }

    async fn done_reply(&self, action: Action) -> Result<(), EngineError> {
        match self
            .plugins_exchange(action, Vec::new(), self.timeout)
            .await?
        {
            Reply::Done => Ok(()),
            _ => Err(unknown()),
        }
    }

    async fn plugins_exchange(
        &self,
        action: Action,
        secrets: Vec<AccountSecret>,
        timeout: Duration,
    ) -> Result<Reply, EngineError> {
        action.validate()?;
        tokio::time::timeout(timeout, async {
            let read = read_kind(&action);
            let envelope = Envelope {
                plugins: action,
                secret_present: !secrets.is_empty(),
            };
            let frame = encode_frame(&envelope).map_err(|_| failure("plugins_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("plugins_owner_unavailable"))?;
            verify_peer(&stream)?;
            let mut packet = [0; 37];
            packet[..4].copy_from_slice(b"CAO1");
            packet[4] = match self.kind {
                OwnerClientKind::Tui => 0,
                OwnerClientKind::Desktop => 1,
            };
            packet[5..].copy_from_slice(&self.token);
            stream
                .write_all(&packet)
                .await
                .map_err(|_| failure("plugins_owner_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("plugins_owner_unavailable"))?;
            for secret in secrets {
                let mut bytes = secret.into_bytes();
                let mut framed = (bytes.len() as u32).to_be_bytes().to_vec();
                framed.extend_from_slice(&bytes);
                bytes.fill(0);
                let sent = stream.write_all(&framed).await;
                framed.fill(0);
                sent.map_err(|_| failure("plugins_owner_unavailable"))?;
            }
            let frame = read_frame(&mut stream).await.map_err(|_| unknown())?;
            match serde_json::from_slice(&frame[4..])
                .map_err(|error| self.skewed_reply(&error, read, unknown))?
            {
                Reply::Rejected { code } => Err(EngineError(code)),
                reply => Ok(reply),
            }
        })
        .await
        .map_err(|_| unknown())?
    }
}

impl Engine {
    /// Reads the skills and MCP servers agents launched for one project receive, for the
    /// harness launchers inside the engine. Input: the project. Output: skill folder paths
    /// and approved MCP servers; read each secret with `storage::plugins::read_secret`.
    pub(crate) async fn project_plugins(
        &self,
        project: &ProjectId,
    ) -> Result<EffectivePlugins, EngineError> {
        match actor(
            self,
            Command::Effective {
                project: project.clone(),
            },
        )
        .await?
        {
            ResultValue::Effective(plugins) => Ok(*plugins),
            _ => Err(unknown()),
        }
    }
}

impl Engine {
    /// Reads what agents launched for one project receive, with every MCP server secret
    /// value already read from its private file, for the harness launchers inside the
    /// engine. Input: the project. Output: skill folders and approved servers whose secret
    /// values are zeroed when dropped and redacted in `Debug`.
    pub async fn project_launch_plugins(
        &self,
        project: &ProjectId,
    ) -> Result<crate::plugins::LaunchPlugins, EngineError> {
        match actor(
            self,
            Command::Launch {
                project: project.clone(),
            },
        )
        .await?
        {
            ResultValue::Launch(plugins) => Ok(*plugins),
            _ => Err(unknown()),
        }
    }
}

/// Encodes this protocol's refusal of a request the engine did not run.
/// Input: the refusal code. Output: the reply frame.
pub(super) fn refusal(code: &str) -> Result<Vec<u8>, EngineError> {
    encode_frame(&Reply::Rejected { code: code.into() }).map_err(|_| unknown())
}

/// Tells whether a Plugins request is a skill import scan or import, which copies skill
/// folders and is served past the owner exchange deadline within `IMPORT_TIMEOUT`.
/// Input: the request body. Output: true for those two; false for any other or an
/// unreadable request, which the inline path refuses.
pub(super) fn imports(bytes: &[u8]) -> bool {
    serde_json::from_slice::<Envelope>(bytes).is_ok_and(|envelope| {
        matches!(
            envelope.plugins,
            Action::ImportScan | Action::ImportSkill { .. }
        )
    })
}

/// A plugins request read and checked once: the reply of a read already made, or a change
/// still to make under the owner barrier, with the secret it carries.
enum Prepared {
    Read(Box<Reply>),
    Change(Action, Vec<AccountSecret>),
}

/// Serves one plugins request. A read is made first; then, under the owner barrier, the
/// grant is checked again, any change is made and the reply is written, so a revocation
/// that returned saw every change and reply admitted before it and lets none through after.
/// Inputs: the connection, engine, authority, packet, admission and request body.
/// Output: done once the reply is written.
pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    let envelope: Envelope =
        serde_json::from_slice(bytes).map_err(|_| failure("plugins_request_invalid"))?;
    let frames = if envelope.secret_present {
        secret_frames(&envelope.plugins).clamp(1, crate::plugins::SECRETS_PER_SERVER_MAX)
    } else {
        0
    };
    let mut secrets = Vec::with_capacity(frames);
    for _ in 0..frames {
        secrets.push(receive_secret(stream).await?);
    }
    let prepared = prepare(
        engine,
        authority,
        packet,
        admission,
        envelope.plugins,
        secrets,
    )
    .await;
    let _barrier = engine.owner_egress.write().await;
    let reply = match finish(engine, authority, packet, admission, prepared).await {
        Ok(reply) => reply,
        Err(error) => Reply::Rejected {
            code: public_code(error),
        },
    };
    let frame = match encode_frame(&reply) {
        Ok(frame) => frame,
        Err(_) => encode_frame(&Reply::Rejected {
            code: "plugins_reply_bounds".into(),
        })
        .map_err(|_| unknown())?,
    };
    write_reply(stream, &frame).await
}

/// Checks the grant of a plugins request, and makes a read outside the owner barrier.
/// Inputs: the engine, authority, packet, admission, request and its secret.
/// Output: the prepared request, or the refusal.
async fn prepare(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    action: Action,
    secrets: Vec<AccountSecret>,
) -> Result<Prepared, EngineError> {
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    if action.changes() {
        return Ok(Prepared::Change(action, secrets));
    }
    handle(engine, authority, action, secrets)
        .await
        .map(|reply| Prepared::Read(Box::new(reply)))
}

/// Answers a prepared request under the owner barrier its caller holds: checks the grant
/// again, then makes any change. Inputs: the engine, authority, packet, admission and the
/// prepared request. Output: the reply, or the refusal.
async fn finish(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    prepared: Result<Prepared, EngineError>,
) -> Result<Reply, EngineError> {
    let prepared = prepared?;
    if &authority.authenticate(packet)? != admission {
        return Err(failure("owner_grant_stale_or_revoked"));
    }
    match prepared {
        Prepared::Read(reply) => Ok(*reply),
        Prepared::Change(action, secrets) => handle(engine, authority, action, secrets).await,
    }
}

fn engine_root(authority: &Authority) -> Result<PathBuf, EngineError> {
    authority.root.verify()?;
    authority
        .root
        .path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| failure("plugin_store_unavailable"))
}

/// Names the sandbox profile file the checks of one server run under.
/// Inputs: the engine's check folder and the server id. Output: the profile path.
fn check_profile(checks: &Path, id: &str) -> PathBuf {
    checks.join(CHECK_PROFILES).join(format!("{id}.sb"))
}

/// Clears a removed server's check folder, with whatever its checks wrote there while they
/// held the server's secrets, and its check profile: the folder moves aside at once and is
/// deleted on a blocking thread of its own that nothing waits for, so no check home size
/// holds up the answer, and the start sweep deletes a moved folder a crash left. Inputs:
/// the engine's check folder and the server id. A failure other than a missing entry is logged.
fn remove_check_folder(checks: &Path, id: &str) {
    match std::fs::remove_file(check_profile(checks, id)) {
        Err(problem) if problem.kind() != std::io::ErrorKind::NotFound => {
            eprintln!("plugin_check_folder_not_removed cause={problem}");
        }
        _ => {}
    }
    match crate::storage::plugins::move_check_folder_aside(checks, id) {
        Ok(Some(aside)) => drop(tokio::task::spawn_blocking(move || {
            if let Err(problem) = std::fs::remove_dir_all(aside) {
                eprintln!("plugin_check_folder_not_removed cause={problem}");
            }
        })),
        Ok(None) => {}
        Err(problem) => eprintln!("plugin_check_folder_not_removed cause={}", problem.0),
    }
}

async fn handle(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    action: Action,
    secrets: Vec<AccountSecret>,
) -> Result<Reply, EngineError> {
    action.validate()?;
    if secrets.len() != secret_frames(&action) {
        return Err(failure("server_secret_invalid"));
    }
    Ok(match action {
        Action::Skills { project, offset } => {
            let ResultValue::Skills(skills, next) =
                actor(engine, Command::Skills { project, offset }).await?
            else {
                return Err(unknown());
            };
            Reply::Skills {
                page: SkillPage { skills, next },
            }
        }
        Action::SkillDetail {
            id,
            project,
            offset,
        } => {
            let ResultValue::Skill(skill) = actor(engine, Command::Skill { id, project }).await?
            else {
                return Err(unknown());
            };
            let store = engine_root(authority)?.join(crate::storage::plugins::STORE);
            let folder = PathBuf::from(&skill.view.folder);
            let inside = folder.starts_with(&store)
                && !folder
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir));
            if !inside {
                return Err(failure("plugin_store_unsafe"));
            }
            let page = tokio::task::spawn_blocking(move || crate::skills::page(skill.view, offset))
                .await
                .map_err(|_| unknown())??;
            Reply::SkillDetail { page }
        }
        Action::Servers { project, offset } => {
            let ResultValue::Servers(servers, next) =
                actor(engine, Command::Servers { project, offset }).await?
            else {
                return Err(unknown());
            };
            Reply::Servers {
                page: ServerPage { servers, next },
            }
        }
        Action::Server { id } => server(actor(engine, Command::Server { id }).await?)?,
        Action::AddSkill { scope, folder } => {
            let ResultValue::Skill(skill) = actor(
                engine,
                Command::AddSkill {
                    scope,
                    source: folder,
                },
            )
            .await?
            else {
                return Err(unknown());
            };
            Reply::Skill { skill: skill.view }
        }
        Action::ImportScan => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .ok_or_else(|| failure("skill_import_home_unknown"))?;
            let mut added = Vec::new();
            let mut offset = 0;
            for page in 0.. {
                assert!(page < CATALOG_PAGES_MAX, "the global skill list is bounded");
                let ResultValue::Skills(skills, next) = actor(
                    engine,
                    Command::Skills {
                        project: None,
                        offset,
                    },
                )
                .await?
                else {
                    return Err(unknown());
                };
                added.extend(skills);
                match next {
                    Some(next) => offset = next,
                    None => break,
                }
            }
            let list =
                tokio::task::spawn_blocking(move || crate::skills::import::scan(&home, &added))
                    .await
                    .map_err(|_| unknown())??;
            Reply::ImportScan { list }
        }
        Action::ImportSkill { folder, replace } => {
            let ResultValue::Imported(skill, skipped, total) = engine
                .storage_job_within(IMPORT_TIMEOUT, |reply| {
                    Job::Plugins(
                        Command::ImportSkill {
                            source: folder,
                            replace,
                        },
                        reply,
                    )
                })
                .await?
            else {
                return Err(unknown());
            };
            Reply::Imported {
                imported: crate::skills::import::Imported {
                    skill: skill.view,
                    skipped,
                    skipped_total: u32::try_from(total).map_err(|_| unknown())?,
                },
            }
        }
        Action::SetSkill {
            id,
            enabled,
            version,
        } => {
            let ResultValue::Skill(skill) = actor(
                engine,
                Command::SetSkill {
                    id,
                    enabled,
                    version,
                },
            )
            .await?
            else {
                return Err(unknown());
            };
            Reply::Skill { skill: skill.view }
        }
        Action::RemoveSkill { id, version } => {
            actor(engine, Command::RemoveSkill { id, version }).await?;
            Reply::Done
        }
        Action::AddServer {
            scope,
            name,
            spec,
            secrets: names,
        } => {
            let root = engine_root(authority)?;
            let mut stored: Vec<crate::plugins::EffectiveSecret> = Vec::with_capacity(names.len());
            for (secret, value) in names.into_iter().zip(secrets) {
                let kept = crate::storage::plugins::new_secret_reference().and_then(|reference| {
                    crate::storage::plugins::store_secret(&root, &reference, value.into_bytes())?;
                    Ok(reference)
                });
                match kept {
                    Ok(reference) => stored.push(crate::plugins::EffectiveSecret {
                        name: secret,
                        reference,
                    }),
                    Err(problem) => {
                        for kept in &stored {
                            let _ = crate::storage::plugins::remove_secret(&root, &kept.reference);
                        }
                        return Err(problem);
                    }
                }
            }
            let references: Vec<String> = stored
                .iter()
                .map(|secret| secret.reference.clone())
                .collect();
            server(
                engine
                    .settled_job(
                        |reply| {
                            Job::Plugins(
                                Command::AddServer {
                                    scope,
                                    name,
                                    spec,
                                    secrets: stored,
                                },
                                reply,
                            )
                        },
                        move |result| match result {
                            Ok(_) => Ok(()),
                            Err(_) => {
                                let _ = crate::storage::plugins::remove_secrets(&root, &references);
                                Ok(())
                            }
                        },
                    )
                    .await?,
            )?
        }
        Action::UpdateServer { id, spec, version } => {
            server(actor(engine, Command::UpdateServer { id, spec, version }).await?)?
        }
        Action::Approve { id, spec } => {
            server(actor(engine, Command::Approve { id, spec }).await?)?
        }
        Action::SetServer {
            id,
            enabled,
            version,
        } => server(
            actor(
                engine,
                Command::SetServer {
                    id,
                    enabled,
                    version,
                },
            )
            .await?,
        )?,
        Action::RemoveServer { id, version } => {
            let root = engine_root(authority)?;
            let checks = root.join(CHECK_FOLDER);
            let server = id.clone();
            let removed = engine
                .settled_job(
                    move |reply| Job::Plugins(Command::RemoveServer { id, version }, reply),
                    move |result| match result {
                        Ok(ResultValue::Removed(references)) => {
                            let secrets =
                                crate::storage::plugins::remove_secrets(&root, references);
                            remove_check_folder(&checks, &server);
                            secrets
                        }
                        _ => Ok(()),
                    },
                )
                .await?;
            let ResultValue::Removed(_) = removed else {
                return Err(unknown());
            };
            Reply::Done
        }
        Action::Switch {
            project,
            id,
            on,
            version,
        } => {
            actor(
                engine,
                Command::Switch {
                    project,
                    id,
                    on,
                    version,
                },
            )
            .await?;
            Reply::Done
        }
        Action::Tools { project, server } => {
            tools(actor(engine, Command::Tools { project, server }).await?)?
        }
        Action::SetTool {
            project,
            server,
            tool,
            switch,
        } => tools(
            actor(
                engine,
                Command::SetTool {
                    project,
                    server,
                    tool,
                    switch,
                },
            )
            .await?,
        )?,
        Action::RestoreTools { project, server } => {
            tools(actor(engine, Command::RestoreTools { project, server }).await?)?
        }
        Action::SetSecret { id, name } => {
            let secret = secrets
                .into_iter()
                .next()
                .ok_or_else(|| failure("server_secret_invalid"))?;
            let root = engine_root(authority)?;
            let reference = crate::storage::plugins::new_secret_reference()?;
            crate::storage::plugins::store_secret(&root, &reference, secret.into_bytes())?;
            let stored = reference.clone();
            let attached = engine
                .settled_job(
                    |reply| {
                        Job::Plugins(
                            Command::AttachSecret {
                                id,
                                name,
                                reference,
                            },
                            reply,
                        )
                    },
                    move |result| match result {
                        Ok(ResultValue::Secret(_, Some(old))) => {
                            crate::storage::plugins::remove_secret(&root, old)
                        }
                        Ok(_) => Ok(()),
                        Err(_) => {
                            let _ = crate::storage::plugins::remove_secret(&root, &stored);
                            Ok(())
                        }
                    },
                )
                .await?;
            let ResultValue::Secret(view, _) = attached else {
                return Err(unknown());
            };
            Reply::Server { server: *view }
        }
        Action::RemoveSecret { id, name } => {
            let root = engine_root(authority)?;
            let detached = engine
                .settled_job(
                    |reply| Job::Plugins(Command::DetachSecret { id, name }, reply),
                    move |result| match result {
                        Ok(ResultValue::Secret(_, Some(reference))) => {
                            crate::storage::plugins::remove_secret(&root, reference)
                        }
                        _ => Ok(()),
                    },
                )
                .await?;
            let ResultValue::Secret(view, _) = detached else {
                return Err(unknown());
            };
            Reply::Server { server: *view }
        }
        Action::Check { id } => start_check(engine, authority, id).await?,
        Action::Effective { project } => Reply::Effective {
            plugins: engine.project_plugins(&project).await?,
        },
        Action::Hooks { project, offset } => {
            let ResultValue::Hooks(hooks, next) =
                actor(engine, Command::Hooks { project, offset }).await?
            else {
                return Err(unknown());
            };
            Reply::Hooks {
                page: HookPage { hooks, next },
            }
        }
        Action::AddHooks { scope, folder } => {
            let ResultValue::HooksAdded(hooks, _) = actor(
                engine,
                Command::AddHooks {
                    scope,
                    source: folder,
                    follows: None,
                },
            )
            .await?
            else {
                return Err(unknown());
            };
            Reply::Hooks {
                page: HookPage { hooks, next: None },
            }
        }
        Action::SetHook { id, enabled } => {
            let ResultValue::Hook(hook) = actor(engine, Command::SetHook { id, enabled }).await?
            else {
                return Err(unknown());
            };
            Reply::Hook { hook: *hook }
        }
    })
}

fn tools(value: ResultValue) -> Result<Reply, EngineError> {
    match value {
        ResultValue::Tools(tools) => Ok(Reply::Tools { tools }),
        _ => Err(unknown()),
    }
}

fn server(value: ResultValue) -> Result<Reply, EngineError> {
    match value {
        ResultValue::Server(server) => Ok(Reply::Server { server: *server }),
        _ => Err(unknown()),
    }
}

struct CheckSlot;

impl CheckSlot {
    fn take() -> Result<Self, EngineError> {
        if CHECKS.fetch_add(1, Ordering::SeqCst) >= CHECKS_MAX {
            CHECKS.fetch_sub(1, Ordering::SeqCst);
            return Err(failure("server_check_busy"));
        }
        Ok(Self)
    }
}

impl Drop for CheckSlot {
    fn drop(&mut self) {
        let before = CHECKS.fetch_sub(1, Ordering::SeqCst);
        assert!(before > 0);
    }
}

async fn start_check(
    engine: &Arc<Engine>,
    authority: &Authority,
    id: String,
) -> Result<Reply, EngineError> {
    let ResultValue::Check(spec, secrets, network) =
        actor(engine, Command::GetServer { id: id.clone() }).await?
    else {
        return Err(unknown());
    };
    let digest = spec.digest();
    let ServerSpec::Stdio { command, args } = *spec else {
        return Err(failure("server_check_command_only"));
    };
    let slot = CheckSlot::take()?;
    let root = engine_root(authority)?;
    let mut values: Vec<(String, Vec<u8>)> = Vec::with_capacity(secrets.len());
    for secret in &secrets {
        match crate::storage::plugins::read_secret(&root, &secret.reference) {
            Ok(value) => values.push((secret.name.clone(), value)),
            Err(problem) => {
                for (_, value) in values.iter_mut() {
                    value.fill(0);
                }
                return Err(problem);
            }
        }
    }
    let ResultValue::Server(view) = actor(engine, Command::Server { id: id.clone() }).await? else {
        return Err(unknown());
    };
    let engine = engine.clone();
    tokio::spawn(async move {
        let _slot = slot;
        let folder = id.clone();
        let settings = engine.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            let outcome = settings.owner_ports().and_then(|kept| {
                boundary_check(&root, &folder, &command, &args, &values, &kept, network)
            });
            for (_, value) in values.iter_mut() {
                value.fill(0);
            }
            outcome
        })
        .await;
        let outcome = match outcome {
            Ok(Ok(tools)) => Ok(tools),
            Ok(Err(code)) => Err(code.to_owned()),
            Err(_) => Err("server_cant_start".to_owned()),
        };
        if let Err(problem) = actor(
            &engine,
            Command::RecordCheck {
                id,
                digest,
                outcome,
            },
        )
        .await
        {
            eprintln!("plugin_check_not_recorded cause={}", problem.0);
        }
    });
    Ok(Reply::Server { server: *view })
}

/// Checks an approved command server inside the boundary agents run it in: its own
/// project, home and temp folders under `mcp-checks/<id>` in the engine folder, reads of
/// system paths and the command's folder only, open network without binding the owner's
/// ports, and only its own secrets. Inputs: the engine folder, server id, command,
/// arguments, secret values and the configured local model ports to keep. Output: the
/// tool names, or a plain rejection code.
#[cfg(unix)]
fn boundary_check(
    root: &Path,
    id: &str,
    command: &str,
    args: &[String],
    secrets: &[(String, Vec<u8>)],
    kept: &[u16],
    network: crate::sandbox::SandboxNetwork,
) -> Result<Vec<String>, &'static str> {
    use crate::sandbox::{InstalledSandbox, ProjectSandbox};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    assert!(valid_id(id));
    let program = Path::new(command);
    match std::fs::metadata(program) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err("server_command_not_found");
        }
        Ok(found) if found.is_file() && found.permissions().mode() & 0o111 != 0 => {}
        _ => return Err("server_command_not_runnable"),
    }
    let refused = |cause: &dyn std::fmt::Display| {
        eprintln!("plugin_check_boundary_refused cause={cause}");
        "server_cant_start"
    };
    let checks = root.join(CHECK_FOLDER);
    let mut folders = Vec::with_capacity(3);
    for name in ["project", "home", "tmp"] {
        let folder = checks.join(id).join(name);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&folder)
            .map_err(|error| refused(&error))?;
        let plain = checks
            .join(id)
            .canonicalize()
            .map_err(|error| refused(&error))?
            .join(name);
        if folder.canonicalize().map_err(|error| refused(&error))? != plain {
            return Err(refused(
                &"check folder is not the plain folder the engine made",
            ));
        }
        folders.push(plain);
    }
    let profiles = checks.join(CHECK_PROFILES);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&profiles)
        .map_err(|error| refused(&error))?;
    let reads = program.parent().ok_or("server_command_not_runnable")?;
    let sandbox = ProjectSandbox::new(&folders[0], &folders[1], &folders[2], network)
        .and_then(|sandbox| sandbox.keep_ports(kept))
        .and_then(|sandbox| sandbox.guard_engine(root))
        .and_then(|sandbox| sandbox.allow_read(reads))
        .and_then(|sandbox| InstalledSandbox::install(sandbox, &check_profile(&checks, id)))
        .map_err(|error| refused(&error))?;
    let mut server = std::process::Command::new(program);
    server.args(args).current_dir(&folders[0]);
    let mut process = sandbox.wrap(&server).map_err(|error| refused(&error))?;
    for (name, value) in secrets {
        process.env(name, std::ffi::OsStr::from_bytes(value));
    }
    assert_eq!(process.get_program(), sandbox.launcher().as_os_str());
    check::check_command(process, check::CHECK_TIME)
}

/// Refuses to check a command server where no process group can be stopped. Takes the
/// engine folder, server id, command, arguments, secrets and kept ports, all unused. Returns
/// `server_check_unavailable`, like [`check::check_stdio`] on this system.
#[cfg(not(unix))]
fn boundary_check(
    _root: &Path,
    _id: &str,
    _command: &str,
    _args: &[String],
    _secrets: &[(String, Vec<u8>)],
    _kept: &[u16],
    _network: crate::sandbox::SandboxNetwork,
) -> Result<Vec<String>, &'static str> {
    Err("server_check_unavailable")
}

async fn actor(engine: &Engine, command: Command) -> Result<ResultValue, EngineError> {
    engine
        .native_auth_job(|reply| Job::Plugins(command, reply))
        .await
}

/// How many private secret frames follow a plugins request: one for a secret change, one
/// for each secret a server is added with, none otherwise. Input: the request. Output: the
/// count.
fn secret_frames(action: &Action) -> usize {
    match action {
        Action::SetSecret { .. } => 1,
        Action::AddServer { secrets, .. } => secrets.len(),
        _ => 0,
    }
}

async fn receive_secret(stream: &mut UnixStream) -> Result<AccountSecret, EngineError> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let invalid = || failure("server_secret_invalid");
        let size = stream.read_u32().await.map_err(|_| invalid())? as usize;
        if size == 0 || size > SECRET_MAX {
            return Err(invalid());
        }
        let mut bytes = vec![0; size];
        if stream.read_exact(&mut bytes).await.is_err() {
            bytes.fill(0);
            return Err(invalid());
        }
        AccountSecret::new(bytes).map_err(|_| invalid())
    })
    .await
    .map_err(|_| failure("server_secret_invalid"))?
}

/// The code a refused plugins request is answered with: the engine's code when it is plain
/// lowercase words of at most 96 bytes, else `plugins_outcome_unknown`. A storage answer
/// that did not come within the owner's wait, which the shared engine waits name after a
/// native sign-in read, is this protocol's `plugins_outcome_unknown` too, so a plugins
/// client is never told to read a sign-in operation. Input: the error. Output: the code.
fn public_code(error: EngineError) -> String {
    if error == crate::engine::owner::native_auth::outcome_unknown() {
        return unknown().0;
    }
    if error.0.len() <= 96
        && error
            .0
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        error.0
    } else {
        "plugins_outcome_unknown".into()
    }
}

#[cfg(test)]
mod tests;
