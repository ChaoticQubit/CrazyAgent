use super::shared_folders::SharedFolders;
use super::toolchains::{ToolHomes, Toolchain};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

const TABLE: &str = "CREATE TABLE IF NOT EXISTS project_agent_policy (
    project TEXT PRIMARY KEY,
    body TEXT NOT NULL
)";

const BODY_BYTES_MAX: usize = 16 * 1024;

#[cfg(target_os = "windows")]
const WINDOWS_UNAVAILABLE: &str =
    "Agents don't start on Windows in this version: their launch needs the Windows sandbox helper.";

#[cfg(target_os = "linux")]
const LINUX_UNAVAILABLE: &str = "On Linux, agents run Sandboxed in this version.";

/// How free one project's agents are (DEC-102): Sandboxed writes only the agent's own
/// worktree and never git; Own branch adds full git in the agent's own git folder and
/// lands its commits only on Accept; Full access runs agents with the owner's user
/// permissions and no Seatbelt profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAccess {
    #[default]
    Sandboxed,
    OwnBranch,
    Full,
}

impl AgentAccess {
    /// Every level, from the most to the least confined.
    pub const ALL: [Self; 3] = [Self::Sandboxed, Self::OwnBranch, Self::Full];

    /// The level the New Project form preselects. A project with no saved level stays
    /// [`AgentAccess::Sandboxed`], the level every project had before the setting existed.
    pub const NEW_PROJECT: Self = Self::OwnBranch;

    /// The level's stable id, as stored and sent to clients.
    pub fn id(self) -> &'static str {
        match self {
            Self::Sandboxed => "sandboxed",
            Self::OwnBranch => "own_branch",
            Self::Full => "full",
        }
    }

    /// Tells whether this level gives agents more than `other`: Full access more than Own
    /// branch, Own branch more than Sandboxed. Input: the level to compare with. Output:
    /// true when this one is wider.
    pub fn wider_than(self, other: Self) -> bool {
        let rank = |level: Self| Self::ALL.iter().position(|each| *each == level);
        rank(self) > rank(other)
    }

    /// The name shown to the owner.
    pub fn label(self) -> &'static str {
        match self {
            Self::Sandboxed => "Sandboxed",
            Self::OwnBranch => "Own branch",
            Self::Full => "Full access",
        }
    }

    /// Why this level cannot run agents on this computer, or None when it can. A level
    /// that cannot run is refused when chosen and at launch, never mapped to another.
    pub fn unavailable(self) -> Option<&'static str> {
        #[cfg(target_os = "windows")]
        {
            Some(WINDOWS_UNAVAILABLE)
        }
        #[cfg(target_os = "linux")]
        {
            (self != Self::Sandboxed).then_some(LINUX_UNAVAILABLE)
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        {
            None
        }
    }
}

/// How far one project's tools-on agents and the plugin "Check again" server may reach on
/// the network (DEC-111 Q17): Open is the whole network, Https is port 443 only, None is
/// the model alone: a hosted model's own hosts through the engine's model proxy, or only a
/// local model's port on loopback. The same choice applies to the agents, research jobs
/// and the check. A master never reaches more than the model's port; under None a master
/// with tools reaches a model off loopback only through its model proxy in its live
/// terminal, and a master turn with tools is refused (S4B-M1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentNetwork {
    #[default]
    Open,
    Https,
    None,
}

impl AgentNetwork {
    /// Every choice, widest first.
    pub const ALL: [Self; 3] = [Self::Open, Self::Https, Self::None];

    /// The choice's stable id, as stored and sent to clients.
    pub fn id(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Https => "https",
            Self::None => "none",
        }
    }

    /// The name shown to the owner.
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Https => "Port 443 only",
            Self::None => "None",
        }
    }

    /// Tells whether this choice lets agents reach more than `other`: Open more than Port
    /// 443 only, Port 443 only more than None (CS-2). Input: the choice to compare with.
    /// Output: true when this one is wider.
    pub fn wider_than(self, other: Self) -> bool {
        let rank = |choice: Self| Self::ALL.iter().position(|each| *each == choice);
        rank(self) < rank(other)
    }

    /// The sandbox reach this choice grants. Input: the model endpoint the agent still
    /// needs, or None for the plugin check, which needs no model. Output: Open for Open,
    /// port 443 plus the model's port for Https, and for None the model only on loopback:
    /// a model on any host is reached under None only through a model proxy's loopback
    /// port, so no None path grants a host port or DNS (S4B-M1).
    pub fn sandbox(self, model: Option<super::SandboxRemote>) -> super::SandboxNetwork {
        match self {
            Self::Open => super::SandboxNetwork::Open,
            Self::Https => {
                let mut remotes = vec![super::SandboxRemote::AnyHost(443)];
                remotes.extend(model);
                super::SandboxNetwork::Only(remotes)
            }
            Self::None => super::SandboxNetwork::Only(
                model
                    .into_iter()
                    .filter(|remote| matches!(remote, super::SandboxRemote::Loopback(_)))
                    .collect(),
            ),
        }
    }
}

/// Who shares one project's tool cache, the uv cache and pnpm store a confined agent's
/// tools write (T8): Shared, the default, gives one cache to the project's agents at the
/// same access level that write the same kind of tree (the project folder, a linked
/// worktree, an own clone or a clone teammate); One per agent gives each agent its own. A
/// Full access agent gets none and uses its tools' own defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCacheScope {
    #[default]
    Shared,
    PerAgent,
}

impl ToolCacheScope {
    /// Every choice, the default first.
    pub const ALL: [Self; 2] = [Self::Shared, Self::PerAgent];

    /// The choice's stable id, as stored and sent to clients.
    pub fn id(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::PerAgent => "per_agent",
        }
    }

    /// The name shown to the owner.
    pub fn label(self) -> &'static str {
        match self {
            Self::Shared => "Shared by agents with the same access",
            Self::PerAgent => "One per agent",
        }
    }
}

/// What a launch does with the download caches an earlier run left in an agent home, its
/// `.cache` and npm's `_cacache` (DEC-173 add.1): Keep leaves them; Clear when access is
/// raised, the default, removes them when the launch runs at a wider level than the home's
/// last launch, so nothing a narrower run wrote reaches a wider one; Clear every launch
/// removes them each time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HomeCaches {
    Keep,
    #[default]
    ClearWhenRaised,
    ClearEveryLaunch,
}

impl HomeCaches {
    /// Every choice, in the order the owner reads them.
    pub const ALL: [Self; 3] = [Self::Keep, Self::ClearWhenRaised, Self::ClearEveryLaunch];

    /// The choice's stable id, as stored and sent to clients.
    pub fn id(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::ClearWhenRaised => "clear_when_raised",
            Self::ClearEveryLaunch => "clear_every_launch",
        }
    }

    /// The name shown to the owner, the same in the desktop app and the terminal app.
    pub fn label(self) -> &'static str {
        match self {
            Self::Keep => "Keep",
            Self::ClearWhenRaised => "Clear when access is raised",
            Self::ClearEveryLaunch => "Clear every launch",
        }
    }
}

/// How far a sandboxed IDE server or terminal at a level reaches: nothing at Sandboxed, and
/// the project's network choice at Own branch through [`AgentNetwork::sandbox`], with no
/// model port. A Full access process has no Seatbelt for a choice to limit, so it never
/// asks. Inputs: the level and the choice. Output: the sandbox's network.
pub fn ide_network(level: AgentAccess, choice: AgentNetwork) -> super::SandboxNetwork {
    assert_ne!(level, AgentAccess::Full);
    match level {
        AgentAccess::OwnBranch => choice.sandbox(None),
        AgentAccess::Sandboxed | AgentAccess::Full => super::SandboxNetwork::Only(Vec::new()),
    }
}

/// What the IDE may do to the files inside agent checkouts at Sandboxed and Own branch
/// (DEC-124): Allow all changes is today's reach; No delete or rename refuses every removal
/// and every rename there and keeps edits; Read only refuses every write there, Edit anyway
/// included. Full access runs the IDE with no profile, so no choice reaches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdeAgentFiles {
    #[default]
    AllowAll,
    NoDeleteOrRename,
    ReadOnly,
}

impl IdeAgentFiles {
    /// Every choice, widest first.
    pub const ALL: [Self; 3] = [Self::AllowAll, Self::NoDeleteOrRename, Self::ReadOnly];

    /// The name shown to the owner, the same in the desktop app.
    pub fn label(self) -> &'static str {
        match self {
            Self::AllowAll => "Allow all changes",
            Self::NoDeleteOrRename => "No delete or rename",
            Self::ReadOnly => "Read only",
        }
    }
}

/// One project's agent folders choice as a Seatbelt profile holds it (DEC-124), with whether
/// the older agents root `<project>/worktrees` still holds engine-made worktrees
/// ([`crate::ide::workspace::older_agents`]). While it holds none, that folder may be the
/// owner's own, so the rule leaves it alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AgentFilesRule {
    pub choice: IdeAgentFiles,
    pub older_agents: bool,
}

impl AgentFilesRule {
    /// The Seatbelt rules that hold the choice under the project's agents root, in any
    /// letter case: none for Allow all changes; for No delete or rename, no removal and no
    /// rename of an agent folder or of anything in it, so edits in place still land; for
    /// Read only, no write of any kind there. Either way neither the agents root nor the
    /// `.crazyagent` folder above it is removed or moved aside, in any letter case, so no
    /// agent folder leaves the rule's reach, not even through a `.CRAZYAGENT` folder made
    /// before the first agent and then holding it on a case-insensitive volume (DEC-124 r1
    /// rv-code L1); under Read only that folder is not made or changed either. Input: the
    /// project folder, a clean absolute path. Output: the rule text.
    pub fn seatbelt(self, root: &Path) -> String {
        let operation = match self.choice {
            IdeAgentFiles::AllowAll => return String::new(),
            IdeAgentFiles::NoDeleteOrRename => "file-write-unlink",
            IdeAgentFiles::ReadOnly => "file-write*",
        };
        assert!(super::clean_absolute(root));
        let root = super::any_case(root.to_str().expect("a clean absolute path is text"));
        let state = super::any_case("/.crazyagent");
        let layout = if self.older_agents {
            format!("({state})?")
        } else {
            state.clone()
        };
        let agents = format!("^{root}{layout}/{}", super::any_case("worktrees"));
        format!(
            "(deny {operation}\n  (regex #\"{agents}/[^/]+(/.*)?$\"))\n(deny file-write-unlink\n  (regex #\"{agents}$\"))\n(deny {operation}\n  (regex #\"^{root}{state}$\"))\n"
        )
    }
}

/// When a running agent picks up a saved policy that gives it more than it started with
/// (F08): When the agent is idle, the default, stops it at its next idle point so its next
/// start uses the new policy, the live master as a planned restart that resumes its own
/// conversation; At its next start leaves it running until it next starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessApply {
    #[default]
    WhenIdle,
    NextStart,
}

impl AccessApply {
    /// Every choice, the default first.
    pub const ALL: [Self; 2] = [Self::WhenIdle, Self::NextStart];

    /// The choice's stable id, as stored and sent to clients.
    pub fn id(self) -> &'static str {
        match self {
            Self::WhenIdle => "when_idle",
            Self::NextStart => "next_start",
        }
    }

    /// The name shown to the owner, the same in the desktop app.
    pub fn label(self) -> &'static str {
        match self {
            Self::WhenIdle => "When the agent is idle",
            Self::NextStart => "At its next start",
        }
    }
}

/// The most socket grants one project's policy holds.
pub const GRANT_SOCKETS_MAX: usize = 2;
/// The most port grants one project's policy holds.
pub const GRANT_PORTS_MAX: usize = 4;
/// The most grants of every kind one project's policy holds: its sockets, its ports and
/// the temp folder.
pub const GRANTS_MAX: usize = GRANT_SOCKETS_MAX + GRANT_PORTS_MAX + 1;
/// The longest socket path a grant names, macOS `sun_path` less its closing NUL.
pub const GRANT_SOCKET_BYTES_MAX: usize = 103;

/// One narrow extra the owner allowed a project's agents beside their access level (F08):
/// connecting to one exact Unix socket, connecting to one TCP port on this computer, or
/// using the owner's own temp folder as their TMPDIR. A grant is added in Agent access or by
/// the owner's Allow on an agent's request, and removed there.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AccessGrant {
    Socket { path: std::path::PathBuf },
    Port { port: u16 },
    TempFolder,
}

impl AccessGrant {
    /// Tells whether the grant has a plain shape: a socket path absolute, at most
    /// [`GRANT_SOCKET_BYTES_MAX`] bytes of letters, digits, `/`, `.`, `-` and `_` with no
    /// empty, `.` or `..` part, and a port other than 0. Input: the grant. Output: the answer.
    pub fn plain(&self) -> bool {
        match self {
            Self::Socket { path } => {
                let Some(text) = path.to_str() else {
                    return false;
                };
                path.is_absolute()
                    && text.len() <= GRANT_SOCKET_BYTES_MAX
                    && !text.ends_with('/')
                    && !text.contains("//")
                    && text
                        .split('/')
                        .skip(1)
                        .all(|part| part != "." && part != "..")
                    && text.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'-' | b'_')
                    })
            }
            Self::Port { port } => *port != 0,
            Self::TempFolder => true,
        }
    }

    /// The grant as the owner reads it. Input: the grant. Output: the words.
    pub fn label(&self) -> String {
        match self {
            Self::Socket { path } => format!("Socket {}", path.display()),
            Self::Port { port } => format!("Port {port} on this computer"),
            Self::TempFolder => "Your temp folder".to_owned(),
        }
    }
}

/// The owner's choice for one toolchain family; a family with no choice follows the
/// project (on when the project uses it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolchainChoice {
    pub toolchain: Toolchain,
    pub allowed: bool,
}

/// One project's agent policy as the owner saved it: the access level and the toolchain
/// choices the owner made, which apply to the master, sub-agents and research jobs alike,
/// and whether git in the IDE runs the project's own hooks in the Project section (Off
/// unless the owner turned it on; a policy saved before the setting existed reads Off), what
/// the IDE may do to agent files (Allow all changes when none was saved) and who shares the
/// agents' tool cache (T8; a policy saved before that setting reads Shared), what a launch
/// does with the download caches in an agent home (DEC-173 add.1; a policy saved before that
/// setting reads Clear when access is raised), when running
/// agents pick up a wider policy and the narrow grants the owner allowed (F08; a policy saved
/// before those settings reads When the agent is idle and no grants), and which of the
/// project folder's local folders agents see and how (DEC-147.9; a policy saved before that
/// setting reads All and Read-only).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPolicy {
    pub access: AgentAccess,
    pub toolchains: Vec<ToolchainChoice>,
    #[serde(default)]
    pub ide_git_hooks: bool,
    #[serde(default)]
    pub network: AgentNetwork,
    #[serde(default)]
    pub tool_cache: ToolCacheScope,
    #[serde(default)]
    pub home_caches: HomeCaches,
    #[serde(default)]
    pub ide_agent_files: IdeAgentFiles,
    #[serde(default)]
    pub apply_changes: AccessApply,
    #[serde(default)]
    pub grants: Vec<AccessGrant>,
    #[serde(default)]
    pub shared_folders: SharedFolders,
}

impl AgentPolicy {
    /// Checks a policy a client sent: each toolchain at most once, a shared folders choice
    /// in shape ([`SharedFolders::validate`]), and at most [`GRANT_SOCKETS_MAX`] sockets,
    /// [`GRANT_PORTS_MAX`] ports and one temp folder, each plain and named once. Takes the
    /// policy. Returns a plain refusal code.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.toolchains.len() > Toolchain::ALL.len() {
            return Err("agent_policy_invalid");
        }
        self.shared_folders.validate()?;
        for (index, choice) in self.toolchains.iter().enumerate() {
            if self.toolchains[..index]
                .iter()
                .any(|earlier| earlier.toolchain == choice.toolchain)
            {
                return Err("agent_policy_invalid");
            }
        }
        if self.grants.len() > GRANTS_MAX {
            return Err("agent_grants_invalid");
        }
        let count = |kind: fn(&AccessGrant) -> bool| self.grants.iter().filter(|g| kind(g)).count();
        let sockets = count(|grant| matches!(grant, AccessGrant::Socket { .. }));
        let ports = count(|grant| matches!(grant, AccessGrant::Port { .. }));
        let temp = count(|grant| matches!(grant, AccessGrant::TempFolder));
        if sockets > GRANT_SOCKETS_MAX || ports > GRANT_PORTS_MAX || temp > 1 {
            return Err("agent_grants_invalid");
        }
        for (index, grant) in self.grants.iter().enumerate() {
            if !grant.plain() || self.grants[..index].contains(grant) {
                return Err("agent_grants_invalid");
            }
        }
        assert_eq!(sockets + ports + temp, self.grants.len());
        Ok(())
    }

    /// The toolchains this project's agents may read: each family the owner allowed, plus
    /// each family without a choice that the project uses. Input: the project folder.
    /// Output: the toolchains in display order.
    pub fn allowed(&self, project: &Path) -> Vec<Toolchain> {
        Toolchain::ALL
            .into_iter()
            .filter(|toolchain| {
                self.choice(*toolchain)
                    .unwrap_or_else(|| toolchain.used_by(project))
            })
            .collect()
    }

    /// The same policy with every family decided as it is for the project now, so a launch
    /// that reads it again grants exactly the toolchains this read gave. Input: the project
    /// folder. Output: the pinned policy.
    pub fn pinned(&self, project: &Path) -> Self {
        let allowed = self.allowed(project);
        let pinned = Self {
            access: self.access,
            toolchains: Toolchain::ALL
                .into_iter()
                .map(|toolchain| ToolchainChoice {
                    toolchain,
                    allowed: allowed.contains(&toolchain),
                })
                .collect(),
            ide_git_hooks: self.ide_git_hooks,
            network: self.network,
            tool_cache: self.tool_cache,
            home_caches: self.home_caches,
            ide_agent_files: self.ide_agent_files,
            apply_changes: self.apply_changes,
            grants: self.grants.clone(),
            shared_folders: self.shared_folders.clone(),
        };
        assert!(pinned.validate().is_ok());
        pinned
    }

    /// What a client shows: the level, every level with its availability, each toolchain
    /// family the owner has installed, the project uses or the owner chose, the IDE hooks
    /// and agent files choices, the tool cache choice with every choice, the shared folders
    /// choice with the top-level folders it can pick, and the policy's version, which a save
    /// names. Inputs: the project folder, the owner's tool homes, the version, the owner's
    /// sandbox settings, none when unreadable, and whether the project may hold another git
    /// repository ([`LevelView::other_git`]). Output: the view.
    pub fn view(
        &self,
        project: &Path,
        homes: &ToolHomes,
        version: u64,
        settings: Option<crate::sandbox_settings::SandboxSettings>,
        other_git: bool,
    ) -> PolicyView {
        let allowed = self.allowed(project);
        let toolchains: Vec<ToolchainView> = Toolchain::ALL
            .into_iter()
            .map(|toolchain| ToolchainView {
                toolchain,
                label: toolchain.label().to_owned(),
                installed: toolchain.installed(homes),
                used: toolchain.used_by(project),
                allowed: allowed.contains(&toolchain),
            })
            .filter(|row| row.installed || row.used || self.choice(row.toolchain).is_some())
            .collect();
        assert!(toolchains.len() <= Toolchain::ALL.len());
        let calls = settings.map(|settings| settings.linux_supervisor);
        let host = crate::sandbox::linux_host_kept().map(|host| host.under(calls));
        let sockets = settings.map(|settings| settings.linux_sockets);
        PolicyView {
            access: self.access,
            network: self.network,
            levels: LevelView::on(host, other_git),
            networks: AgentNetwork::ALL
                .into_iter()
                .map(|network| NetworkView {
                    network,
                    label: network.label().to_owned(),
                    gap: linux_network_gap(network, host, sockets, &self.grants),
                })
                .collect(),
            toolchains,
            version,
            ide_git_hooks: self.ide_git_hooks,
            tool_cache: self.tool_cache,
            tool_caches: ToolCacheScope::ALL
                .into_iter()
                .map(|scope| ToolCacheView {
                    scope,
                    label: scope.label().to_owned(),
                })
                .collect(),
            home_caches: self.home_caches,
            home_cache_choices: HomeCaches::ALL
                .into_iter()
                .map(|choice| HomeCachesView {
                    choice,
                    label: choice.label().to_owned(),
                })
                .collect(),
            ide_agent_files: self.ide_agent_files,
            apply_changes: self.apply_changes,
            apply_choices: AccessApply::ALL
                .into_iter()
                .map(|apply| ApplyView {
                    apply,
                    label: apply.label().to_owned(),
                })
                .collect(),
            grants: self
                .grants
                .iter()
                .map(|grant| GrantView {
                    grant: grant.clone(),
                    label: grant.label(),
                })
                .collect(),
            temp_folder: match super::grants::owner_temp_folder() {
                Ok(folder) => TempFolderView {
                    path: Some(folder.display().to_string()),
                    unavailable: None,
                },
                Err(code) => TempFolderView {
                    path: None,
                    unavailable: Some(super::grants::refusal_text(code).to_owned()),
                },
            },
            grant_log: Vec::new(),
            shared_folders: self.shared_folders.clone(),
            local_folders: super::shared_folders::local_folders(project),
        }
    }

    /// Tells whether this saved policy gives a running agent more than it started with: a
    /// wider level, a wider network for an agent with tools, a toolchain its profile lacks
    /// or a grant it does not hold. A level or network of none is not compared, and a
    /// toolchain list of none counts as every family, so a record from before the engine
    /// kept them never asks for a restart. Inputs: the project folder, none when it cannot
    /// be read, the level and network the agent runs with, the toolchains and the grants its
    /// profile holds. Output: true when a restart would give it more.
    pub fn widens(
        &self,
        project: Option<&Path>,
        access: Option<AgentAccess>,
        network: Option<AgentNetwork>,
        granted: Option<&[Toolchain]>,
        grants: &[AccessGrant],
    ) -> bool {
        assert!(grants.len() <= GRANTS_MAX);
        let tools = match (project, granted) {
            (Some(project), Some(granted)) => self
                .allowed(project)
                .iter()
                .any(|toolchain| !granted.contains(toolchain)),
            _ => false,
        };
        access.is_some_and(|access| self.access.wider_than(access))
            || network.is_some_and(|network| self.network.wider_than(network))
            || tools
            || self.grants.iter().any(|grant| !grants.contains(grant))
    }

    /// Tells whether a running agent holds a grant this saved policy no longer has, which
    /// stops it at once as a lowered level does. Input: the grants its profile holds.
    /// Output: the answer.
    pub fn revokes(&self, grants: &[AccessGrant]) -> bool {
        grants.iter().any(|grant| !self.grants.contains(grant))
    }

    fn choice(&self, toolchain: Toolchain) -> Option<bool> {
        self.toolchains
            .iter()
            .find(|choice| choice.toolchain == toolchain)
            .map(|choice| choice.allowed)
    }
}

/// Why a process that started under a project's agent policy holds more than the policy
/// the owner saved now allows: a wider access level; a profile with tools made for a wider
/// network (CS-2); a toolchain the owner turned off; a toolchain with no owner choice that
/// the project no longer uses, which is also how
/// turning off a family the project does not use is saved; or a policy, or a project folder
/// that toolchain detection needs, that cannot be read, so nothing held can be confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Narrowing {
    Access,
    Network,
    TurnedOff,
    NotAllowed,
    PolicyUnreadable,
    FolderUnreadable,
}

impl Narrowing {
    /// The refusal code a process stopped for this cause reports. Input: the cause.
    /// Output: the code.
    pub fn code(self) -> &'static str {
        match self {
            Self::Access => "agent_access_lowered",
            Self::Network => "agent_network_narrowed",
            Self::TurnedOff | Self::NotAllowed => "agent_toolchain_lowered",
            Self::PolicyUnreadable => "agent_policy_unreadable",
            Self::FolderUnreadable => "agent_project_unreadable",
        }
    }
}

/// Tells whether a process that started at an access level and network with some
/// toolchains granted holds more than the saved policy allows now. Inputs: the saved
/// policy, none when it cannot be read; the project folder, none when it cannot be
/// resolved; the level the process runs at, none when it reads the level again before it
/// next runs anything; the network its profile was made for, none for a process without
/// tools, whose profile has no network rule of the policy's; and the toolchains its sandbox
/// profile grants. Output: why it must stop, or none.
pub fn narrowing(
    saved: Option<&AgentPolicy>,
    project: Option<&Path>,
    access: Option<AgentAccess>,
    network: Option<AgentNetwork>,
    granted: &[Toolchain],
) -> Option<Narrowing> {
    assert!(granted.len() <= Toolchain::ALL.len());
    let Some(saved) = saved else {
        let holds = !granted.is_empty()
            || access.is_some_and(|access| access.wider_than(AgentAccess::Sandboxed))
            || network.is_some_and(|network| network.wider_than(AgentNetwork::None));
        return holds.then_some(Narrowing::PolicyUnreadable);
    };
    if access.is_some_and(|access| access.wider_than(saved.access)) {
        return Some(Narrowing::Access);
    }
    if network.is_some_and(|network| network.wider_than(saved.network)) {
        return Some(Narrowing::Network);
    }
    if granted
        .iter()
        .any(|toolchain| saved.choice(*toolchain) == Some(false))
    {
        return Some(Narrowing::TurnedOff);
    }
    let detected: Vec<Toolchain> = granted
        .iter()
        .copied()
        .filter(|toolchain| saved.choice(*toolchain).is_none())
        .collect();
    if detected.is_empty() {
        return None;
    }
    let Some(project) = project else {
        return Some(Narrowing::FolderUnreadable);
    };
    detected
        .iter()
        .any(|toolchain| !toolchain.used_by(project))
        .then_some(Narrowing::NotAllowed)
}

/// One project's agent policy as a client shows it, with the version a save names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyView {
    pub access: AgentAccess,
    pub network: AgentNetwork,
    pub levels: Vec<LevelView>,
    pub networks: Vec<NetworkView>,
    pub toolchains: Vec<ToolchainView>,
    pub version: u64,
    pub ide_git_hooks: bool,
    #[serde(default)]
    pub tool_cache: ToolCacheScope,
    #[serde(default)]
    pub tool_caches: Vec<ToolCacheView>,
    #[serde(default)]
    pub home_caches: HomeCaches,
    #[serde(default)]
    pub home_cache_choices: Vec<HomeCachesView>,
    pub ide_agent_files: IdeAgentFiles,
    #[serde(default)]
    pub apply_changes: AccessApply,
    #[serde(default)]
    pub apply_choices: Vec<ApplyView>,
    #[serde(default)]
    pub grants: Vec<GrantView>,
    #[serde(default)]
    pub temp_folder: TempFolderView,
    #[serde(default)]
    pub grant_log: Vec<super::grants::LogView>,
    #[serde(default)]
    pub shared_folders: SharedFolders,
    #[serde(default)]
    pub local_folders: Vec<String>,
}

/// One Apply access changes choice with the name shown to the owner (F08).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyView {
    pub apply: AccessApply,
    pub label: String,
}

/// One saved grant with the words the owner reads (F08).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantView {
    pub grant: AccessGrant,
    pub label: String,
}

/// The owner's temp folder a temp folder grant would hand to agents, or why this computer
/// cannot grant it (F08).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TempFolderView {
    pub path: Option<String>,
    pub unavailable: Option<String>,
}

/// One tool cache choice with the name shown to the owner (T8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCacheView {
    pub scope: ToolCacheScope,
    pub label: String,
}

/// One agent home caches choice with the name shown to the owner (DEC-173 add.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeCachesView {
    pub choice: HomeCaches,
    pub label: String,
}

/// One network choice with the name shown to the owner and, on Linux, the line that says
/// what the choice cannot hold on this computer ([`linux_network_gap`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkView {
    pub network: AgentNetwork,
    pub label: String,
    #[serde(default)]
    pub gap: Option<String>,
}

/// What a network choice cannot hold on this Linux, for the Agent access page: a port agents
/// may reach on this computer is open on every host. Under `TcpOnly` the call supervisor
/// keeps those ports to this computer, though Https reaches port 53 on any host; without it
/// a nesting host's bridge does under None, unless a granted port is below its lowest; a
/// host that routes keeps Https to its ports in a network of its own (DEC-179 SBX-2).
/// Inputs: the choice, the host under the setting, sockets, grants. Output: the line, or none.
pub fn linux_network_gap(
    network: AgentNetwork,
    host: Option<crate::sandbox::LinuxHost>,
    sockets: Option<crate::sandbox::LinuxSockets>,
    grants: &[AccessGrant],
) -> Option<String> {
    let host = host?;
    let tcp = sockets == Some(crate::sandbox::LinuxSockets::TcpOnly);
    let low = grants.iter().any(|grant| {
        matches!(grant, AccessGrant::Port { port } if *port < crate::sandbox::BRIDGE_PORT_MIN)
    });
    if host.routed && tcp && network == AgentNetwork::Https && !low {
        assert!(host.nested);
        return None;
    }
    if host.supervised && tcp && network != AgentNetwork::Open {
        return (network == AgentNetwork::Https)
            .then(|| crate::sandbox::LINUX_DNS_ANY_HOST_LINE.to_owned());
    }
    let bridges = network == AgentNetwork::None && host.nested && tcp;
    if !bridges {
        return Some(crate::sandbox::LINUX_PORTS_ANY_HOST_LINE.to_owned());
    }
    grants
        .iter()
        .filter_map(|grant| match grant {
            AccessGrant::Port { port } if *port < crate::sandbox::BRIDGE_PORT_MIN => Some(*port),
            _ => None,
        })
        .min()
        .map(crate::sandbox::linux_unbridged_line)
}

/// One access level with the reason it cannot run here, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevelView {
    pub access: AgentAccess,
    pub label: String,
    pub unavailable: Option<String>,
    #[serde(default)]
    pub gaps: Vec<String>,
}

impl LevelView {
    /// Every access level for the New Project catalog, from the most to the least
    /// confined, each with the reason it cannot run agents on this computer, from what
    /// [`LevelView::refresh`] last read of it under the owner's call supervisor setting; a
    /// repository may be anywhere in a project not made yet. Input: the setting, none when
    /// unreadable. Output: one view for each level.
    pub fn all(calls: Option<crate::sandbox::LinuxSupervisor>) -> Vec<Self> {
        Self::on(
            crate::sandbox::linux_host_kept().map(|host| host.under(calls)),
            true,
        )
    }

    /// Reads again what this computer holds once it is a minute old, which on Linux may
    /// start the sandbox helper and bubblewrap. A reader calls it before it opens its
    /// database transaction, so [`LevelView::all`] inside never waits on a program.
    /// Takes nothing. Returns nothing.
    pub fn refresh() {
        crate::sandbox::linux_host();
    }

    /// Every access level as [`LevelView::all`] gives them, for a computer that holds
    /// `host`. Inputs: what a Linux holds under the setting, none on another system, and
    /// whether the project may hold another git repository ([`LevelView::other_git`]), read
    /// before any transaction. Output: one view for each level, with its [`linux_gaps`].
    pub fn on(host: Option<crate::sandbox::LinuxHost>, other_git: bool) -> Vec<Self> {
        let levels: Vec<Self> = AgentAccess::ALL
            .into_iter()
            .map(|access| Self {
                access,
                label: access.label().to_owned(),
                unavailable: access.unavailable().map(str::to_owned),
                gaps: linux_gaps(access, host, other_git)
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            })
            .collect();
        assert_eq!(levels.len(), AgentAccess::ALL.len());
        levels
    }

    /// Tells whether a project may hold a git repository besides its own, as its Agent
    /// access lines need where the call supervisor runs: a bounded read of its folder
    /// ([`crate::sandbox::linux_other_git`]), made only for a supervised host. A reader
    /// calls it before its database transaction, so that read never holds one. Inputs:
    /// what this Linux holds under the setting, none elsewhere, and the folder.
    pub fn other_git(host: Option<crate::sandbox::LinuxHost>, project: &Path) -> bool {
        host.is_some_and(|host| host.supervised) && crate::sandbox::linux_other_git(project)
    }
}

/// What a Linux cannot hold for agents at one access level, as plain lines both clients
/// show under it: why root runs no supervisor; nested git, or under the supervisor its
/// narrower line where another repository may be and what it changes; then, where agents
/// share the process namespace, why AppArmor keeps it shared when it does, and the process
/// list, or signals too. Inputs: the level, the host under the setting and whether the
/// project may hold another repository.
pub fn linux_gaps(
    access: AgentAccess,
    host: Option<crate::sandbox::LinuxHost>,
    other_git: bool,
) -> Vec<&'static str> {
    let Some(host) = host else {
        return Vec::new();
    };
    assert!(!(host.root && host.supervised));
    assert!(!(host.nested && host.armored));
    if access == AgentAccess::Full {
        return Vec::new();
    }
    let mut lines = Vec::with_capacity(4);
    if host.root {
        lines.push(crate::sandbox::LINUX_ROOT_LINE);
    }
    if host.supervised {
        if other_git {
            lines.push(crate::sandbox::LINUX_KEPT_GIT_LINE);
        }
        lines.push(crate::sandbox::LINUX_SUPERVISED_LINE);
    } else {
        lines.push(crate::sandbox::LINUX_NESTED_GIT_LINE);
    }
    if host.armored && !host.root {
        lines.push(crate::sandbox::LINUX_APPARMOR_LINE);
    }
    if !host.nested && (host.scoped || host.supervised) {
        lines.push(crate::sandbox::LINUX_PROCESS_LIST_LINE);
    } else if !host.nested {
        lines.push(crate::sandbox::LINUX_SIGNALS_LINE);
    }
    assert!((1..=4).contains(&lines.len()));
    lines
}

/// One toolchain family: whether the owner has it, the project uses it and agents may
/// read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolchainView {
    pub toolchain: Toolchain,
    pub label: String,
    pub installed: bool,
    pub used: bool,
    pub allowed: bool,
}

/// The toolchain choices a project records when it is made (F26): every family the project
/// uses now or the owner has installed, allowed, so a project that starts empty still gets
/// the owner's Rust, Node.js and Python once its code lands; a family neither used nor
/// installed keeps following the project. The owner can turn each one off later. Inputs:
/// the project folder and the owner's tool homes, none when unknown. Output: the choices in
/// display order.
pub fn recorded_toolchains(project: &Path, homes: Option<&ToolHomes>) -> Vec<ToolchainChoice> {
    let recorded: Vec<ToolchainChoice> = Toolchain::ALL
        .into_iter()
        .filter(|toolchain| {
            toolchain.used_by(project) || homes.is_some_and(|homes| toolchain.installed(homes))
        })
        .map(|toolchain| ToolchainChoice {
            toolchain,
            allowed: true,
        })
        .collect();
    assert!(recorded.len() <= Toolchain::ALL.len());
    recorded
}

/// Whether a project's agent policy was ever saved. Inputs: the open connection or
/// transaction and the project id. Output: the answer, or the database error.
pub fn saved(connection: &Connection, project: &str) -> rusqlite::Result<bool> {
    connection.execute(TABLE, [])?;
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM project_agent_policy WHERE project=?1)",
        [project],
        |row| row.get(0),
    )
}

/// Replaces a project's agent policy. Inputs: the open connection or transaction, the
/// project id and a validated policy. Output: done, or the database error.
pub fn save(connection: &Connection, project: &str, policy: &AgentPolicy) -> rusqlite::Result<()> {
    assert!(policy.validate().is_ok());
    let body = serde_json::to_string(policy)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    assert!(body.len() <= BODY_BYTES_MAX);
    connection.execute(TABLE, [])?;
    connection.execute(
        "INSERT INTO project_agent_policy (project, body) VALUES (?1, ?2)
         ON CONFLICT(project) DO UPDATE SET body=excluded.body",
        params![project, body],
    )?;
    Ok(())
}

/// Reads a project's agent policy; the default (Sandboxed, no toolchain choice) when none
/// was ever saved. Inputs: the open connection or transaction and the project id. Output:
/// the policy, or the database error for an unreadable row.
pub fn load(connection: &Connection, project: &str) -> rusqlite::Result<AgentPolicy> {
    let present: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='project_agent_policy')",
        [],
        |row| row.get(0),
    )?;
    if !present {
        return Ok(AgentPolicy::default());
    }
    let body: Option<String> = connection
        .query_row(
            "SELECT body FROM project_agent_policy WHERE project=?1",
            [project],
            |row| row.get(0),
        )
        .optional()?;
    let Some(body) = body else {
        return Ok(AgentPolicy::default());
    };
    let unreadable = |error: Box<dyn std::error::Error + Send + Sync>| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, error)
    };
    if body.len() > BODY_BYTES_MAX {
        return Err(unreadable("agent policy too long".into()));
    }
    let policy: AgentPolicy =
        serde_json::from_str(&body).map_err(|error| unreadable(Box::new(error)))?;
    policy.validate().map_err(|code| unreadable(code.into()))?;
    Ok(policy)
}

/// Deletes a project's agent policy when the project is removed. Inputs: the open
/// connection or transaction and the project id. Output: done, or the database error.
pub fn remove(connection: &Connection, project: &str) -> rusqlite::Result<()> {
    connection.execute(TABLE, [])?;
    connection.execute(
        "DELETE FROM project_agent_policy WHERE project=?1",
        [project],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
