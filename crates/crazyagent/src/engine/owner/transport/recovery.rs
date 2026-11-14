use super::*;
use crate::ledger::{self, Action as Recorded, ActionKind, Outcome};
use crate::protocol::{ProjectId, encode_frame};
use crate::recovery::{self, Interrupted, RunState};
use crate::team::AgentRole;
use crate::team::names;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::sync::OwnedRwLockWriteGuard;

const REQUEST_BYTES_MAX: usize = 1024;
/// The most runs one banner lists, so its reply fits one owner frame.
pub const BANNER_RUNS_MAX: usize = 32;
/// The most notices one banner lists, so its reply fits one owner frame.
pub const BANNER_NOTICES_MAX: usize = 32;
const NOTICE_KINDS: [ActionKind; 4] = [
    ActionKind::AgentSpawn,
    ActionKind::WorktreeCreate,
    ActionKind::LandingAccept,
    ActionKind::ProjectDelete,
];
pub(super) const RECOVERY_TIMEOUT: Duration = Duration::from_secs(30);
const REPLY_WRITE_TIME: Duration = Duration::from_secs(2);

/// Every code a Recovery request can answer an owner client with: the ledger's refusals,
/// this protocol's own, and the owner connection's it travels on.
pub const RECOVERY_CODES: &[&str] = &[
    "ledger_finished",
    "ledger_missing",
    "ledger_not_notice",
    "ledger_unavailable",
    "owner_authentication_failed",
    "owner_authority_unavailable",
    "owner_busy",
    "owner_grant_stale_or_revoked",
    "owner_request_failed",
    "owner_root_replaced_or_unsafe",
    "owner_root_unavailable",
    "owner_socket_replaced",
    "owner_socket_unsafe",
    "peer_owner_mismatch",
    "recovery_not_failed",
    "recovery_outcome_unknown",
    "recovery_record_invalid",
    "recovery_reply_bounds",
    "recovery_request_bounds",
    "recovery_request_invalid",
    "recovery_unavailable",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Dismiss { action_kind: ActionKind, id: i64 },
    List { project: ProjectId },
    Retry { agent: i64 },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    recovery: Action,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Dismissed,
    Listed { banner: RecoveryBanner },
    Retried,
    Rejected { code: String },
}

/// Where the resume of one interrupted run stands, as the owner is shown it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunShown {
    Interrupted,
    Resuming,
    Resumed,
    Failed,
}

/// What a notice tells the owner happened: an agent start or worktree add whose outcome
/// could not be told, or an Accept an engine stop cut short that landed or was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeShown {
    Unknown,
    Landed,
    Dropped,
}

/// One agent run an engine stop cut short, as the banner lists it: the team agent, its
/// name, whether it is the master, its harness as the owner knows it, when it was last
/// seen running in ms since the epoch, its turn and the first line it was asked, where
/// its resume stands, why a failed resume failed, and whether its team is paused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BannerRun {
    pub agent: i64,
    pub label: String,
    pub role: AgentRole,
    pub harness: String,
    pub at_ms: i64,
    pub turn: i64,
    pub asked: Option<String>,
    pub state: RunShown,
    pub reason: Option<String>,
    pub paused: bool,
}

/// One ledger notice the owner dismisses in the banner: its kind and number, which a
/// dismiss names, the team agent and its name or the change set it is about, what it shows,
/// and when its action began in ms since the epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BannerNotice {
    pub action_kind: ActionKind,
    pub id: i64,
    pub agent: Option<i64>,
    pub label: Option<String>,
    pub change_set: Option<String>,
    pub shown: NoticeShown,
    pub started_ms: i64,
}

/// One project's interrupted work: its runs and notices, oldest first, and how many open
/// items did not fit. Runs holds the runs still waiting on their resume or the owner's
/// retry, however old, and the settled runs, resumed or given up, since the oldest of those;
/// it is empty when no run waits. Nothing in either list means the owner has nothing to
/// resolve and no banner shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryBanner {
    pub runs: Vec<BannerRun>,
    pub notices: Vec<BannerNotice>,
    pub more: u32,
}

/// Names the requests that only read, for the log of replies this client could not read.
/// Input: the request. Output: its name, None for a request that changes something.
fn read_kind(action: &Action) -> Option<&'static str> {
    match action {
        Action::List { .. } => Some("recovery.list"),
        _ => None,
    }
}

fn unknown() -> EngineError {
    failure("recovery_outcome_unknown")
}

impl OwnerClient {
    /// Reads one project's interrupted work for its recovery banner. Input: the project.
    /// Output: its runs and the ledger notices waiting for the owner.
    pub async fn recovery_banner(&self, project: ProjectId) -> Result<RecoveryBanner, EngineError> {
        match self.recovery_exchange(Action::List { project }).await? {
            Reply::Listed { banner } => Ok(banner),
            _ => Err(unknown()),
        }
    }

    /// Starts a failed resume again: the agent's newest failed run waits to resume on the
    /// team runtime's next tick. Input: the team agent. Output: Ok once it waits, or
    /// `recovery_not_failed` when the agent has no failed resume.
    pub async fn recovery_retry(&self, agent: i64) -> Result<(), EngineError> {
        match self.recovery_exchange(Action::Retry { agent }).await? {
            Reply::Retried => Ok(()),
            _ => Err(unknown()),
        }
    }

    /// Dismisses one recovery notice the owner was shown: the engine closes it with the
    /// outcome it showed, and the agent start it held back may run again. A pending action
    /// a check still settles is refused with `ledger_not_notice`. Inputs: the notice's
    /// kind and number from the ledger. Output: Ok once it is dismissed.
    pub async fn recovery_dismiss(&self, kind: ActionKind, id: i64) -> Result<(), EngineError> {
        let action = Action::Dismiss {
            action_kind: kind,
            id,
        };
        match self.recovery_exchange(action).await? {
            Reply::Dismissed => Ok(()),
            _ => Err(unknown()),
        }
    }

    async fn recovery_exchange(&self, action: Action) -> Result<Reply, EngineError> {
        tokio::time::timeout(self.timeout.max(RECOVERY_TIMEOUT), async {
            let read = read_kind(&action);
            let frame = encode_frame(&Envelope { recovery: action })
                .map_err(|_| failure("recovery_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("recovery_unavailable"))?;
            verify_peer(&stream)?;
            let mut packet = [0; 37];
            packet[..4].copy_from_slice(b"CAO1");
            packet[4] = match self.kind {
                OwnerClientKind::Tui => 0,
                OwnerClientKind::Desktop => 1,
            };
            packet[5..].copy_from_slice(&self.token);
            stream
                .write_all(&packet)
                .await
                .map_err(|_| failure("recovery_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("recovery_unavailable"))?;
            let frame = read_frame(&mut stream).await.map_err(|_| unknown())?;
            match serde_json::from_slice(&frame[4..])
                .map_err(|error| self.skewed_reply(&error, read, unknown))?
            {
                Reply::Rejected { code } => Err(EngineError(code)),
                reply => Ok(reply),
            }
        })
        .await
        .map_err(|_| unknown())?
    }
}

/// Encodes this protocol's refusal of a request the engine did not run.
/// Input: the refusal code. Output: the reply frame.
pub(super) fn refusal(code: &str) -> Result<Vec<u8>, EngineError> {
    encode_frame(&Reply::Rejected { code: code.into() }).map_err(|_| unknown())
}

/// Serves one Recovery request past the owner exchange, under RECOVERY_TIMEOUT. The owner
/// barrier is waited for at most the engine timeout, then held while the grant is checked
/// again, the action runs and the reply is written within REPLY_WRITE_TIME, so a revocation
/// that returned saw every dismissal admitted before it and lets none through after. Inputs:
/// the connection, engine, authority, packet, admission and body. Output: done once the
/// reply is written.
pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    let action = parse(bytes);
    let barrier = Arc::clone(&engine.owner_egress).write_owned();
    let (reply, _barrier) = match tokio::time::timeout(engine.timeout, barrier).await {
        Ok(barrier) => answer(authority, packet, admission, action, barrier).await,
        Err(_) => (rejected(failure("owner_busy")), None),
    };
    let frame = match encode_frame(&reply) {
        Ok(frame) => frame,
        Err(_) => encode_frame(&Reply::Rejected {
            code: "recovery_reply_bounds".into(),
        })
        .map_err(|_| unknown())?,
    };
    tokio::time::timeout(REPLY_WRITE_TIME, stream.write_all(&frame))
        .await
        .map_err(|_| unknown())?
        .map_err(io_error)
}

/// Answers a parsed request under the owner barrier it is handed: checks the grant again,
/// then runs the action on a blocking thread that owns the barrier until the action
/// returns, so a request cut short never lets a revocation pass a dismissal still
/// committing. Inputs: the authority, packet, admission, parsed request and barrier.
/// Output: the reply or refusal, with the barrier to hold while it is written; no barrier
/// when the blocking thread failed.
async fn answer(
    authority: &Authority,
    packet: &[u8; 37],
    admission: &Admission,
    action: Result<Action, EngineError>,
    barrier: OwnedRwLockWriteGuard<()>,
) -> (Reply, Option<OwnedRwLockWriteGuard<()>>) {
    let admitted = action.and_then(|action| {
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
        Ok((super::changes::engine_root(authority)?, action))
    });
    let (root, action) = match admitted {
        Ok(admitted) => admitted,
        Err(error) => return (rejected(error), Some(barrier)),
    };
    match tokio::task::spawn_blocking(move || (run(&root, action), barrier)).await {
        Ok((result, barrier)) => (result.unwrap_or_else(rejected), Some(barrier)),
        Err(_) => (rejected(failure("recovery_unavailable")), None),
    }
}

/// The refusal reply for one engine error. Input: the error. Output: the reply.
fn rejected(error: EngineError) -> Reply {
    Reply::Rejected { code: error.0 }
}

/// Reads one Recovery request from an owner frame body, at most REQUEST_BYTES_MAX bytes.
/// Input: the body. Output: the action, or `recovery_request_bounds` or
/// `recovery_request_invalid`.
fn parse(bytes: &[u8]) -> Result<Action, EngineError> {
    if bytes.len() > REQUEST_BYTES_MAX {
        return Err(failure("recovery_request_bounds"));
    }
    let Envelope { recovery } =
        serde_json::from_slice(bytes).map_err(|_| failure("recovery_request_invalid"))?;
    Ok(recovery)
}

/// Runs one authenticated Recovery action against the recovery store and action ledger in
/// the engine root. Inputs: the engine root and the action. Output: the reply, or the
/// plain refusal.
fn run(root: &Path, action: Action) -> Result<Reply, EngineError> {
    match action {
        Action::Dismiss { action_kind, id } => {
            ledger::dismiss(&ledger::open(root)?, action_kind, id)?;
            Ok(Reply::Dismissed)
        }
        Action::List { project } => Ok(Reply::Listed {
            banner: banner(root, &project)?,
        }),
        Action::Retry { agent } => {
            if agent <= 0 {
                return Err(failure("recovery_request_invalid"));
            }
            if !recovery::retry(root, agent)? {
                return Err(failure("recovery_not_failed"));
            }
            Ok(Reply::Retried)
        }
    }
}

/// Reads one project's banner: its oldest runs still waiting on their resume or the
/// owner's retry, the settled runs since the oldest of those, and the ledger notices about
/// its agents or change sets, each at most its maximum, with the waiting runs and notices
/// left out counted. Inputs: the engine root and the project. Output: the banner, empty when
/// nothing waits for the owner.
fn banner(root: &Path, project: &ProjectId) -> Result<RecoveryBanner, EngineError> {
    let listed = recovery::list(root, project)?;
    let mut more = listed.more + listed.waiting.len().saturating_sub(BANNER_RUNS_MAX);
    let mut chosen: Vec<&Interrupted> = listed.waiting.iter().take(BANNER_RUNS_MAX).collect();
    if let Some(oldest) = chosen.first().map(|run| run.id) {
        let room = BANNER_RUNS_MAX - chosen.len();
        chosen.extend(
            listed
                .settled
                .iter()
                .filter(|run| run.id > oldest)
                .rev()
                .take(room),
        );
        chosen.sort_by_key(|run| run.id);
    }
    let db = ledger::open(root)?;
    let runs = chosen
        .into_iter()
        .map(|run| banner_run(&db, run))
        .collect::<Result<Vec<_>, _>>()?;
    let mut notices = Vec::new();
    for kind in NOTICE_KINDS {
        for action in ledger::pending(&db, kind)? {
            notices.extend(banner_notice(&db, project, &action)?);
        }
    }
    notices.sort_by_key(|notice| notice.id);
    more += notices.len().saturating_sub(BANNER_NOTICES_MAX);
    notices.truncate(BANNER_NOTICES_MAX);
    assert!(runs.len() <= BANNER_RUNS_MAX && notices.len() <= BANNER_NOTICES_MAX);
    assert!(runs.is_empty() || runs.iter().any(|run| run.state != RunShown::Resumed));
    Ok(RecoveryBanner {
        runs,
        notices,
        more: u32::try_from(more).map_err(|_| failure("recovery_unavailable"))?,
    })
}

/// Names one team agent as the owner knows it. Inputs: the engine database, the agent and
/// its role. Output: its persona and handle, or "Master" or "Sub-agent <id>".
fn agent_label(db: &Connection, agent: i64, role: AgentRole) -> Result<String, EngineError> {
    let name = names::name(db, agent).map_err(|_| failure("recovery_unavailable"))?;
    Ok(names::label(role, agent, name.as_ref()))
}

/// Shapes one stored run for the banner. Inputs: the engine database and the run.
/// Output: the run as the owner is shown it.
fn banner_run(db: &Connection, run: &Interrupted) -> Result<BannerRun, EngineError> {
    Ok(BannerRun {
        agent: run.agent,
        label: agent_label(db, run.agent, run.kind)?,
        role: run.kind,
        harness: names::harness_name(&run.harness).to_owned(),
        at_ms: run.at,
        turn: run.last_turn.number,
        asked: run.last_turn.asked.clone(),
        state: match run.state {
            RunState::Interrupted => RunShown::Interrupted,
            RunState::Resuming => RunShown::Resuming,
            RunState::Resumed => RunShown::Resumed,
            RunState::Failed => RunShown::Failed,
        },
        reason: run.reason.clone(),
        paused: run.paused,
    })
}

/// Shapes one pending ledger action as a banner notice of one project. Inputs: the engine
/// database, the project and the action. Output: the notice, none for an intent a check
/// still settles or a notice of another project, or `recovery_record_invalid` for a notice
/// that names neither a team agent nor a project.
fn banner_notice(
    db: &Connection,
    project: &ProjectId,
    action: &Recorded,
) -> Result<Option<BannerNotice>, EngineError> {
    let Some(outcome) = ledger::notice(action) else {
        return Ok(None);
    };
    let invalid = || failure("recovery_record_invalid");
    let payload: serde_json::Value =
        serde_json::from_str(&action.payload).map_err(|_| invalid())?;
    let shown = match (action.kind, outcome) {
        (ActionKind::LandingAccept, Outcome::Done) => NoticeShown::Landed,
        (ActionKind::LandingAccept, Outcome::Failed) => NoticeShown::Dropped,
        (_, Outcome::Unknown) => NoticeShown::Unknown,
        _ => return Err(invalid()),
    };
    let mut notice = BannerNotice {
        action_kind: action.kind,
        id: action.id.get(),
        agent: None,
        label: None,
        change_set: None,
        shown,
        started_ms: action.started_ms,
    };
    if matches!(
        action.kind,
        ActionKind::LandingAccept | ActionKind::ProjectDelete
    ) {
        return project_notice(&payload, project, notice);
    }
    let agent = payload
        .get("team_agent")
        .and_then(serde_json::Value::as_i64)
        .filter(|agent| *agent > 0)
        .ok_or_else(invalid)?;
    let found: Option<(String, String)> = db
        .query_row(
            "SELECT project,role FROM team_agents WHERE id=?1",
            [agent],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|_| failure("recovery_unavailable"))?;
    let Some((owner, role)) = found else {
        return Err(invalid());
    };
    if owner != project.as_str() {
        return Ok(None);
    }
    let role = match role.as_str() {
        "master" => AgentRole::Master,
        "sub" => AgentRole::Sub,
        _ => return Err(invalid()),
    };
    notice.agent = Some(agent);
    notice.label = Some(agent_label(db, agent, role)?);
    Ok(Some(notice))
}

/// Scopes a notice that names its project in its payload: an interrupted Accept, which also
/// names its change set, or a delete an engine stop cut short. Inputs: the payload, the
/// project the banner is for and the notice. Output: the notice, none for another project's,
/// or `recovery_record_invalid` when the payload names no project or an Accept no change set.
fn project_notice(
    payload: &serde_json::Value,
    project: &ProjectId,
    mut notice: BannerNotice,
) -> Result<Option<BannerNotice>, EngineError> {
    let accept = notice.action_kind == ActionKind::LandingAccept;
    assert!(accept || notice.action_kind == ActionKind::ProjectDelete);
    assert!(notice.agent.is_none() && notice.change_set.is_none());
    let invalid = || failure("recovery_record_invalid");
    let owner = payload.get("project").and_then(serde_json::Value::as_str);
    let set = payload
        .get("change_set")
        .and_then(serde_json::Value::as_str);
    let Some(owner) = owner else {
        return Err(invalid());
    };
    if accept {
        notice.change_set = Some(set.ok_or_else(invalid)?.to_owned());
    }
    if owner != project.as_str() {
        return Ok(None);
    }
    Ok(Some(notice))
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
