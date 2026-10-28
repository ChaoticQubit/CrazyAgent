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

/// The owner's choice for one toolchain family; a family with no choice follows the
/// project (on when the project uses it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolchainChoice {
    pub toolchain: Toolchain,
    pub allowed: bool,
}

/// One project's agent policy as the owner saved it: the access level and the toolchain
/// choices the owner made. It applies to the master, sub-agents and research jobs alike.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPolicy {
    pub access: AgentAccess,
    pub toolchains: Vec<ToolchainChoice>,
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

    /// What a client shows: the level, every level with its availability, and each
    /// toolchain family the owner has installed, the project uses or the owner chose.
    /// Inputs: the project folder and the owner's tool homes. Output: the view.
    pub fn view(&self, project: &Path, homes: &ToolHomes) -> PolicyView {
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
            levels: AgentAccess::ALL
                .into_iter()
                .map(|access| LevelView {
                    access,
                    label: access.label().to_owned(),
                    unavailable: access.unavailable().map(str::to_owned),
                })
                .collect(),
            toolchains,
        }
    }

    fn choice(&self, toolchain: Toolchain) -> Option<bool> {
        self.toolchains
            .iter()
            .find(|choice| choice.toolchain == toolchain)
            .map(|choice| choice.allowed)
    }
}

/// One project's agent policy as a client shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyView {
    pub access: AgentAccess,
    pub levels: Vec<LevelView>,
    pub toolchains: Vec<ToolchainView>,
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
    fn a_saved_policy_round_trips_and_a_missing_or_tampered_one_is_never_wider() {
        let connection = Connection::open_in_memory().unwrap();
        assert_eq!(load(&connection, "p").unwrap(), AgentPolicy::default());
        let policy = AgentPolicy {
            access: AgentAccess::Full,
            toolchains: vec![ToolchainChoice {
                toolchain: Toolchain::Python,
                allowed: false,
            }],
        };
        save(&connection, "p", &policy).unwrap();
        assert_eq!(load(&connection, "p").unwrap(), policy);
        assert_eq!(load(&connection, "other").unwrap(), AgentPolicy::default());
        for body in [
            r#"{"access":"root","toolchains":[]}"#.to_owned(),
            r#"{"access":"full","toolchains":[],"extra":1}"#.to_owned(),
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
}
