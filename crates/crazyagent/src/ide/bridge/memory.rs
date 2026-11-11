use super::{Decision, IdeReply};
use crate::engine::EngineError;
use crate::engine::owner::{OwnerData, OwnerRead, OwnerRuntimeHandle, failure};
use crate::memory::{HumanReview, KnowledgeEntry, KnowledgeStatus};
use crate::protocol::{
    ClientId, Command, Cursor, Outcome, PROTOCOL_VERSION, ProjectId, RequestId, WireReply,
    WireRequest,
};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};

pub const STALE: &str = "state_stale";
pub const UNKNOWN: &str = "ide_memory_unknown";
pub const INVALID: &str = "ide_request_invalid";
pub const REPLY: &str = "ide_reply_invalid";
const CLIENT: &str = "crazyagent-ide";
const ASK_TTL_MS: i64 = 10 * 60 * 1000;
const ASKS_PER_PROJECT_MAX: i64 = 8;

/// Where one memory stands: waiting for the owner, approved, or set aside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryState {
    Pending,
    Approved,
    Rejected,
    Removed,
    Retracted,
}

/// One memory as the IDE sidebar shows it: the id, the revision a decision must name, the
/// text, its state and whether the owner already reviewed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdeMemory {
    pub id: String,
    pub revision: u64,
    pub text: String,
    pub state: MemoryState,
    pub reviewed: bool,
}

/// The arguments of `ide.memory_decide`: the memory, the revision the owner saw and the
/// decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decide {
    pub id: RequestId,
    pub revision: u64,
    pub decision: Decision,
}

/// Who wrote a memory's current revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Writer {
    Owner,
    Agent,
}

/// A decision that named an older revision: the revision it named, the current one to
/// read again and who wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stale {
    pub version: u64,
    pub current: u64,
    pub writer: Writer,
}

/// What a checked decision does next: ask the owner to confirm it in the desktop app, or
/// the stale reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Ask {
        entry_id: RequestId,
        version: u64,
        decision: Decision,
    },
    Stale(Stale),
}

/// One decision the IDE asked the owner to confirm in the desktop app: the memory at the
/// asked revision and whether to approve or reject it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdeAsk {
    pub entry: KnowledgeEntry,
    pub decision: Decision,
}

/// The owner read `ide.memories` runs: one engine batch of the project's memories after
/// the id of the last one the sidebar already holds, or the first batch. Takes the project
/// and that id. Returns the read.
pub fn list(project: &ProjectId, after: Option<RequestId>) -> OwnerRead {
    OwnerRead::Knowledge {
        review: HumanReview::Inbox {
            project_id: project.clone(),
            after,
        },
    }
}

/// The sidebar's memories from the engine's reply to `list`, in id order. Takes the
/// project and the reply. Returns the memories, or `ide_reply_invalid` for another reply
/// or a memory of another project.
pub fn listed(project: &ProjectId, data: &OwnerData) -> Result<Vec<IdeMemory>, &'static str> {
    entries(data)?
        .iter()
        .map(|entry| {
            if &entry.project_id != project {
                return Err(REPLY);
            }
            Ok(memory(entry))
        })
        .collect()
}

/// The owner read that checks a decision before it is written: the memory's revisions
/// from the one the owner saw. Takes the decision. Returns the read, or
/// `ide_request_invalid` for revision 0, which no memory has.
pub fn check(decide: &Decide) -> Result<OwnerRead, &'static str> {
    if decide.revision == 0 {
        return Err(INVALID);
    }
    Ok(OwnerRead::Knowledge {
        review: HumanReview::History {
            entry_id: decide.id.clone(),
            after_version: decide.revision - 1,
        },
    })
}

/// Plans a decision from the engine's reply to `check`: the memory must be this project's
/// and the revision the owner saw must be its newest, else the reply is stale. No decision
/// is written from the IDE: Approve and Reject each become an ask the owner confirms in the
/// desktop app, which decides through its own owner review. Takes the project, the decision
/// and the reply. Returns the plan, `ide_memory_unknown` for a memory this project does not
/// have at that revision, or `ide_reply_invalid` for another reply.
pub fn plan(project: &ProjectId, decide: &Decide, data: &OwnerData) -> Result<Plan, &'static str> {
    let entries = entries(data)?;
    let (Some(seen), Some(newest)) = (entries.first(), entries.last()) else {
        return Err(UNKNOWN);
    };
    if entries.iter().any(|entry| &entry.project_id != project) || seen.version != decide.revision {
        return Err(UNKNOWN);
    }
    if newest.version != decide.revision {
        return Ok(Plan::Stale(Stale {
            version: decide.revision,
            current: newest.version,
            writer: writer(newest),
        }));
    }
    Ok(Plan::Ask {
        entry_id: decide.id.clone(),
        version: decide.revision,
        decision: decide.decision,
    })
}

/// The wire reply for a stale decision, with code `state_stale`. Takes the stale facts.
/// Returns the reply.
pub fn stale_reply(stale: &Stale) -> IdeReply {
    IdeReply::Stale {
        code: STALE.to_owned(),
        version: stale.version,
        current: serde_json::json!(stale.current),
        writer: match stale.writer {
            Writer::Owner => "owner",
            Writer::Agent => "agent",
        }
        .to_owned(),
    }
}

/// Answers `ide.memories` through the engine: one batch of the project's memories after
/// the last id the sidebar holds. Takes the owner runtime, the project and that id.
/// Returns the memories as JSON, or the engine's refusal.
pub(crate) async fn memories(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
    after: Option<RequestId>,
) -> Result<serde_json::Value, EngineError> {
    let data = read(runtime, list(project, after)).await?;
    let shown = listed(project, &data).map_err(failure)?;
    serde_json::to_value(shown).map_err(|_| failure(REPLY))
}

/// Answers `ide.memory_decide`: checks the revision the owner saw, then records the
/// approval or rejection as an ask for the desktop app's confirm (`{"confirm": "desktop"}`).
/// Takes the owner runtime, the project and the decision. Returns `ok`, the stale reply, or
/// a refusal such as `ide_memory_unknown`.
pub(crate) async fn decide(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
    decide: &Decide,
) -> Result<IdeReply, EngineError> {
    let data = read(runtime, check(decide).map_err(failure)?).await?;
    match plan(project, decide, &data).map_err(failure)? {
        Plan::Stale(stale) => Ok(stale_reply(&stale)),
        Plan::Ask {
            entry_id,
            version,
            decision,
        } => {
            asked(runtime, project, entry_id, version, decision).await?;
            Ok(IdeReply::Ok {
                data: serde_json::json!({ "confirm": "desktop" }),
            })
        }
    }
}

/// Creates the table of decisions the IDE asked the owner to confirm in the desktop app,
/// and adds the decision column to a table an earlier build made without it; every ask of
/// that shape was an approval, so its rows read as approvals. Takes the engine connection.
/// Returns Ok, or `ide_asks_unavailable`.
#[cfg(unix)]
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    let unavailable = |_| failure("ide_asks_unavailable");
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS ide_asks (
                project TEXT NOT NULL,
                entry TEXT NOT NULL,
                version INTEGER NOT NULL CHECK (version > 0),
                decision TEXT NOT NULL CHECK (decision IN ('approve','reject')),
                asked_ms INTEGER NOT NULL,
                PRIMARY KEY (project, entry)
            ) WITHOUT ROWID;",
        )
        .map_err(unavailable)?;
    let decided: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('ide_asks') WHERE name='decision')",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !decided {
        connection
            .execute_batch(
                "ALTER TABLE ide_asks ADD COLUMN decision TEXT NOT NULL DEFAULT 'approve'
                 CHECK (decision IN ('approve','reject'));",
            )
            .map_err(unavailable)?;
    }
    Ok(())
}

fn decision_name(decision: Decision) -> &'static str {
    match decision {
        Decision::Approve => "approve",
        Decision::Reject => "reject",
    }
}

/// Records that the IDE asked to approve or reject one memory revision, replacing an older
/// ask for the same memory, dropping asks older than `ASK_TTL_MS` and keeping the project's
/// newest `ASKS_PER_PROJECT_MAX`. Takes the connection, project, memory, revision, decision
/// and time. Returns Ok, or `ide_asks_unavailable`.
pub fn ask(
    connection: &Connection,
    project: &ProjectId,
    entry_id: &RequestId,
    version: u64,
    decision: Decision,
    now_ms: i64,
) -> Result<(), EngineError> {
    let unavailable = |_| failure("ide_asks_unavailable");
    let version = i64::try_from(version).map_err(|_| failure(INVALID))?;
    let write = connection.unchecked_transaction().map_err(unavailable)?;
    write
        .execute(
            "DELETE FROM ide_asks WHERE asked_ms < ?1",
            [now_ms.saturating_sub(ASK_TTL_MS)],
        )
        .map_err(unavailable)?;
    write
        .execute(
            "INSERT INTO ide_asks(project,entry,version,decision,asked_ms) VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(project,entry) DO UPDATE SET version=excluded.version,
                decision=excluded.decision, asked_ms=excluded.asked_ms",
            params![
                project.as_str(),
                entry_id.as_str(),
                version,
                decision_name(decision),
                now_ms
            ],
        )
        .map_err(unavailable)?;
    write
        .execute(
            "DELETE FROM ide_asks WHERE project=?1 AND entry NOT IN
             (SELECT entry FROM ide_asks WHERE project=?1 ORDER BY asked_ms DESC, entry LIMIT ?2)",
            params![project.as_str(), ASKS_PER_PROJECT_MAX],
        )
        .map_err(unavailable)?;
    write.commit().map_err(unavailable)
}

/// The decisions the IDE asked the owner to confirm on memories that still wait at the
/// asked revision, oldest ask first, at most `ASKS_PER_PROJECT_MAX`; the desktop app
/// confirms each through its own owner review. Takes the connection, project and time.
/// Returns the asks.
pub fn asks(
    connection: &Connection,
    project: &ProjectId,
    now_ms: i64,
) -> Result<Vec<IdeAsk>, EngineError> {
    let unavailable = |_| failure("ide_asks_unavailable");
    let mut statement = connection
        .prepare(
            "SELECT e.body, a.decision FROM ide_asks a
             JOIN knowledge_heads h ON h.id=a.entry AND h.version=a.version AND h.project=a.project
             JOIN knowledge_entries e ON e.id=h.id AND e.version=h.version
             WHERE a.project=?1 AND a.asked_ms>=?2 ORDER BY a.asked_ms, a.entry LIMIT ?3",
        )
        .map_err(unavailable)?;
    let bodies = statement
        .query_map(
            params![
                project.as_str(),
                now_ms.saturating_sub(ASK_TTL_MS),
                ASKS_PER_PROJECT_MAX
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(unavailable)?;
    let mut waiting = Vec::with_capacity(bodies.len());
    for (body, decision) in bodies {
        let entry: KnowledgeEntry = serde_json::from_str(&body).map_err(|_| failure(REPLY))?;
        let decision = match decision.as_str() {
            "approve" => Decision::Approve,
            "reject" => Decision::Reject,
            _ => return Err(failure(REPLY)),
        };
        if entry.status == KnowledgeStatus::Provisional {
            waiting.push(IdeAsk { entry, decision });
        }
    }
    Ok(waiting)
}

/// Records an ask on the engine database, off the async runtime. Takes the owner runtime,
/// project, memory, revision and decision. Returns Ok, or `ide_asks_unavailable`.
async fn asked(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
    entry_id: RequestId,
    version: u64,
    decision: Decision,
) -> Result<(), EngineError> {
    let database = super::agents::database(runtime)?;
    let project = project.clone();
    tokio::task::spawn_blocking(move || {
        let connection = super::agents::open(&database, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        ask(
            &connection,
            &project,
            &entry_id,
            version,
            decision,
            crate::team::now_ms(),
        )
    })
    .await
    .map_err(|_| failure("ide_asks_unavailable"))?
}

/// Runs one owner read in the engine. Takes the runtime and the read. Returns the data,
/// or `ide_reply_invalid` for a read with no command.
async fn read(runtime: &OwnerRuntimeHandle, query: OwnerRead) -> Result<OwnerData, EngineError> {
    let command = query.command()?.ok_or_else(|| failure(REPLY))?;
    let reply = run(runtime, command).await?;
    Ok(OwnerData::Domain {
        outcome: Box::new(reply.outcome),
    })
}

/// Sends one read command to the engine under a fresh request id. Takes the runtime and
/// the command. Returns the engine's reply.
async fn run(runtime: &OwnerRuntimeHandle, command: Command) -> Result<WireReply, EngineError> {
    let mut suffix = [0; 16];
    getrandom::fill(&mut suffix).map_err(|_| failure("ide_entropy_unavailable"))?;
    let request = WireRequest {
        protocol_version: PROTOCOL_VERSION,
        client_id: ClientId::new(CLIENT).map_err(|_| failure(REPLY))?,
        request_id: RequestId::new(format!("ide-{}", super::hex(&suffix)))
            .map_err(|_| failure(REPLY))?,
        expected_revision: Cursor::new(0),
        command,
    };
    runtime.ide_apply(request).await
}

fn entries(data: &OwnerData) -> Result<&[KnowledgeEntry], &'static str> {
    let OwnerData::Domain { outcome } = data else {
        return Err(REPLY);
    };
    let Outcome::Knowledge { entries } = outcome.as_ref() else {
        return Err(REPLY);
    };
    Ok(entries)
}

fn memory(entry: &KnowledgeEntry) -> IdeMemory {
    IdeMemory {
        id: entry.id.as_str().to_owned(),
        revision: entry.version,
        text: entry.text.clone(),
        state: match entry.status {
            KnowledgeStatus::Provisional => MemoryState::Pending,
            KnowledgeStatus::Confirmed => MemoryState::Approved,
            KnowledgeStatus::Rejected => MemoryState::Rejected,
            KnowledgeStatus::Removed => MemoryState::Removed,
            KnowledgeStatus::Retracted => MemoryState::Retracted,
        },
        reviewed: entry.human_reviewed,
    }
}

fn writer(entry: &KnowledgeEntry) -> Writer {
    if entry.human_reviewed {
        Writer::Owner
    } else {
        Writer::Agent
    }
}

#[cfg(all(test, unix))]
mod tests;
