use crate::app_plugins::store::{self, Loaded};
use crate::app_plugins::{PluginKind, connector};
use crate::engine::EngineError;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS connector_grants (
    project TEXT NOT NULL CHECK(length(project) BETWEEN 1 AND 128),
    connector TEXT NOT NULL CHECK(length(connector) BETWEEN 1 AND 64),
    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
    who TEXT NOT NULL CHECK(who IN ('both', 'master', 'agents')),
    changed_ms INTEGER NOT NULL,
    PRIMARY KEY (project, connector)
)";

/// The most grant rows one project keeps. It is above the most connectors that can exist
/// at once, so rows naming connectors that are gone are dropped before it is reached.
pub const GRANTS_PER_PROJECT_MAX: usize = 128;

const _: () = assert!(GRANTS_PER_PROJECT_MAX > store::PLUGINS_MAX);

const NOT_READY: [&str; 5] = [
    "app_plugin_not_found",
    "app_plugin_refused",
    "app_plugin_needs_review",
    "app_plugin_needs_key",
    connector::OAUTH_NOT_YET,
];

/// Who in a project a connector reaches: the master and the agents whose tools are on,
/// the master only, or those agents only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Who {
    Both,
    Master,
    Agents,
}

impl Who {
    pub const ALL: [Self; 3] = [Self::Both, Self::Master, Self::Agents];

    fn stored(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Master => "master",
            Self::Agents => "agents",
        }
    }

    fn read(stored: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|who| who.stored() == stored)
    }

    fn reaches(self, role: Role) -> bool {
        match role {
            Role::Master => self != Self::Agents,
            Role::Agent { tools_on } => tools_on && self != Self::Master,
        }
    }
}

/// The launch asking for its connectors: the project's master, or one of its agents with
/// its tools switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Master,
    Agent { tools_on: bool },
}

/// One connector's switch in one project, as the owner's page shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub connector: String,
    pub enabled: bool,
    pub who: Who,
}

/// A connector a launch may use: its name and the loaded plugin, which holds where its
/// key is saved and never the key.
#[derive(Debug)]
pub struct GrantedConnector {
    pub name: String,
    pub loaded: Loaded,
}

fn error(code: &'static str) -> EngineError {
    EngineError(code.into())
}

fn sqlite(_: rusqlite::Error) -> EngineError {
    error("connectors_unavailable")
}

fn tables(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(sqlite)
}

fn names(db: &Connection, project: &str, enabled_only: bool) -> Result<Vec<Grant>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT connector, enabled, who FROM connector_grants
             WHERE project=?1 AND (enabled=1 OR ?2=0) ORDER BY connector LIMIT ?3",
        )
        .map_err(sqlite)?;
    let rows = statement
        .query_map(
            params![project, enabled_only, GRANTS_PER_PROJECT_MAX as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .map_err(sqlite)?;
    let mut grants = Vec::new();
    for row in rows {
        let (connector, enabled, who) = row.map_err(sqlite)?;
        grants.push(Grant {
            connector,
            enabled,
            who: Who::read(&who).ok_or_else(|| error("connectors_unavailable"))?,
        });
    }
    assert!(grants.len() <= GRANTS_PER_PROJECT_MAX);
    Ok(grants)
}

fn others(db: &Connection, project: &str, connector: &str) -> Result<usize, EngineError> {
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM connector_grants WHERE project=?1 AND connector<>?2",
            [project, connector],
            |row| row.get(0),
        )
        .map_err(sqlite)?;
    usize::try_from(count).map_err(|_| error("connectors_unavailable"))
}

fn drop_gone(db: &Connection, root: &Path, project: &str) -> Result<(), EngineError> {
    for grant in names(db, project, false)? {
        if store::kind_of(root, &grant.connector)? != Some(PluginKind::Connector) {
            db.execute(
                "DELETE FROM connector_grants WHERE project=?1 AND connector=?2",
                [project, grant.connector.as_str()],
            )
            .map_err(sqlite)?;
        }
    }
    Ok(())
}

/// Turns one connector on or off for one project and says who it reaches. Turning on
/// needs a project that exists and a connector that is trusted and has its key; turning
/// off only changes a row that exists. Inputs: database, engine root, project, connector,
/// the switch, who, and the time. Output: Ok, `connector_project_unknown`, or the store's
/// refusal for a connector that is not ready.
pub fn set(
    db: &Connection,
    root: &Path,
    project: &str,
    connector: &str,
    enabled: bool,
    who: Who,
    now_ms: i64,
) -> Result<(), EngineError> {
    tables(db)?;
    if !enabled {
        db.execute(
            "UPDATE connector_grants SET enabled=0, who=?3, changed_ms=?4
             WHERE project=?1 AND connector=?2",
            params![project, connector, who.stored(), now_ms],
        )
        .map_err(sqlite)?;
        return Ok(());
    }
    if !super::project_known(db, project)? {
        return Err(error(super::PROJECT_UNKNOWN));
    }
    store::connector_ready(db, root, connector)?;
    if others(db, project, connector)? >= GRANTS_PER_PROJECT_MAX {
        drop_gone(db, root, project)?;
    }
    assert!(others(db, project, connector)? < GRANTS_PER_PROJECT_MAX);
    db.execute(
        "INSERT INTO connector_grants (project, connector, enabled, who, changed_ms)
         VALUES (?1, ?2, 1, ?3, ?4)
         ON CONFLICT(project, connector) DO UPDATE SET enabled=1, who=excluded.who,
         changed_ms=excluded.changed_ms",
        params![project, connector, who.stored(), now_ms],
    )
    .map_err(sqlite)?;
    Ok(())
}

/// Lists a project's connector switches by connector name, on and off. Inputs: database
/// and project. Output: at most `GRANTS_PER_PROJECT_MAX` rows; a connector with no row is
/// off.
pub fn of_project(db: &Connection, project: &str) -> Result<Vec<Grant>, EngineError> {
    tables(db)?;
    names(db, project, false)
}

/// The connectors one launch may use: those turned on for its project that reach its
/// role, each still trusted with its key saved. An agent with tools off gets none, and a
/// connector that is not ready is left out. Inputs: database, engine root, project and
/// role. Output: the connectors by name, or the store's refusal when it cannot be read.
pub fn for_launch(
    db: &Connection,
    root: &Path,
    project: &str,
    role: Role,
) -> Result<Vec<GrantedConnector>, EngineError> {
    if role == (Role::Agent { tools_on: false }) {
        return Ok(Vec::new());
    }
    tables(db)?;
    let mut granted = Vec::new();
    for grant in names(db, project, true)? {
        assert!(grant.enabled);
        if !grant.who.reaches(role) {
            continue;
        }
        match store::connector_ready(db, root, &grant.connector) {
            Ok(loaded) => granted.push(GrantedConnector {
                name: grant.connector,
                loaded,
            }),
            Err(refusal) if NOT_READY.contains(&refusal.0.as_str()) => {}
            Err(refusal) => return Err(refusal),
        }
    }
    assert!(granted.len() <= GRANTS_PER_PROJECT_MAX);
    Ok(granted)
}

/// Deletes a project's connector switches when the project is removed, so a project added
/// again under the same id starts with every connector off. Inputs: the transaction and
/// the project. Output: done, or the database error.
pub fn forget_project(db: &Connection, project: &str) -> rusqlite::Result<()> {
    db.execute_batch(SCHEMA)?;
    db.execute("DELETE FROM connector_grants WHERE project=?1", [project])?;
    Ok(())
}

/// Deletes one connector's switches in every project: when it is removed, when it is
/// trusted again reaching other hosts, and when a bundled connector takes a name an older
/// plugin of the owner's had. Inputs: database and the connector's name. Output: Ok, or
/// `connectors_unavailable`.
pub fn forget_connector(db: &Connection, connector: &str) -> Result<(), EngineError> {
    tables(db)?;
    db.execute(
        "DELETE FROM connector_grants WHERE connector=?1",
        [connector],
    )
    .map_err(sqlite)?;
    Ok(())
}
