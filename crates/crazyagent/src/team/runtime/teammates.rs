use super::{error, names};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};
use std::io::Read;
use std::path::Path;

const TEAM_FOLDERS_MAX: usize = 16;
const CONFIG_BYTES_MAX: u64 = 256 * 1024;
const MEMBERS_MAX: usize = 64;
const NAME_BYTES_MAX: usize = 64;
const LEAD_NAME: &str = "team-lead";

const _: () = assert!(MEMBERS_MAX >= crate::team::TEAMMATES_SHOWN_MAX);

/// One teammate a Claude Code lead's agent team lists: its name and the tmux pane it runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Member {
    pub name: String,
    pub pane: String,
}

/// Reads the teammates a native lead's Claude Code agent team lists, read-only, from the team
/// config files Claude Code keeps in the lead's private home: `teams/<team>/config.json` under
/// its `CLAUDE_CONFIG_DIR`, or under `.claude` there. The files are the agent's own, so every
/// field is untrusted and bounded. Input: the lead's home. Output: at most `MEMBERS_MAX`
/// members, the lead itself and any member without a valid pane id left out.
pub(super) fn members(home: &Path) -> Vec<Member> {
    let mut found = Vec::new();
    for root in [home.join("teams"), home.join(".claude").join("teams")] {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten().take(TEAM_FOLDERS_MAX) {
            let Some(bytes) = config_bytes(&entry.path().join("config.json")) else {
                continue;
            };
            let Ok(config) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                continue;
            };
            let Some(listed) = config.get("members").and_then(serde_json::Value::as_array) else {
                continue;
            };
            for member in listed.iter().take(MEMBERS_MAX) {
                if found.len() == MEMBERS_MAX {
                    break;
                }
                let text = |key: &str| member.get(key).and_then(serde_json::Value::as_str);
                let (Some(name), Some(pane)) = (text("name"), text("tmuxPaneId")) else {
                    continue;
                };
                if name == LEAD_NAME
                    || name.is_empty()
                    || name.len() > NAME_BYTES_MAX
                    || name.chars().any(char::is_control)
                    || !crate::terminal::pane_id_valid(pane)
                    || found.iter().any(|seen: &Member| seen.pane == pane)
                {
                    continue;
                }
                found.push(Member {
                    name: name.to_owned(),
                    pane: pane.to_owned(),
                });
            }
        }
    }
    assert!(found.len() <= MEMBERS_MAX);
    found
}

/// Reads one team config without following a link or waiting on a FIFO, and only a regular
/// file of at most `CONFIG_BYTES_MAX` bytes. Input: the path. Output: the bytes, or none.
fn config_bytes(path: &Path) -> Option<Vec<u8>> {
    let file = crate::platform::open_plain(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > CONFIG_BYTES_MAX {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(CONFIG_BYTES_MAX + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= CONFIG_BYTES_MAX).then_some(bytes)
}

/// Keeps one team agent row per live native teammate of a lead: role `teammate`, parent the
/// lead, and the pane id `team_teammates` records for the same pane. A member whose pane is
/// live gets a row, Working until the relay reads it resting; an Idle row stays Idle, so
/// only the teammate's own hook moves it (`relay::serve`). A row stopped in this run of the
/// lead is marked working again only when the engine closed or the pane went missing; one
/// the owner stopped, the cap ended or that ended with its lead stays stopped while its pane
/// is ended. A teammate row whose pane is no longer live is stopped. Only panes the terminal host lists as the lead's live teammate
/// panes count, never a pane a config file alone names. Inputs: the connection, project,
/// lead, its home, its live teammate pane ids, when this run of the lead started and the
/// time. Output: any SQLite failure.
pub(super) fn register(
    connection: &Connection,
    project: &ProjectId,
    lead: i64,
    home: &Path,
    live: &[&str],
    since: i64,
    at: i64,
) -> Result<(), EngineError> {
    let row: Option<(String, bool, Option<String>, bool)> = connection
        .query_row(
            "SELECT harness,tools,connection,hookless FROM team_agents
             WHERE id=?1 AND project=?2 AND role='sub' AND parent IS NULL",
            params![lead, project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((harness, tools, account, hookless)) = row else {
        return Ok(());
    };
    for member in members(home)
        .into_iter()
        .filter(|member| live.contains(&member.pane.as_str()))
    {
        let known: Option<(i64, bool)> = connection
            .query_row(
                "SELECT id, status IN ('starting','idle','working')
                        OR error IS NULL OR error NOT IN (?5,?6,?7,?8,?9)
                 FROM team_agents
                 WHERE project=?1 AND parent=?2 AND role='teammate' AND pane=?3
                 AND (status IN ('starting','idle','working') OR stopped_ms>=?4)
                 ORDER BY status IN ('starting','idle','working') DESC, id DESC LIMIT 1",
                params![
                    project.as_str(),
                    lead,
                    member.pane,
                    since,
                    super::OWNER_STOPPED_TEAMMATE,
                    super::MASTER_STOPPED_TEAMMATE,
                    super::LEAD_STOPPED_TEAMMATE,
                    super::LEAD_ENDED_TEAMMATE,
                    super::OVER_TEAMMATE_CAP
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(error)?;
        match known {
            Some((_, false)) => {}
            Some((id, true)) => {
                connection
                    .execute(
                        "UPDATE team_agents SET status=CASE status WHEN 'idle' THEN 'idle' ELSE 'working' END,
                         error=NULL, stopped_ms=NULL, heartbeat_ms=?2 WHERE id=?1",
                        params![id, at],
                    )
                    .map_err(error)?;
                keep_name(connection, lead, id, &member.name)?;
            }
            None => {
                connection
                    .execute(
                        "INSERT INTO team_agents(project,role,harness,tools,connection,status,created_ms,heartbeat_ms,parent,pane,hookless)
                         VALUES (?1,'teammate',?2,?3,?4,'working',?5,?5,?6,?7,?8)",
                        params![project.as_str(), harness, tools, account, at, lead, member.pane, hookless],
                    )
                    .map_err(error)?;
                let id = connection.last_insert_rowid();
                let _ = names::assign_role(connection, project, id, &member.name)?;
                keep_name(connection, lead, id, &member.name)?;
            }
        }
    }
    let open: Vec<(i64, String)> = {
        let mut statement = connection
            .prepare(
                "SELECT id,pane FROM team_agents WHERE project=?1 AND parent=?2
                 AND role='teammate' AND status IN ('starting','idle','working')",
            )
            .map_err(error)?;
        statement
            .query_map(params![project.as_str(), lead], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(error)?
            .collect::<Result<_, _>>()
            .map_err(error)?
    };
    for (id, pane) in open {
        if !live.contains(&pane.as_str()) {
            connection
                .execute(
                    "UPDATE team_agents SET status='stopped', stopped_ms=?2 WHERE id=?1",
                    params![id, at],
                )
                .map_err(error)?;
        }
    }
    keep_worktrees(connection, project, lead)
}

/// Keeps the worktree each running teammate row of a lead works in (DEC-111 Q17, OP-9):
/// `<lead clone>/.worktrees/<name>`, relative to the owner's checkout root as every other
/// team worktree is stored, while the lead's clone records it on the teammate's own branch
/// `<task branch>--<name>`, else none, which the graph shows in amber. The clone is read
/// as data with no git run; one that cannot be read holds none. Inputs: the connection,
/// the project and the lead. Output: any SQLite failure.
pub(super) fn keep_worktrees(
    connection: &Connection,
    project: &ProjectId,
    lead: i64,
) -> Result<(), EngineError> {
    let place: Option<(String, String, String)> = connection
        .query_row(
            "SELECT repository,worktree,git_dir FROM team_worktrees WHERE agent=?1 AND project=?2",
            params![lead, project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(error)?;
    let own = place
        .as_ref()
        .and_then(|(_, worktree, git_dir)| {
            crate::landing::teammate_worktrees(connection, Path::new(worktree), Path::new(git_dir))
                .ok()
        })
        .unwrap_or_default();
    for (teammate, _) in working(connection, project, lead)? {
        let folder = name(connection, teammate)
            .filter(|kept| own.contains(kept))
            .zip(place.as_ref())
            .and_then(|(kept, (repository, worktree, _))| {
                Path::new(worktree)
                    .join(crate::agent_git::TEAMMATES_FOLDER)
                    .join(kept)
                    .strip_prefix(repository)
                    .ok()?
                    .to_str()
                    .map(str::to_owned)
            });
        connection
            .execute(
                "UPDATE team_agents SET worktree=?2 WHERE id=?1 AND worktree IS NOT ?2",
                params![teammate, folder],
            )
            .map_err(error)?;
    }
    Ok(())
}

/// Keeps the name a native teammate's row is known by: the first one its lead's team config
/// gave its pane when the engine saw it live, kept only when it names a worktree and no
/// other live teammate of the lead holds it (DEC-111 Q18, RH-2). The config is the agents'
/// own file, so a later rename there changes nothing. Inputs: the connection, the lead,
/// the teammate row and the name. Output: any SQLite failure.
fn keep_name(
    connection: &Connection,
    lead: i64,
    teammate: i64,
    name: &str,
) -> Result<(), EngineError> {
    if !crate::agent_git::worktree_name_valid(name) {
        return Ok(());
    }
    connection
        .execute(
            "INSERT OR IGNORE INTO team_teammate_names(agent,name)
             SELECT ?1,?2 WHERE NOT EXISTS (
                 SELECT 1 FROM team_teammate_names n JOIN team_agents a ON a.id=n.agent
                 WHERE a.parent=?3 AND a.role='teammate'
                 AND a.status IN ('starting','idle','working') AND n.name=?2 AND n.agent<>?1)",
            params![teammate, name, lead],
        )
        .map_err(error)?;
    Ok(())
}

/// The name the engine keeps for a native teammate's row (DEC-111 Q18, RH-2). Inputs: the
/// connection and the row. Output: the name, or none, also when unread.
pub(super) fn name(connection: &Connection, teammate: i64) -> Option<String> {
    connection
        .query_row(
            "SELECT name FROM team_teammate_names WHERE agent=?1",
            [teammate],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten()
}

/// The lead of one team row now, so a teammate's land held for a worker's open change set
/// is tried again once that worker is re-parented or its row removed (TEST-ROWS-B-R1-L1).
/// Inputs: the connection and the agent. Output: its lead, None for a row with none or no
/// row, or a SQLite failure.
pub(super) fn parent(connection: &Connection, agent: i64) -> Result<Option<i64>, EngineError> {
    let parent: Option<Option<i64>> = connection
        .query_row(
            "SELECT parent FROM team_agents WHERE id=?1",
            [agent],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    Ok(parent.flatten())
}

/// Lists a lead's running teammate rows with whether each rests, so the engine commits a
/// resting teammate's turn and lands its work on the lead (OP-9b). Inputs: the connection,
/// the project and the lead. Output: the rows, true for a resting one, or a SQLite failure.
pub(super) fn resting(
    connection: &Connection,
    project: &ProjectId,
    lead: i64,
) -> Result<Vec<(i64, bool)>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT id,status='idle' FROM team_agents WHERE project=?1 AND parent=?2
             AND role='teammate' AND status IN ('starting','idle','working')
             ORDER BY id LIMIT ?3",
        )
        .map_err(error)?;
    let rows: Vec<(i64, bool)> = statement
        .query_map(params![project.as_str(), lead, MEMBERS_MAX as i64], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    Ok(rows)
}

/// Lists a lead's teammate rows that stopped or failed, the newest row of each name only
/// and only while no running row of the lead holds that name ([`crate::team::ENDED_TEAMMATE`]),
/// so the engine saves the edits such a teammate left in its worktree, which no rest commits
/// (team-20 r1 M1), each with whether the lead's finish asked for another save (team-20 r2
/// R2-M1). Inputs: the connection, the project and the lead. Output: at most `MEMBERS_MAX`
/// rows, newest first, or a SQLite failure.
pub(super) fn ended(
    connection: &Connection,
    project: &ProjectId,
    lead: i64,
) -> Result<Vec<(i64, bool)>, EngineError> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT a.id, IFNULL(n.save_refused='',0) FROM team_agents a
             JOIN team_teammate_names n ON n.agent=a.id
             WHERE a.project=?1 AND a.parent=?2 AND a.role='teammate' AND {}
             ORDER BY a.id DESC LIMIT ?3",
            crate::team::ENDED_TEAMMATE
        ))
        .map_err(error)?;
    let rows: Vec<(i64, bool)> = statement
        .query_map(params![project.as_str(), lead, MEMBERS_MAX as i64], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    assert!(rows.len() <= MEMBERS_MAX);
    Ok(rows)
}

/// Keeps why the engine last gave up saving a native teammate's edits, or clears it once a
/// save went through or found no worktree, so the lead's finish can name the cause and ask
/// for another save (team-20 r2 R2-M1). Inputs: the connection, the teammate and the
/// non-empty plain code, None to clear. Output: Ok, or a SQLite failure.
pub(super) fn save_refused(
    connection: &Connection,
    teammate: i64,
    code: Option<&str>,
) -> Result<(), EngineError> {
    connection
        .execute(
            "UPDATE team_teammate_names SET save_refused=?2 WHERE agent=?1",
            params![teammate, code],
        )
        .map_err(error)?;
    Ok(())
}

/// Lists a lead's running teammate rows with their pane ids, so the terminal host learns
/// each teammate's own name for its pane (`Host::adopt_pane`) on every discovery pass.
/// Inputs: the connection, the project and the lead. Output: the rows, or a SQLite failure.
pub(super) fn working(
    connection: &Connection,
    project: &ProjectId,
    lead: i64,
) -> Result<Vec<(i64, String)>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT id,pane FROM team_agents WHERE project=?1 AND parent=?2 AND role='teammate'
             AND pane IS NOT NULL AND status IN ('starting','idle','working')
             ORDER BY id LIMIT ?3",
        )
        .map_err(error)?;
    let rows: Vec<(i64, String)> = statement
        .query_map(params![project.as_str(), lead, MEMBERS_MAX as i64], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    assert!(rows.len() <= MEMBERS_MAX);
    Ok(rows)
}

/// Stops every teammate row whose lead no longer runs, so no teammate outlives its lead in
/// the agents list or the graph, with the reason the lead's own end gives. Inputs: the
/// connection and the time. Output: any SQLite failure.
pub(super) fn end_orphans(connection: &Connection, at: i64) -> Result<(), EngineError> {
    connection
        .execute(
            "UPDATE team_agents SET status='stopped', task=NULL, error=?2, stopped_ms=?1
             WHERE role='teammate' AND status IN ('starting','idle','working')
             AND (parent IS NULL OR parent NOT IN
               (SELECT id FROM team_agents WHERE role='sub' AND status IN ('starting','idle','working')))",
            params![at, super::LEAD_ENDED_TEAMMATE],
        )
        .map_err(error)?;
    Ok(())
}

#[cfg(all(test, unix))]
#[path = "teammates/tests.rs"]
mod tests;
