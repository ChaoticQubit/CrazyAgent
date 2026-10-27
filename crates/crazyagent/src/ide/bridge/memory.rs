use super::{Decision, IdeEvent, IdeReply};
use crate::engine::EngineError;
use crate::engine::owner::{OwnerData, OwnerMutation, OwnerRead, OwnerRuntimeHandle, failure};
use crate::memory::{HumanReview, KnowledgeEntry, KnowledgeStatus, ReviewDecision};
use crate::protocol::{
    ClientId, Command, Cursor, Outcome, PROTOCOL_VERSION, ProjectId, RequestId, WireReply,
    WireRequest,
};
use serde::{Deserialize, Serialize};

pub const STALE: &str = "state_stale";
pub const UNKNOWN: &str = "ide_memory_unknown";
pub const INVALID: &str = "ide_request_invalid";
pub const REPLY: &str = "ide_reply_invalid";
const CONFLICT: &str = "knowledge_version_conflict";
const MOVED: &str = "stale_revision";
const CLIENT: &str = "crazyagent-ide";
const DECIDE_TRIES_MAX: usize = 3;

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

/// What a checked decision does next: the owner write to apply, or the stale reply.
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    Apply(OwnerMutation),
    Stale(Stale),
}

/// How an applied decision ended: applied, run `check` and `plan` again because the memory
/// or the engine moved on between the check and the write, or refused with the engine's code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decided {
    Applied,
    Recheck,
    Refused(String),
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
/// and the revision the owner saw must be its newest, else the reply is stale. Approve
/// keeps the memory through the engine's own review; reject rejects it there. Takes the
/// project, the decision and the reply. Returns the plan, `ide_memory_unknown` for a memory
/// this project does not have at that revision, or `ide_reply_invalid` for another reply.
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
    let decision = match decide.decision {
        Decision::Approve => ReviewDecision::Keep,
        Decision::Reject => ReviewDecision::Reject,
    };
    Ok(Plan::Apply(OwnerMutation::Knowledge {
        review: HumanReview::Review {
            entry_id: decide.id.clone(),
            version: decide.revision,
            decision,
        },
    }))
}

/// Reads the end of an applied decision from the engine's outcome. Takes the outcome.
/// Returns applied, recheck for a memory or engine revision that changed after the check,
/// or the refusal.
pub fn decided(outcome: &Outcome) -> Decided {
    match outcome {
        Outcome::Applied => Decided::Applied,
        Outcome::Rejected { code } if code == CONFLICT || code == MOVED => Decided::Recheck,
        Outcome::Rejected { code } => Decided::Refused(code.clone()),
        _ => Decided::Refused(REPLY.to_owned()),
    }
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
    let (_, data) = read(runtime, list(project, after)).await?;
    let shown = listed(project, &data).map_err(failure)?;
    serde_json::to_value(shown).map_err(|_| failure(REPLY))
}

/// Answers `ide.memory_decide` through the engine's own owner review: checks the revision
/// the owner saw, writes the decision at the engine revision it read, and checks again when
/// either moved in between, at most `DECIDE_TRIES_MAX` times. Tells the project's IDE the
/// memories changed. Takes the owner runtime, the project and the decision. Returns `ok`,
/// the stale reply, or a refusal such as `ide_memory_unknown` or `ide_memory_busy`.
pub(crate) async fn decide(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
    decide: &Decide,
) -> Result<IdeReply, EngineError> {
    let query = check(decide).map_err(failure)?;
    for _ in 0..DECIDE_TRIES_MAX {
        let (revision, data) = read(runtime, query.clone()).await?;
        let mutation = match plan(project, decide, &data).map_err(failure)? {
            Plan::Stale(stale) => return Ok(stale_reply(&stale)),
            Plan::Apply(mutation) => mutation,
        };
        match decided(&run(runtime, revision, mutation.command()?).await?.outcome) {
            Decided::Applied => {
                super::publish(runtime, project, IdeEvent::MemoriesChanged);
                return Ok(IdeReply::Ok {
                    data: serde_json::json!({}),
                });
            }
            Decided::Recheck => continue,
            Decided::Refused(code) => return Err(failure(&code)),
        }
    }
    Err(failure("ide_memory_busy"))
}

/// Runs one owner read in the engine. Takes the runtime and the read. Returns the engine
/// revision it saw and the data, or `ide_reply_invalid` for a read with no command.
async fn read(
    runtime: &OwnerRuntimeHandle,
    query: OwnerRead,
) -> Result<(Cursor, OwnerData), EngineError> {
    let command = query.command()?.ok_or_else(|| failure(REPLY))?;
    let reply = run(runtime, Cursor::new(0), command).await?;
    Ok((
        reply.revision,
        OwnerData::Domain {
            outcome: Box::new(reply.outcome),
        },
    ))
}

/// Sends one command to the engine under a fresh request id. Takes the runtime, the
/// engine revision a write expects, and the command. Returns the engine's reply.
async fn run(
    runtime: &OwnerRuntimeHandle,
    expected_revision: Cursor,
    command: Command,
) -> Result<WireReply, EngineError> {
    let mut suffix = [0; 16];
    getrandom::fill(&mut suffix).map_err(|_| failure("ide_entropy_unavailable"))?;
    let request = WireRequest {
        protocol_version: PROTOCOL_VERSION,
        client_id: ClientId::new(CLIENT).map_err(|_| failure(REPLY))?,
        request_id: RequestId::new(format!("ide-{}", super::hex(&suffix)))
            .map_err(|_| failure(REPLY))?,
        expected_revision,
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
