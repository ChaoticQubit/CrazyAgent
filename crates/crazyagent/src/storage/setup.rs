use super::*;
#[cfg(unix)]
use crate::plugins::{Scope, ServerSpec};
use crate::setup::pack::{GRAPH_ENTRY, PackKind, PackManifest, PackManifestEntry};
use crate::setup::{SetupAction, SetupStatus, progress};
use crate::storage::versioned;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[cfg(any(unix, test))]
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS setup_pack (
    id TEXT PRIMARY KEY CHECK(length(id) BETWEEN 1 AND 64),
    body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) <= 65536)
);
CREATE TABLE IF NOT EXISTS setup_trash (
    reference TEXT PRIMARY KEY CHECK(length(reference) BETWEEN 1 AND 128)
);";
const REMOVED: &str = ".removed-";
const PRESENT: &str = "present";
const SKILLS: [&str; 2] = ["skills", "global"];

pub(crate) enum Command {
    #[cfg(unix)]
    Install {
        id: String,
        source: String,
        skills: Vec<PathBuf>,
        server: Option<(String, ServerSpec)>,
    },
    #[cfg(unix)]
    SeedHooks {
        id: String,
        plugin: PathBuf,
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
#[cfg(any(unix, test))]
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

/// An entry the plugin store already holds with no pack row: a global skill or MCP server
/// named like the entry, as when the owner added it before setup ran. The row keeps source
/// `present`, so the item stays the owner's: a setup run never replaces or removes it.
/// Inputs: the transaction and the entry. Output: the row that adopts that item, on or off
/// as the item is, or none.
fn present(
    tx: &Transaction<'_>,
    entry: &PackManifestEntry,
) -> Result<Option<PackRecord>, EngineError> {
    let kind = match entry.kind {
        PackKind::Skill => "skill",
        PackKind::McpServer => "server",
    };
    let item: Option<(String, String)> = tx
        .query_row(
            "SELECT id, body FROM plugin_items WHERE kind=?1 AND scope='global' AND name=?2",
            [kind, entry.id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((item, body)) = item else {
        return Ok(None);
    };
    let enabled = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|body| body.get("enabled")?.as_bool())
        .ok_or_else(|| error("setup_record_invalid"))?;
    let (skills, server) = match entry.kind {
        PackKind::Skill => (vec![item], None),
        PackKind::McpServer => (Vec::new(), Some(item)),
    };
    Ok(Some(PackRecord {
        id: entry.id.clone(),
        source: PRESENT.into(),
        skills,
        server,
        enabled,
    }))
}

/// The pack row of an entry, adopting an item the plugin store already holds when the
/// entry has no row yet. Inputs: the transaction and the entry. Output: the row, or none
/// when nothing of the entry is installed.
fn adopted(
    tx: &Transaction<'_>,
    entry: &PackManifestEntry,
) -> Result<Option<PackRecord>, EngineError> {
    if let Some(record) = load_pack(tx, &entry.id)? {
        return Ok(Some(record));
    }
    let Some(record) = present(tx, entry)? else {
        return Ok(None);
    };
    save_pack(tx, &record)?;
    Ok(Some(record))
}

/// Tells whether a pack entry is installed and, when it is, switched on, counting an item
/// the plugin store already holds. Onboarding builds a code graph only while the graphify
/// entry is on. Inputs: the transaction and the entry id. Output: none when nothing of the
/// entry is installed, otherwise whether it is on.
pub(crate) fn entry_switch(tx: &Transaction<'_>, id: &str) -> Result<Option<bool>, EngineError> {
    if let Some(record) = load_pack(tx, id)? {
        return Ok(Some(record.enabled));
    }
    let manifest = manifest()?;
    let Some(entry) = manifest.entries.iter().find(|entry| entry.id == id) else {
        return Ok(None);
    };
    Ok(present(tx, entry)?.map(|record| record.enabled))
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

/// Writes the next default-pack version when an installed entry holds this plugin, so a
/// change made on the plugin screen refuses a pack screen opened before it. Inputs: the
/// transaction and the plugin id. Output: Ok, or a store failure.
pub(super) fn plugin_changed(tx: &Transaction<'_>, id: &str) -> Result<(), EngineError> {
    let manifest = manifest()?;
    for entry in &manifest.entries {
        let record = match load_pack(tx, &entry.id)? {
            Some(record) => Some(record),
            None => present(tx, entry)?,
        };
        let held = record.is_some_and(|record| {
            record.skills.iter().any(|skill| skill == id) || record.server.as_deref() == Some(id)
        });
        if held {
            let item = versioned::default_pack_item();
            let current = versioned::current(tx, &item)?.version;
            let written = versioned::owner_write(tx, &item, current)?;
            assert_eq!(written.ok(), Some(current + 1));
            return Ok(());
        }
    }
    Ok(())
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
        let command = plugins::Command::RemoveSkill {
            id: id.clone(),
            version: plugins::plugin_version(tx, id)?,
        };
        match plugins::run(tx, root, command) {
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
        let command = plugins::Command::RemoveServer {
            id: id.clone(),
            version: plugins::plugin_version(tx, id)?,
        };
        match plugins::run(tx, root, command) {
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

/// What a new pin keeps from the pin it replaces: whether the entry is on, each skill's
/// on/off state and per-project switches by skill name, and the server when the new pin
/// names the same one.
#[cfg(unix)]
struct Kept {
    enabled: bool,
    server: Option<String>,
    skills: Vec<(String, bool, Vec<String>)>,
}

/// Reads what the owner set on an older pin, then removes its skills, and its server
/// unless the new pin names the same server, which is then reused so its secrets,
/// switches and on/off state stay. Inputs: the transaction, engine root, old row, new
/// server and the moved-aside folders. Output: what the new pin keeps.
#[cfg(unix)]
fn carry(
    tx: &Transaction<'_>,
    root: &Path,
    old: PackRecord,
    server: Option<&(String, ServerSpec)>,
    moved: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<Kept, EngineError> {
    let mut skills = Vec::new();
    for id in &old.skills {
        let held: Option<(String, String)> = tx
            .query_row(
                "SELECT name, body FROM plugin_items WHERE id=?1 AND kind='skill'",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(error)?;
        let Some((name, body)) = held else {
            continue;
        };
        let enabled = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|body| body.get("enabled")?.as_bool())
            .ok_or_else(|| error("setup_record_invalid"))?;
        let mut statement = tx
            .prepare("SELECT project FROM plugin_switches WHERE item=?1")
            .map_err(error)?;
        let projects = statement
            .query_map([id], |row| row.get::<_, String>(0))
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        skills.push((name, enabled, projects));
    }
    let reused = match (&old.server, server) {
        (Some(id), Some((name, _))) => tx
            .query_row(
                "SELECT name FROM plugin_items WHERE id=?1 AND kind='server'",
                [id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(error)?
            .filter(|held| held == name)
            .map(|_| id.clone()),
        _ => None,
    };
    let mut dropped = old;
    if reused.is_some() {
        dropped.server = None;
    }
    remove_items(tx, root, &dropped, moved)?;
    Ok(Kept {
        enabled: dropped.enabled,
        server: reused,
        skills,
    })
}

/// Installs one entry's pinned skills and server, replacing an older pin the pack itself
/// installed while keeping what the owner set on it (see `carry`). An item the owner holds
/// under the entry's name is adopted as present and never replaced or removed. Inputs: the
/// transaction, engine root, new row, skill folders and server, plus the lists of added
/// and moved-aside folders. Output: any failure.
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
    let mut kept = None;
    if let Some(old) = load_pack(tx, &record.id)? {
        if old.source == record.source {
            return Ok(());
        }
        if old.source != PRESENT {
            kept = Some(carry(tx, root, old, server.as_ref(), moved)?);
        }
    }
    let reused = kept.as_ref().and_then(|kept| kept.server.clone());
    if let Some(entry) = manifest()?
        .entries
        .iter()
        .find(|entry| entry.id == record.id)
        && let Some(owned) = present(tx, entry)?
        && (reused.is_none() || owned.server != reused)
    {
        return save_pack(tx, &owned);
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
        let carried = kept
            .iter()
            .flat_map(|kept| &kept.skills)
            .find(|(name, ..)| *name == change.view.name);
        if let Some((_, enabled, projects)) = carried {
            if !enabled {
                plugins::run(
                    tx,
                    root,
                    plugins::Command::SetSkill {
                        id: change.view.id.clone(),
                        enabled: false,
                        version: plugins::plugin_version(tx, &change.view.id)?,
                    },
                )?;
            }
            for project in projects {
                tx.execute(
                    "INSERT OR IGNORE INTO plugin_switches(project,item) VALUES(?1,?2)",
                    [project, &change.view.id],
                )
                .map_err(error)?;
            }
        }
    }
    if let Some((name, spec)) = server {
        let command = match &reused {
            Some(id) => plugins::Command::UpdateServer {
                id: id.clone(),
                spec: spec.clone(),
                version: plugins::plugin_version(tx, id)?,
            },
            None => plugins::Command::AddServer {
                scope: Scope::Global,
                name,
                spec: spec.clone(),
                secrets: Vec::new(),
            },
        };
        let change = plugins::run(tx, root, command).map_err(taken)?;
        let plugins::ResultValue::Server(view) = change else {
            return Err(error("setup_record_invalid"));
        };
        if view.enabled {
            plugins::run(
                tx,
                root,
                plugins::Command::Approve {
                    id: view.id.clone(),
                    spec,
                },
            )?;
        }
        record.server = Some(view.id.clone());
    }
    assert!(!record.skills.is_empty() || record.server.is_some());
    if kept.is_none_or(|kept| kept.enabled) {
        assert!(record.enabled);
        save_pack(tx, &record)
    } else {
        set(tx, root, &mut record, false)
    }
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
            version: plugins::plugin_version(tx, skill)?,
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
            version: plugins::plugin_version(tx, server)?,
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

fn installed(
    tx: &Transaction<'_>,
    manifest: &PackManifest,
) -> Result<Vec<(String, bool)>, EngineError> {
    let mut statement = tx
        .prepare("SELECT body FROM setup_pack ORDER BY id")
        .map_err(error)?;
    let bodies = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    let mut rows = bodies
        .iter()
        .map(|body| {
            let record: PackRecord =
                serde_json::from_str(body).map_err(|_| error("setup_record_invalid"))?;
            Ok((record.id, record.enabled))
        })
        .collect::<Result<Vec<_>, EngineError>>()?;
    for entry in &manifest.entries {
        if rows.iter().any(|(id, _)| *id == entry.id) {
            continue;
        }
        if let Some(record) = present(tx, entry)? {
            rows.push((record.id, record.enabled));
        }
    }
    Ok(rows)
}

/// Takes an installed entry for an owner switch or removal once the pack is still at the
/// version the screen showed, writing the next pack version before the entry changes.
/// Inputs: the transaction, the entry and the version shown. Output: the entry's record,
/// or none when the entry is not installed or the pack moved on (see `refusal`).
fn owned(
    tx: &Transaction<'_>,
    entry: &PackManifestEntry,
    version: u64,
) -> Result<Option<PackRecord>, EngineError> {
    if entry_switch(tx, &entry.id)?.is_none() {
        return Ok(None);
    }
    let Ok(next) = versioned::owner_write(tx, &versioned::default_pack_item(), version)? else {
        return Ok(None);
    };
    assert_eq!(next, version + 1);
    let record = adopted(tx, entry)?;
    assert!(record.is_some(), "an installed pack entry is adopted");
    Ok(record)
}

/// Names why `owned` gave no record: `pack_not_installed` before `stale_revision`.
/// Inputs: the transaction and the entry. Output: the refusal code.
fn refusal(tx: &Transaction<'_>, entry: &PackManifestEntry) -> Result<&'static str, EngineError> {
    Ok(match entry_switch(tx, &entry.id)? {
        None => "pack_not_installed",
        Some(_) => versioned::STALE_CODE,
    })
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
/// run is only admitted here (the engine starts it once this commits), the ticked MCP
/// servers in it count as approved by the owner, and it installs the graphify command only
/// when the code graph entry is ticked or on. Switching or removing checks the entry is
/// known and installed, adopting an item the plugin store already holds, before changing
/// anything; no entry is forced on. Inputs: the transaction, the database path and the
/// action. Output: applied or a plain refusal.
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
            let graph_tool = pack.iter().any(|id| id.as_str() == GRAPH_ENTRY)
                || entry_switch(tx, GRAPH_ENTRY)? == Some(true);
            progress::with(root, |state| state.graph_tool = graph_tool);
            Ok(Outcome::Applied)
        }
        SetupAction::Set { id, on, version } => {
            let Some(known) = entry(id.as_str()) else {
                return Ok(rejected("pack_unknown"));
            };
            let Some(mut record) = owned(tx, known, *version)? else {
                return Ok(rejected(refusal(tx, known)?));
            };
            set(tx, root, &mut record, *on)?;
            Ok(Outcome::Applied)
        }
        SetupAction::Remove { id, version } => {
            let Some(known) = entry(id.as_str()) else {
                return Ok(rejected("pack_unknown"));
            };
            let Some(record) = owned(tx, known, *version)? else {
                return Ok(rejected(refusal(tx, known)?));
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
    let installed = installed(tx, &manifest)?;
    Ok(SetupStatus {
        dependencies: progress::dependency_rows(&progress),
        pack: progress::pack_rows(&manifest, &installed, &progress),
        running: progress.running,
        notes: Vec::new(),
        version: versioned::current(tx, &versioned::default_pack_item())?.version,
    })
}

fn sweep(connection: &mut Connection, root: &Path) -> Result<(), EngineError> {
    let tx = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
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

/// Adds the hooks an installed, switched-on entry's plugin folder defines as global hooks
/// that follow the entry's MCP server, or else its first skill (the owner's own item when
/// the entry adopted one), unless hooks already follow that item or already exist under
/// those names. An entry not installed or switched off gets none. Inputs: the engine
/// connection, the engine root, the entry id and its plugin folder. Output: nothing, or the
/// plain reason, with the hooks' folder copy removed when they did not commit.
#[cfg(unix)]
fn seed_hooks(
    connection: &mut Connection,
    root: &Path,
    id: &str,
    plugin: &Path,
) -> Result<(), EngineError> {
    let tx = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let Some(record) = load_pack(&tx, id)? else {
        return Ok(());
    };
    if !record.enabled {
        return Ok(());
    }
    let Some(follows) = record
        .server
        .clone()
        .or_else(|| record.skills.first().cloned())
    else {
        return Ok(());
    };
    if plugins::hooks_follow(&tx, &follows)? {
        return Ok(());
    }
    let source = plugin
        .to_str()
        .ok_or_else(|| error("pack_no_skills"))?
        .to_owned();
    let command = plugins::Command::AddHooks {
        scope: Scope::Global,
        source,
        follows: Some(follows),
    };
    match plugins::run(&tx, root, command) {
        Ok(plugins::ResultValue::HooksAdded(_, copy)) => tx.commit().map_err(|problem| {
            let _ = std::fs::remove_dir_all(&copy);
            error(problem)
        }),
        Ok(_) => Err(error("setup_record_invalid")),
        Err(problem)
            if matches!(
                problem.0.as_str(),
                "hooks_none_found" | "hooks_already_added"
            ) =>
        {
            Ok(())
        }
        Err(problem) => Err(problem),
    }
}

/// Runs one default-pack job for the engine: install an entry's skills and its approved
/// MCP server in one transaction, add the hooks of its plugin folder and keep why they were
/// refused for the entry's setup row, or sweep folders and secrets that removals left.
/// Copied skill folders are removed again and moved ones put back when an install does not
/// commit; replaced ones are deleted after it commits.
/// Inputs: the engine connection, the engine root and the job. Output: nothing, or the
/// plain reason.
#[cfg(unix)]
pub(crate) fn execute(
    connection: &mut Connection,
    root: &Path,
    command: Command,
) -> Result<(), EngineError> {
    let (id, source, skills, server) = match command {
        Command::Install {
            id,
            source,
            skills,
            server,
        } => (id, source, skills, server),
        Command::SeedHooks { id, plugin } => {
            let seeded = seed_hooks(connection, root, &id, &plugin);
            progress::with(root, |state| match &seeded {
                Ok(()) => state.hooks.remove(&id),
                Err(problem) => state
                    .hooks
                    .insert(id.clone(), crate::setup::pack_reason(&problem.0)),
            });
            return seeded;
        }
        Command::Sweep => return sweep(connection, root),
    };
    if source.is_empty() || source == PRESENT || (skills.is_empty() && server.is_none()) {
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
    let tx = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
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

#[cfg(test)]
#[path = "setup_switch_tests.rs"]
mod switch_tests;
#[cfg(all(test, unix))]
#[path = "setup_tests.rs"]
mod tests;
