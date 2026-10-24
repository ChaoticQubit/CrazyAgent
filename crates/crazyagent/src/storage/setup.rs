use super::*;
#[cfg(unix)]
use crate::plugins::{Scope, ServerSpec};
use crate::setup::pack::{PackGroup, PackManifest};
use crate::setup::{SetupAction, SetupStatus, progress};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[cfg(unix)]
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS setup_pack (
    id TEXT PRIMARY KEY CHECK(length(id) BETWEEN 1 AND 64),
    body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) <= 65536)
);
CREATE TABLE IF NOT EXISTS setup_trash (
    reference TEXT PRIMARY KEY CHECK(length(reference) BETWEEN 1 AND 128)
);";
const REMOVED: &str = ".removed-";
const SKILLS: [&str; 2] = ["skills", "global"];

pub(crate) enum Command {
    #[cfg(unix)]
    Install {
        id: String,
        source: String,
        skills: Vec<PathBuf>,
        server: Option<(String, ServerSpec)>,
    },
    Sweep,
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

/// Creates the installed default-pack tables when the engine database opens.
/// Input: the engine connection. Output: any SQLite failure.
#[cfg(unix)]
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(SCHEMA).map_err(error)
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

#[cfg(unix)]
fn taken(problem: EngineError) -> EngineError {
    match problem.0.as_str() {
        "skill_name_taken" | "server_name_taken" => error("pack_name_taken"),
        _ => problem,
    }
}

fn missing(problem: &EngineError) -> bool {
    matches!(problem.0.as_str(), "skill_not_found" | "server_not_found")
}

/// Removes one installed entry's skills, server and row inside the transaction. Each
/// skill folder moves aside to `.removed-<skill id>` at once, so a reinstall can reuse
/// the name; `sweep` deletes it after the change commits or puts it back if it did not.
/// The server's secret references go to `setup_trash` for `sweep` to delete.
fn remove_items(
    tx: &Transaction<'_>,
    root: &Path,
    record: &PackRecord,
    moved: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<(), EngineError> {
    for id in &record.skills {
        match plugins::run(tx, root, plugins::Command::RemoveSkill { id: id.clone() }) {
            Ok(plugins::ResultValue::SkillRemoved(folder)) => {
                if std::fs::symlink_metadata(&folder).is_ok() {
                    let parent = folder
                        .parent()
                        .ok_or_else(|| error("plugin_store_unavailable"))?;
                    let aside = parent.join(format!("{REMOVED}{id}"));
                    std::fs::rename(&folder, &aside)
                        .map_err(|_| error("plugin_store_unavailable"))?;
                    moved.push((folder, aside));
                }
            }
            Ok(_) => return Err(error("setup_record_invalid")),
            Err(problem) if missing(&problem) => {}
            Err(problem) => return Err(problem),
        }
    }
    if let Some(id) = &record.server {
        match plugins::run(tx, root, plugins::Command::RemoveServer { id: id.clone() }) {
            Ok(plugins::ResultValue::Removed(references)) => {
                for reference in references {
                    tx.execute(
                        "INSERT OR IGNORE INTO setup_trash(reference) VALUES(?1)",
                        [&reference],
                    )
                    .map_err(error)?;
                }
            }
            Ok(_) => return Err(error("setup_record_invalid")),
            Err(problem) if missing(&problem) => {}
            Err(problem) => return Err(problem),
        }
    }
    tx.execute("DELETE FROM setup_pack WHERE id=?1", [&record.id])
        .map_err(error)?;
    Ok(())
}

fn restore(moved: &[(PathBuf, PathBuf)]) {
    for (folder, aside) in moved.iter().rev() {
        let _ = std::fs::rename(aside, folder);
    }
}

#[cfg(unix)]
fn install(
    tx: &Transaction<'_>,
    root: &Path,
    record: PackRecord,
    folders: Vec<PathBuf>,
    server: Option<(String, ServerSpec)>,
    added: &mut Vec<PathBuf>,
    moved: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<(), EngineError> {
    if let Some(old) = load_pack(tx, &record.id)? {
        if old.source == record.source {
            return Ok(());
        }
        remove_items(tx, root, &old, moved)?;
    }
    let mut record = record;
    for folder in folders {
        let source = folder
            .to_str()
            .ok_or_else(|| error("pack_no_skills"))?
            .to_owned();
        let change = plugins::run(
            tx,
            root,
            plugins::Command::AddSkill {
                scope: Scope::Global,
                source,
            },
        )
        .map_err(taken)?;
        let plugins::ResultValue::Skill(change) = change else {
            return Err(error("setup_record_invalid"));
        };
        added.push(PathBuf::from(&change.view.folder));
        record.skills.push(change.view.id.clone());
    }
    if let Some((name, spec)) = server {
        let change = plugins::run(
            tx,
            root,
            plugins::Command::AddServer {
                scope: Scope::Global,
                name,
                spec: spec.clone(),
            },
        )
        .map_err(taken)?;
        let plugins::ResultValue::Server(view) = change else {
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

fn set(
    tx: &Transaction<'_>,
    root: &Path,
    record: &mut PackRecord,
    on: bool,
) -> Result<(), EngineError> {
    for skill in &record.skills {
        let command = plugins::Command::SetSkill {
            id: skill.clone(),
            enabled: on,
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
            enabled: on,
        };
        match plugins::run(tx, root, command) {
            Ok(_) => {}
            Err(problem) if missing(&problem) => {}
            Err(problem) => return Err(problem),
        }
    }
    record.enabled = on;
    save_pack(tx, record)
}

fn installed(tx: &Transaction<'_>) -> Result<Vec<(String, bool)>, EngineError> {
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
            Ok((record.id, record.enabled))
        })
        .collect()
}

fn manifest() -> Result<PackManifest, EngineError> {
    crate::setup::pack::default_pack()
}

fn root_of(path: &Path) -> Result<&Path, EngineError> {
    path.parent()
        .ok_or_else(|| error("setup_store_unavailable"))
}

fn rejected(code: &str) -> Outcome {
    Outcome::Rejected { code: code.into() }
}

/// Applies one owner change to the default pack inside the owner transaction. A setup
/// run is only admitted here (the engine starts it once this commits) and the ticked
/// MCP servers in it count as approved by the owner. Switching or removing checks the
/// entry is known, installed and not always-on before changing anything. Inputs: the
/// transaction, the database path and the action. Output: applied or a plain refusal.
pub(crate) fn apply_owner(
    tx: &Transaction<'_>,
    path: &Path,
    action: &SetupAction,
) -> Result<Outcome, EngineError> {
    if action.validate().is_err() {
        return Ok(rejected("pack_unknown"));
    }
    let root = root_of(path)?;
    let manifest = manifest()?;
    let entry = |id: &str| manifest.entries.iter().find(|entry| entry.id == id);
    match action {
        SetupAction::Run { pack } => {
            if pack.iter().any(|id| entry(id.as_str()).is_none()) {
                return Ok(rejected("pack_unknown"));
            }
            if progress::snapshot(root).running {
                return Ok(rejected("setup_busy"));
            }
            Ok(Outcome::Applied)
        }
        SetupAction::Set { id, on } => {
            let Some(known) = entry(id.as_str()) else {
                return Ok(rejected("pack_unknown"));
            };
            if !on && known.group == PackGroup::Always {
                return Ok(rejected("pack_required"));
            }
            let Some(mut record) = load_pack(tx, id)? else {
                return Ok(rejected("pack_not_installed"));
            };
            set(tx, root, &mut record, *on)?;
            Ok(Outcome::Applied)
        }
        SetupAction::Remove { id } => {
            let Some(known) = entry(id.as_str()) else {
                return Ok(rejected("pack_unknown"));
            };
            if known.group == PackGroup::Always {
                return Ok(rejected("pack_required"));
            }
            let Some(record) = load_pack(tx, id)? else {
                return Ok(rejected("pack_not_installed"));
            };
            let mut moved = Vec::new();
            if let Err(problem) = remove_items(tx, root, &record, &mut moved) {
                restore(&moved);
                return Err(problem);
            }
            Ok(Outcome::Applied)
        }
    }
}

/// Reads the setup screen inside an owner read: every pack entry with its state from the
/// installed rows and the running setup, and each dependency as the running setup left it
/// (the engine fills in what the machine has). Inputs: the transaction and the database
/// path. Output: the status.
pub(crate) fn status(tx: &Transaction<'_>, path: &Path) -> Result<SetupStatus, EngineError> {
    let root = root_of(path)?;
    let progress = progress::snapshot(root);
    let manifest = manifest()?;
    let installed = installed(tx)?;
    Ok(SetupStatus {
        dependencies: progress::dependency_rows(&progress),
        pack: progress::pack_rows(&manifest, &installed, &progress),
        running: progress.running,
    })
}

fn sweep(connection: &mut Connection, root: &Path) -> Result<(), EngineError> {
    let tx = connection.transaction().map_err(error)?;
    let folder = root.join(plugins::STORE).join(SKILLS[0]).join(SKILLS[1]);
    if let Ok(entries) = std::fs::read_dir(&folder) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_prefix(REMOVED) else {
                continue;
            };
            let kept: Option<String> = tx
                .query_row(
                    "SELECT body FROM plugin_items WHERE id=?1 AND kind='skill'",
                    [id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(error)?;
            let back = kept.and_then(|body| {
                serde_json::from_str::<serde_json::Value>(&body)
                    .ok()?
                    .get("folder")?
                    .as_str()
                    .map(|relative| root.join(relative))
            });
            match back {
                Some(back) if std::fs::symlink_metadata(&back).is_err() => {
                    let _ = std::fs::rename(entry.path(), back);
                }
                _ => {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
    }
    let references: Vec<String> = {
        let mut statement = tx
            .prepare("SELECT reference FROM setup_trash")
            .map_err(error)?;
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?
    };
    for reference in &references {
        plugins::remove_secret(root, reference)?;
        tx.execute("DELETE FROM setup_trash WHERE reference=?1", [reference])
            .map_err(error)?;
    }
    tx.commit().map_err(error)
}

/// Runs one default-pack job for the engine: install an entry's skills and its approved
/// MCP server in one transaction, or sweep folders and secrets that removals left. Copied
/// skill folders are removed again and moved ones put back when an install does not
/// commit; replaced ones are deleted after it commits. Inputs: the engine connection, the
/// engine root and the job. Output: nothing, or the plain reason.
#[cfg(unix)]
pub(crate) fn execute(
    connection: &mut Connection,
    root: &Path,
    command: Command,
) -> Result<(), EngineError> {
    let Command::Install {
        id,
        source,
        skills,
        server,
    } = command
    else {
        return sweep(connection, root);
    };
    if source.is_empty() || (skills.is_empty() && server.is_none()) {
        return Err(error("pack_unknown"));
    }
    let record = PackRecord {
        id,
        source,
        skills: Vec::new(),
        server: None,
        enabled: true,
    };
    let mut added = Vec::new();
    let mut moved = Vec::new();
    let tx = connection.transaction().map_err(error)?;
    let outcome = install(&tx, root, record, skills, server, &mut added, &mut moved)
        .and_then(|()| tx.commit().map_err(error));
    match outcome {
        Ok(()) => {
            for (_, aside) in &moved {
                let _ = std::fs::remove_dir_all(aside);
            }
            Ok(())
        }
        Err(problem) => {
            for folder in &added {
                let _ = std::fs::remove_dir_all(folder);
            }
            restore(&moved);
            Err(problem)
        }
    }
}

/// Runs the only default-pack job that exists where setup cannot install packs: the sweep
/// of folder and secret removals left by an earlier run. Inputs: the engine connection,
/// engine root and job. Output: nothing, or a plain reason.
#[cfg(not(unix))]
pub(crate) fn execute(
    connection: &mut Connection,
    root: &Path,
    command: Command,
) -> Result<(), EngineError> {
    let Command::Sweep = command;
    sweep(connection, root)
}

#[cfg(all(test, unix))]
#[path = "setup_tests.rs"]
mod tests;
