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
/// live gets a row. A row stopped in this run of the lead is marked working again only when
/// the engine closed or the pane went missing; one the owner stopped, the cap ended or that
/// ended with its lead stays stopped while its pane is ended. A teammate row whose pane is no
/// longer live is stopped. Only panes the terminal host lists as the lead's live teammate
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
    let row: Option<(String, bool, Option<String>)> = connection
        .query_row(
            "SELECT harness,tools,connection FROM team_agents
             WHERE id=?1 AND project=?2 AND role='sub' AND parent IS NULL",
            params![lead, project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((harness, tools, account)) = row else {
        return Ok(());
    };
    for member in members(home)
        .into_iter()
        .filter(|member| live.contains(&member.pane.as_str()))
    {
        let known: Option<(i64, bool)> = connection
            .query_row(
                "SELECT id, status IN ('starting','idle','working')
                        OR error IS NULL OR error NOT IN (?5,?6,?7)
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
                        "UPDATE team_agents SET status='working', error=NULL, stopped_ms=NULL,
                         heartbeat_ms=?2 WHERE id=?1",
                        params![id, at],
                    )
                    .map_err(error)?;
            }
            None => {
                connection
                    .execute(
                        "INSERT INTO team_agents(project,role,harness,tools,connection,status,created_ms,heartbeat_ms,parent,pane)
                         VALUES (?1,'teammate',?2,?3,?4,'working',?5,?5,?6,?7)",
                        params![project.as_str(), harness, tools, account, at, lead, member.pane],
                    )
                    .map_err(error)?;
                let id = connection.last_insert_rowid();
                let _ = names::assign_role(connection, project, id, &member.name)?;
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
    Ok(())
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
