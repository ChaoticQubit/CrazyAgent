use super::*;
use crate::plugins::{Scope, ServerSpec};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS setup_pack (
    id TEXT PRIMARY KEY CHECK(length(id) BETWEEN 1 AND 64),
    body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) <= 65536)
);";

pub(crate) enum Command {
    Install {
        id: String,
        source: String,
        skills: Vec<PathBuf>,
        server: Option<(String, ServerSpec)>,
    },
    Set {
        id: String,
        enabled: bool,
    },
    Remove {
        id: String,
    },
    Installed,
}

/// One installed default-pack entry: its id and whether the owner left it on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstalledPack {
    pub(crate) id: String,
    pub(crate) enabled: bool,
}

pub(crate) enum ResultValue {
    Done,
    Installed(Vec<InstalledPack>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackRecord {
    id: String,
    source: String,
    skills: Vec<String>,
    server: Option<String>,
    enabled: bool,
}

#[derive(Default)]
struct Cleanup {
    added: Vec<PathBuf>,
    removed: Vec<PathBuf>,
    secrets: Vec<String>,
}

/// Creates the installed default-pack table when the engine database opens.
/// Input: the engine connection. Output: any SQLite failure.
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(SCHEMA).map_err(error)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

fn load_pack(tx: &Transaction<'_>, id: &str) -> Result<Option<PackRecord>, EngineError> {
    let body: Option<String> = tx
        .query_row("SELECT body FROM setup_pack WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()
        .map_err(error)?;
    body.map(|body| serde_json::from_str(&body).map_err(|_| error("setup_record_invalid")))
        .transpose()
}

fn save_pack(tx: &Transaction<'_>, record: &PackRecord) -> Result<(), EngineError> {
    let body = serde_json::to_string(record).map_err(|_| error("setup_record_invalid"))?;
    tx.execute(
        "INSERT INTO setup_pack(id, body) VALUES(?1, ?2)
         ON CONFLICT(id) DO UPDATE SET body=excluded.body",
        [&record.id, &body],
    )
    .map_err(error)?;
    Ok(())
}

fn taken(problem: EngineError) -> EngineError {
    match problem.0.as_str() {
        "skill_name_taken" | "server_name_taken" => error("pack_name_taken"),
        _ => problem,
    }
}

fn missing(problem: &EngineError) -> bool {
    matches!(problem.0.as_str(), "skill_not_found" | "server_not_found")
}

fn remove_items(
    tx: &Transaction<'_>,
    root: &Path,
    record: &PackRecord,
    cleanup: &mut Cleanup,
) -> Result<(), EngineError> {
    for id in &record.skills {
        match plugins::run(tx, root, plugins::Command::RemoveSkill { id: id.clone() }) {
            Ok(plugins::ResultValue::SkillRemoved(folder)) => cleanup.removed.push(folder),
            Ok(_) => return Err(error("setup_record_invalid")),
            Err(problem) if missing(&problem) => {}
            Err(problem) => return Err(problem),
        }
    }
    if let Some(id) = &record.server {
        match plugins::run(tx, root, plugins::Command::RemoveServer { id: id.clone() }) {
            Ok(plugins::ResultValue::Removed(references)) => cleanup.secrets.extend(references),
            Ok(_) => return Err(error("setup_record_invalid")),
            Err(problem) if missing(&problem) => {}
            Err(problem) => return Err(problem),
        }
    }
    tx.execute("DELETE FROM setup_pack WHERE id=?1", [&record.id])
        .map_err(error)?;
    Ok(())
}

fn install(
    tx: &Transaction<'_>,
    root: &Path,
    record: PackRecord,
    folders: Vec<PathBuf>,
    server: Option<(String, ServerSpec)>,
    cleanup: &mut Cleanup,
) -> Result<(), EngineError> {
    if let Some(old) = load_pack(tx, &record.id)? {
        if old.source == record.source {
            return Ok(());
        }
        remove_items(tx, root, &old, cleanup)?;
    }
    let mut record = record;
    for folder in folders {
        let source = folder
            .to_str()
            .ok_or_else(|| error("pack_no_skills"))?
            .to_owned();
        let added = plugins::run(
            tx,
            root,
            plugins::Command::AddSkill {
                scope: Scope::Global,
                source,
            },
        )
        .map_err(taken)?;
        let plugins::ResultValue::Skill(change) = added else {
            return Err(error("setup_record_invalid"));
        };
        cleanup.added.push(PathBuf::from(&change.view.folder));
        record.skills.push(change.view.id.clone());
    }
    if let Some((name, spec)) = server {
        let added = plugins::run(
            tx,
            root,
            plugins::Command::AddServer {
                scope: Scope::Global,
                name,
                spec: spec.clone(),
            },
        )
        .map_err(taken)?;
        let plugins::ResultValue::Server(view) = added else {
            return Err(error("setup_record_invalid"));
        };
        plugins::run(
            tx,
            root,
            plugins::Command::Approve {
                id: view.id.clone(),
                spec,
            },
        )?;
        record.server = Some(view.id.clone());
    }
    assert!(record.enabled);
    assert!(!record.skills.is_empty() || record.server.is_some());
    save_pack(tx, &record)
}

fn set(tx: &Transaction<'_>, root: &Path, id: &str, enabled: bool) -> Result<(), EngineError> {
    let mut record = load_pack(tx, id)?.ok_or_else(|| error("pack_not_installed"))?;
    for skill in &record.skills {
        let command = plugins::Command::SetSkill {
            id: skill.clone(),
            enabled,
        };
        match plugins::run(tx, root, command) {
            Ok(_) => {}
            Err(problem) if missing(&problem) => {}
            Err(problem) => return Err(problem),
        }
    }
    if let Some(server) = &record.server {
        let command = plugins::Command::SetServer {
            id: server.clone(),
            enabled,
        };
        match plugins::run(tx, root, command) {
            Ok(_) => {}
            Err(problem) if missing(&problem) => {}
            Err(problem) => return Err(problem),
        }
    }
    record.enabled = enabled;
    save_pack(tx, &record)
}

fn installed(tx: &Transaction<'_>) -> Result<Vec<InstalledPack>, EngineError> {
    let mut statement = tx
        .prepare("SELECT body FROM setup_pack ORDER BY id")
        .map_err(error)?;
    let bodies = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    bodies
        .iter()
        .map(|body| {
            let record: PackRecord =
                serde_json::from_str(body).map_err(|_| error("setup_record_invalid"))?;
            Ok(InstalledPack {
                id: record.id,
                enabled: record.enabled,
            })
        })
        .collect()
}

fn run(
    tx: &Transaction<'_>,
    root: &Path,
    command: Command,
    cleanup: &mut Cleanup,
) -> Result<ResultValue, EngineError> {
    Ok(match command {
        Command::Install {
            id,
            source,
            skills,
            server,
        } => {
            if !valid_id(&id) || source.is_empty() || (skills.is_empty() && server.is_none()) {
                return Err(error("pack_unknown"));
            }
            let record = PackRecord {
                id,
                source,
                skills: Vec::new(),
                server: None,
                enabled: true,
            };
            install(tx, root, record, skills, server, cleanup)?;
            ResultValue::Done
        }
        Command::Set { id, enabled } => {
            set(tx, root, &id, enabled)?;
            ResultValue::Done
        }
        Command::Remove { id } => {
            let record = load_pack(tx, &id)?.ok_or_else(|| error("pack_not_installed"))?;
            remove_items(tx, root, &record, cleanup)?;
            ResultValue::Done
        }
        Command::Installed => ResultValue::Installed(installed(tx)?),
    })
}

/// Runs one default-pack command in a single transaction: install an entry's skills and
/// its MCP server (approved, since the owner ticked it at setup), switch an entry on or
/// off, remove it, or list installed entries. Copied skill folders are removed again when
/// the change does not commit; removed folders and secrets go only after it commits.
/// Inputs: the engine connection, the engine root and the command. Output: the result.
pub(crate) fn execute(
    connection: &mut Connection,
    root: &Path,
    command: Command,
) -> Result<ResultValue, EngineError> {
    let mut cleanup = Cleanup::default();
    let tx = connection.transaction().map_err(error)?;
    let outcome = run(&tx, root, command, &mut cleanup).and_then(|value| {
        tx.commit().map_err(error)?;
        Ok(value)
    });
    match outcome {
        Ok(value) => {
            for folder in &cleanup.removed {
                let _ = std::fs::remove_dir_all(folder);
            }
            for reference in &cleanup.secrets {
                let _ = plugins::remove_secret(root, reference);
            }
            Ok(value)
        }
        Err(problem) => {
            for folder in &cleanup.added {
                let _ = std::fs::remove_dir_all(folder);
            }
            Err(problem)
        }
    }
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
