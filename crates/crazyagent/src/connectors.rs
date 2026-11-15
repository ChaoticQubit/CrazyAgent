//! Connectors: app plugins of kind `connector` that let a project's master and agents use
//! a vendor's MCP server through the engine. This module holds who may use which connector
//! in which project, the app-wide connector settings and the owner page's view. A key is
//! never part of a view; only where it is bound is.

use crate::app_plugins::store;
use crate::app_plugins::{Origin, PluginKind, PluginStatus, RunsView, SecretView};
use crate::engine::EngineError;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub mod grants;
pub mod settings;

pub use grants::{Grant, GrantedConnector, Role, Who};
pub use settings::{ConnectorSettings, KeepLog, NativeConnectors, SizeCap, Wait, WritePolicy};

/// The refusal for a project id that names no project.
pub const PROJECT_UNKNOWN: &str = "connector_project_unknown";

/// How the owner signs a connector in today: with a key, or not yet, for a connector whose
/// only way in is an OAuth sign-in this build does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignIn {
    Key,
    NotYet,
}

/// One connector as the owner's page lists it: what it is, where it came from, whether it
/// is trusted and has its key, what it reaches and where each secret is bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorView {
    pub name: String,
    pub title: String,
    pub version: String,
    pub description: String,
    pub origin: Origin,
    pub status: PluginStatus,
    pub sign_in: SignIn,
    pub runs: RunsView,
    pub secrets: Vec<SecretView>,
    pub sha256: String,
}

/// The owner's Connectors page: every connector, bundled first, whether the plugins folder
/// held more files than are listed, the settings with their version, and for the project
/// asked about its switches. A connector with no switch row is off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorsView {
    pub connectors: Vec<ConnectorView>,
    pub more: bool,
    pub settings: ConnectorSettings,
    pub version: u64,
    pub project: Option<String>,
    pub grants: Vec<Grant>,
}

const PLAIN: &[(&str, &str)] = &[
    (PROJECT_UNKNOWN, "that project is not set up here any more."),
    (
        "connector_request_invalid",
        "the request could not be read.",
    ),
    ("connector_request_bounds", "the request is too large."),
    (
        "connector_outcome_unknown",
        "the engine did not answer in time; check the list again.",
    ),
    ("connectors_unavailable", "the engine could not be reached."),
];

/// The plain sentence for a code the Connectors page can show: this module's own, or an
/// app plugin's. Input: the code. Output: the sentence, or none for a code of neither.
pub fn plain_problem(code: &str) -> Option<&'static str> {
    PLAIN
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, plain)| *plain)
        .or_else(|| crate::app_plugins::plain_problem(code))
}

pub(crate) fn project_known(db: &Connection, project: &str) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
        [project],
        |row| row.get(0),
    )
    .map_err(|_| EngineError("connectors_unavailable".into()))
}

/// Reads the Connectors page. Inputs: the engine database and root, and the project whose
/// switches to include. Output: the page, `connector_project_unknown`, or the store's
/// refusal when the plugins folder cannot be read.
pub fn view(
    db: &Connection,
    root: &Path,
    project: Option<&str>,
) -> Result<ConnectorsView, EngineError> {
    let grants = match project {
        None => Vec::new(),
        Some(project) if project_known(db, project)? => grants::of_project(db, project)?,
        Some(_) => return Err(EngineError(PROJECT_UNKNOWN.into())),
    };
    let listing = store::list(db, root)?;
    let connectors: Vec<ConnectorView> = listing
        .plugins
        .into_iter()
        .filter(|plugin| plugin.kind == PluginKind::Connector)
        .filter_map(|plugin| {
            Some(ConnectorView {
                sign_in: if plugin.secrets.is_empty() {
                    SignIn::NotYet
                } else {
                    SignIn::Key
                },
                runs: plugin.runs?,
                name: plugin.name,
                title: plugin.title,
                version: plugin.version,
                description: plugin.description,
                origin: plugin.origin,
                status: plugin.status,
                secrets: plugin.secrets,
                sha256: plugin.sha256,
            })
        })
        .collect();
    assert!(connectors.len() <= store::PLUGINS_MAX);
    let (settings, version) = settings::load(db)?;
    Ok(ConnectorsView {
        connectors,
        more: listing.more,
        settings,
        version,
        project: project.map(str::to_owned),
        grants,
    })
}

#[cfg(all(test, unix))]
mod tests;
