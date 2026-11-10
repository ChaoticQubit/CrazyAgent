use super::toolchains::{ToolHomes, Toolchain};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

const TABLE: &str = "CREATE TABLE IF NOT EXISTS project_agent_policy (
    project TEXT PRIMARY KEY,
    body TEXT NOT NULL
)";

const BODY_BYTES_MAX: usize = 4096;

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

/// How far a sandboxed IDE server or terminal at a level reaches: nothing at Sandboxed, and
/// the project's network choice at Own branch through [`AgentNetwork::sandbox`], with no
/// model port. A Full access process has no Seatbelt for a choice to limit, so it never
/// asks. Inputs: the level and the choice. Output: the sandbox's network.
pub fn ide_network(level: AgentAccess, choice: AgentNetwork) -> super::SandboxNetwork {
    match level {
        AgentAccess::OwnBranch => super::SandboxNetwork::Only(Vec::new()),
        AgentAccess::Sandboxed | AgentAccess::Full => choice.sandbox(None),
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
/// unless the owner turned it on; a policy saved before the setting existed reads Off).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPolicy {
    pub access: AgentAccess,
    pub toolchains: Vec<ToolchainChoice>,
    #[serde(default)]
    pub ide_git_hooks: bool,
    #[serde(default)]
    pub network: AgentNetwork,
}

impl AgentPolicy {
    /// Checks a policy a client sent: each toolchain at most once. Takes the policy.
    /// Returns a plain refusal code.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.toolchains.len() > Toolchain::ALL.len() {
            return Err("agent_policy_invalid");
        }
        for (index, choice) in self.toolchains.iter().enumerate() {
            if self.toolchains[..index]
                .iter()
                .any(|earlier| earlier.toolchain == choice.toolchain)
            {
                return Err("agent_policy_invalid");
            }
        }
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
        };
        assert!(pinned.validate().is_ok());
        pinned
    }

    /// What a client shows: the level, every level with its availability, each toolchain
    /// family the owner has installed, the project uses or the owner chose, the IDE hooks
    /// choice and the policy's version, which a save names. Inputs: the project folder, the
    /// owner's tool homes and the version. Output: the view.
    pub fn view(&self, project: &Path, homes: &ToolHomes, version: u64) -> PolicyView {
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
        PolicyView {
            access: self.access,
            network: self.network,
            levels: AgentAccess::ALL
                .into_iter()
                .map(|access| LevelView {
                    access,
                    label: access.label().to_owned(),
                    unavailable: access.unavailable().map(str::to_owned),
                })
                .collect(),
            networks: AgentNetwork::ALL
                .into_iter()
                .map(|network| NetworkView {
                    network,
                    label: network.label().to_owned(),
                })
                .collect(),
            toolchains,
            version,
            ide_git_hooks: self.ide_git_hooks,
        }
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
}

/// One network choice with the name shown to the owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkView {
    pub network: AgentNetwork,
    pub label: String,
}

/// One access level with the reason it cannot run here, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevelView {
    pub access: AgentAccess,
    pub label: String,
    pub unavailable: Option<String>,
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
mod tests {
    use super::*;

    #[test]
    fn no_network_grants_a_model_only_on_loopback_and_never_a_host_port() {
        use crate::sandbox::{SandboxNetwork, SandboxRemote};
        assert_eq!(
            AgentNetwork::None.sandbox(Some(SandboxRemote::AnyHost(443))),
            SandboxNetwork::Only(vec![])
        );
        assert_eq!(
            AgentNetwork::None.sandbox(Some(SandboxRemote::Loopback(11_434))),
            SandboxNetwork::Only(vec![SandboxRemote::Loopback(11_434)])
        );
        assert_eq!(
            AgentNetwork::None.sandbox(None),
            SandboxNetwork::Only(vec![])
        );
        assert_eq!(
            AgentNetwork::Https.sandbox(Some(SandboxRemote::AnyHost(8443))),
            SandboxNetwork::Only(vec![
                SandboxRemote::AnyHost(443),
                SandboxRemote::AnyHost(8443)
            ])
        );
        assert_eq!(AgentNetwork::Open.sandbox(None), SandboxNetwork::Open);
    }

    #[test]
    fn a_network_choice_is_wider_only_than_the_narrower_ones() {
        let (open, https, none) = (AgentNetwork::Open, AgentNetwork::Https, AgentNetwork::None);
        assert!(open.wider_than(https) && open.wider_than(none) && https.wider_than(none));
        for (one, other) in [
            (https, open),
            (none, open),
            (none, https),
            (open, open),
            (https, https),
            (none, none),
        ] {
            assert!(
                !one.wider_than(other),
                "{one:?} is not wider than {other:?}"
            );
        }
    }

    #[test]
    fn each_platform_offers_only_the_levels_it_can_enforce() {
        let offered: Vec<AgentAccess> = AgentAccess::ALL
            .into_iter()
            .filter(|access| access.unavailable().is_none())
            .collect();
        #[cfg(target_os = "windows")]
        {
            assert!(offered.is_empty());
            assert!(
                AgentAccess::ALL
                    .iter()
                    .all(|access| access.unavailable() == Some(WINDOWS_UNAVAILABLE))
            );
        }
        #[cfg(target_os = "linux")]
        assert_eq!(offered, [AgentAccess::Sandboxed]);
        #[cfg(target_os = "macos")]
        assert_eq!(offered, AgentAccess::ALL);
        assert_eq!(AgentAccess::default(), AgentAccess::Sandboxed);
        assert!(AgentAccess::Full.wider_than(AgentAccess::OwnBranch));
        assert!(AgentAccess::OwnBranch.wider_than(AgentAccess::Sandboxed));
        assert!(!AgentAccess::Sandboxed.wider_than(AgentAccess::Full));
        assert!(!AgentAccess::Full.wider_than(AgentAccess::Full));
        assert_eq!(AgentAccess::NEW_PROJECT, AgentAccess::OwnBranch);
    }

    #[test]
    fn a_running_process_is_narrowed_only_by_what_the_saved_policy_no_longer_allows() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("Cargo.toml"), "[package]\n").unwrap();
        let root = Some(project.path());
        let rust = [Toolchain::Rust];
        let chose = |access, choices: &[(Toolchain, bool)]| AgentPolicy {
            access,
            toolchains: choices
                .iter()
                .map(|(toolchain, allowed)| ToolchainChoice {
                    toolchain: *toolchain,
                    allowed: *allowed,
                })
                .collect(),
            ide_git_hooks: false,
            network: AgentNetwork::default(),
        };
        let sandboxed = chose(AgentAccess::Sandboxed, &[]);
        let full = Some(AgentAccess::Full);
        assert_eq!(narrowing(Some(&sandboxed), root, None, None, &rust), None);
        assert_eq!(
            narrowing(Some(&sandboxed), root, full, None, &rust),
            Some(Narrowing::Access)
        );
        assert_eq!(
            narrowing(
                Some(&chose(AgentAccess::Full, &[])),
                root,
                full,
                None,
                &rust
            ),
            None
        );
        let off = chose(AgentAccess::Sandboxed, &[(Toolchain::Rust, false)]);
        assert_eq!(
            narrowing(Some(&off), None, None, None, &rust),
            Some(Narrowing::TurnedOff)
        );
        let node = [Toolchain::Node];
        assert_eq!(
            narrowing(Some(&sandboxed), root, None, None, &node),
            Some(Narrowing::NotAllowed),
            "an unused family turned off leaves no choice behind"
        );
        let on = chose(AgentAccess::Sandboxed, &[(Toolchain::Node, true)]);
        assert_eq!(narrowing(Some(&on), None, None, None, &node), None);
        assert_eq!(
            narrowing(Some(&sandboxed), None, None, None, &rust),
            Some(Narrowing::FolderUnreadable)
        );
        assert_eq!(narrowing(Some(&sandboxed), None, None, None, &[]), None);
        assert_eq!(
            narrowing(None, root, None, None, &rust),
            Some(Narrowing::PolicyUnreadable)
        );
        assert_eq!(
            narrowing(None, root, Some(AgentAccess::OwnBranch), None, &[]),
            Some(Narrowing::PolicyUnreadable)
        );
        assert_eq!(
            narrowing(None, root, Some(AgentAccess::Sandboxed), None, &[]),
            None
        );
        assert_eq!(Narrowing::NotAllowed.code(), "agent_toolchain_lowered");
        let closed = AgentPolicy {
            network: AgentNetwork::None,
            ..sandboxed.clone()
        };
        let open = Some(AgentNetwork::Open);
        assert_eq!(
            narrowing(Some(&closed), root, None, open, &rust),
            Some(Narrowing::Network)
        );
        assert_eq!(
            narrowing(Some(&closed), root, full, open, &rust),
            Some(Narrowing::Access),
            "a wider level is named before a wider network"
        );
        assert_eq!(
            narrowing(Some(&closed), root, None, Some(AgentNetwork::None), &rust),
            None
        );
        assert_eq!(
            narrowing(Some(&closed), root, None, None, &rust),
            None,
            "a process without tools has no network rule to narrow"
        );
        assert_eq!(narrowing(Some(&sandboxed), root, None, open, &rust), None);
        assert_eq!(
            narrowing(Some(&closed), None, None, open, &rust),
            Some(Narrowing::Network),
            "a wider network is named before an unreadable folder"
        );
        assert_eq!(
            narrowing(None, root, None, open, &[]),
            Some(Narrowing::PolicyUnreadable)
        );
        assert_eq!(
            narrowing(None, root, None, Some(AgentNetwork::None), &[]),
            None
        );
        assert_eq!(Narrowing::Network.code(), "agent_network_narrowed");

        let pinned = sandboxed.pinned(project.path());
        assert_eq!(pinned.allowed(project.path()), rust);
        std::fs::remove_file(project.path().join("Cargo.toml")).unwrap();
        assert_eq!(pinned.allowed(project.path()), rust);
        assert!(sandboxed.allowed(project.path()).is_empty());
        assert_eq!(narrowing(Some(&pinned), None, None, None, &rust), None);
    }

    #[test]
    fn a_saved_policy_round_trips_and_a_missing_or_tampered_one_is_never_wider() {
        let connection = Connection::open_in_memory().unwrap();
        assert_eq!(load(&connection, "p").unwrap(), AgentPolicy::default());
        let policy = AgentPolicy {
            access: AgentAccess::Full,
            network: AgentNetwork::default(),
            toolchains: vec![ToolchainChoice {
                toolchain: Toolchain::Python,
                allowed: false,
            }],
            ide_git_hooks: true,
        };
        save(&connection, "p", &policy).unwrap();
        assert_eq!(load(&connection, "p").unwrap(), policy);
        let view = policy.view(
            Path::new("/nonexistent"),
            &ToolHomes::at(Path::new("/nonexistent")),
            0,
        );
        assert!(view.ide_git_hooks);
        connection
            .execute(
                "UPDATE project_agent_policy SET body=?1 WHERE project='p'",
                [r#"{"access":"full","toolchains":[]}"#],
            )
            .unwrap();
        let old = load(&connection, "p").unwrap();
        assert_eq!(old.access, AgentAccess::Full);
        assert!(!old.ide_git_hooks);
        assert!(!AgentPolicy::default().ide_git_hooks);
        assert_eq!(load(&connection, "other").unwrap(), AgentPolicy::default());
        for body in [
            r#"{"access":"root","toolchains":[]}"#.to_owned(),
            r#"{"access":"full","toolchains":[],"extra":1}"#.to_owned(),
            r#"{"access":"full","toolchains":[],"ide_git_hooks":"yes"}"#.to_owned(),
            r#"{"access":"full","toolchains":[{"toolchain":"node","allowed":true},{"toolchain":"node","allowed":false}]}"#.to_owned(),
            format!(r#"{{"access":"full","toolchains":[],"pad":"{}"}}"#, "x".repeat(BODY_BYTES_MAX)),
        ] {
            connection
                .execute(
                    "UPDATE project_agent_policy SET body=?1 WHERE project='p'",
                    [&body],
                )
                .unwrap();
            assert!(load(&connection, "p").is_err(), "{body}");
        }
        remove(&connection, "p").unwrap();
        assert_eq!(load(&connection, "p").unwrap(), AgentPolicy::default());
    }

    #[test]
    fn a_saved_network_choice_reads_back_and_defaults_to_open() {
        let connection = Connection::open_in_memory().unwrap();
        assert_eq!(AgentNetwork::default(), AgentNetwork::Open);
        assert_eq!(load(&connection, "p").unwrap().network, AgentNetwork::Open);
        let mut policy = AgentPolicy::default();
        for choice in AgentNetwork::ALL {
            policy.network = choice;
            save(&connection, "p", &policy).unwrap();
            assert_eq!(load(&connection, "p").unwrap().network, choice);
        }
        connection.execute(TABLE, []).unwrap();
        connection
            .execute(
                "INSERT INTO project_agent_policy (project, body) VALUES ('old', ?1)
                 ON CONFLICT(project) DO UPDATE SET body=excluded.body",
                [r#"{"access":"own_branch","toolchains":[]}"#],
            )
            .unwrap();
        let old = load(&connection, "old").unwrap();
        assert_eq!(old.access, AgentAccess::OwnBranch);
        assert_eq!(old.network, AgentNetwork::Open);
    }
}
