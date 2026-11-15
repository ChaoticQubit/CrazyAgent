use super::*;
use crate::protocol::encode_frame;
use crate::roadmap_status::{self, Refresh, RoadmapStatus};
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = 1024;
const PAGE_BYTES_MAX: usize = 48 * 1024;
const ITEM_JSON_BYTES_MAX: usize = 2
    * (roadmap_status::SECTION_BYTES_MAX
        + roadmap_status::ITEM_BYTES_MAX
        + roadmap_status::EVIDENCE_BYTES_MAX)
    + 96;
const READS_MAX: usize = 3;
pub(super) const ROADMAP_TIMEOUT: Duration = Duration::from_secs(30);

const _: () = assert!(REQUEST_BYTES_MAX <= crate::protocol::FRAME_BYTES_MAX);
const _: () = assert!(ITEM_JSON_BYTES_MAX <= PAGE_BYTES_MAX);
const _: () = assert!(
    PAGE_BYTES_MAX + 4 * roadmap_status::NOTE_BYTES_MAX + 1024 <= crate::protocol::FRAME_BYTES_MAX
);

/// Every code a Roadmap request can answer an owner client with: the status store's
/// refusals, this protocol's own, and the owner connection's it travels on.
pub const ROADMAP_CODES: &[&str] = &[
    "changes_unavailable",
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
    "roadmap_status_changed",
    "roadmap_status_off",
    "roadmap_status_outcome_unknown",
    "roadmap_status_project_missing",
    "roadmap_status_reply_bounds",
    "roadmap_status_request_bounds",
    "roadmap_status_request_invalid",
    "roadmap_status_unavailable",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    ReadStatus {
        project: ProjectId,
        #[serde(default)]
        after: u32,
    },
    Refresh {
        project: ProjectId,
    },
    SetRefresh {
        project: ProjectId,
        refresh: Refresh,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    roadmap: Action,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Status {
        status: RoadmapStatus,
        next: Option<u32>,
    },
    Done {},
    Rejected {
        code: String,
    },
}

fn unknown() -> EngineError {
    failure("roadmap_status_outcome_unknown")
}

/// Names the requests that only read, for the log of replies this client could not read.
/// Input: the request. Output: its name, None for a request that changes something.
fn read_kind(action: &Action) -> Option<&'static str> {
    match action {
        Action::ReadStatus { .. } => Some("roadmap.read_status"),
        _ => None,
    }
}

impl OwnerClient {
    /// Reads a project's roadmap status: every item with its state, when it was checked,
    /// what is happening now and the refresh setting. The items come in pages one frame
    /// carries; a status that changes between two pages is read again, at most `READS_MAX`
    /// times. Input: the project. Output: the status, or `roadmap_status_changed`.
    pub async fn roadmap_status(&self, project: ProjectId) -> Result<RoadmapStatus, EngineError> {
        for _ in 0..READS_MAX {
            if let Some(status) = self.roadmap_pages(&project).await? {
                assert!(status.items.len() <= roadmap_status::ITEMS_MAX);
                return Ok(status);
            }
        }
        Err(failure("roadmap_status_changed"))
    }

    /// Reads every page of a project's status once. Input: the project. Output: the whole
    /// status, or none when a later page came from another check than the first.
    async fn roadmap_pages(
        &self,
        project: &ProjectId,
    ) -> Result<Option<RoadmapStatus>, EngineError> {
        let mut whole: Option<RoadmapStatus> = None;
        let mut after = 0;
        for _ in 0..=roadmap_status::ITEMS_MAX {
            let action = Action::ReadStatus {
                project: project.clone(),
                after,
            };
            let Reply::Status { status, next } = self.roadmap_exchange(action).await? else {
                return Err(unknown());
            };
            match &mut whole {
                None => whole = Some(status),
                Some(first) if first.checked_ms != status.checked_ms => return Ok(None),
                Some(first) => first.items.extend(status.items),
            }
            let held = whole.as_ref().map_or(0, |status| status.items.len());
            match next {
                None => return Ok(whole),
                Some(next) if next > after && next as usize == held => after = next,
                Some(_) => return Err(unknown()),
            }
            if held > roadmap_status::ITEMS_MAX {
                return Err(unknown());
            }
        }
        Err(unknown())
    }

    /// Asks for a project's roadmap status to be checked now, without the quiet period after
    /// a landing. Input: the project. Output: unit once the check is due, or
    /// `roadmap_status_off` under the Off setting.
    pub async fn roadmap_refresh(&self, project: ProjectId) -> Result<(), EngineError> {
        match self.roadmap_exchange(Action::Refresh { project }).await? {
            Reply::Done {} => Ok(()),
            _ => Err(unknown()),
        }
    }

    /// Saves when a project's roadmap status is checked. Inputs: the project and the choice.
    /// Output: unit once saved.
    pub async fn roadmap_set_refresh(
        &self,
        project: ProjectId,
        refresh: Refresh,
    ) -> Result<(), EngineError> {
        let action = Action::SetRefresh { project, refresh };
        match self.roadmap_exchange(action).await? {
            Reply::Done {} => Ok(()),
            _ => Err(unknown()),
        }
    }

    async fn roadmap_exchange(&self, action: Action) -> Result<Reply, EngineError> {
        tokio::time::timeout(self.timeout.max(ROADMAP_TIMEOUT), async {
            let read = read_kind(&action);
            let frame = encode_frame(&Envelope { roadmap: action })
                .map_err(|_| failure("roadmap_status_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("roadmap_status_unavailable"))?;
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
                .map_err(|_| failure("roadmap_status_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("roadmap_status_unavailable"))?;
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

pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    let reply = match handle(engine, authority, packet, admission, bytes).await {
        Ok(reply) => reply,
        Err(error) => Reply::Rejected { code: error.0 },
    };
    let frame = match encode_frame(&reply) {
        Ok(frame) => frame,
        Err(_) => encode_frame(&Reply::Rejected {
            code: "roadmap_status_reply_bounds".into(),
        })
        .map_err(|_| unknown())?,
    };
    stream.write_all(&frame).await.map_err(io_error)
}

async fn handle(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<Reply, EngineError> {
    let action = parse(bytes)?;
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    let root = super::changes::engine_root(authority)?;
    tokio::task::spawn_blocking(move || run(&root, action))
        .await
        .map_err(|_| failure("roadmap_status_unavailable"))?
}

/// Reads one Roadmap request from an owner frame body, at most REQUEST_BYTES_MAX bytes.
/// Input: the body. Output: the action, or `roadmap_status_request_bounds` or
/// `roadmap_status_request_invalid`.
fn parse(bytes: &[u8]) -> Result<Action, EngineError> {
    if bytes.len() > REQUEST_BYTES_MAX {
        return Err(failure("roadmap_status_request_bounds"));
    }
    let Envelope { roadmap } =
        serde_json::from_slice(bytes).map_err(|_| failure("roadmap_status_request_invalid"))?;
    Ok(roadmap)
}

/// Cuts a status to the items one reply frame carries, starting at `after`. Inputs: the
/// whole status and how many items the pages before held. Output: the status with that
/// page's items and where the next page starts, none on the last page; a start past the end,
/// as after a newer check with fewer items, gives an empty last page.
fn page(
    mut status: RoadmapStatus,
    after: u32,
) -> Result<(RoadmapStatus, Option<u32>), EngineError> {
    let total = status.items.len();
    let after = (after as usize).min(total);
    let (mut used, mut taken) = (0, 0);
    for item in &status.items[after..] {
        let size = serde_json::to_vec(item)
            .map_err(|_| failure("roadmap_status_unavailable"))?
            .len()
            + 1;
        if used + size > PAGE_BYTES_MAX {
            break;
        }
        used += size;
        taken += 1;
    }
    status.items = status.items.split_off(after);
    status.items.truncate(taken);
    let next = (after + taken < total).then_some((after + taken) as u32);
    assert!(taken > 0 || after == total);
    assert!(used <= PAGE_BYTES_MAX);
    Ok((status, next))
}

/// Runs one authenticated Roadmap action against the engine database in the engine root,
/// for a project the engine has. Inputs: the engine root and the action. Output: the reply,
/// `roadmap_status_project_missing`, or the store's refusal.
fn run(root: &Path, action: Action) -> Result<Reply, EngineError> {
    let db = crate::questions::open(root).map_err(|_| failure("roadmap_status_unavailable"))?;
    let (Action::ReadStatus { project, .. }
    | Action::Refresh { project }
    | Action::SetRefresh { project, .. }) = &action;
    let known: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(|_| failure("roadmap_status_unavailable"))?;
    if !known {
        return Err(failure("roadmap_status_project_missing"));
    }
    match action {
        Action::ReadStatus { project, after } => {
            let (status, next) = page(roadmap_status::read(&db, &project)?, after)?;
            Ok(Reply::Status { status, next })
        }
        Action::Refresh { project } => {
            roadmap_status::refresh(&db, &project)?;
            Ok(Reply::Done {})
        }
        Action::SetRefresh { project, refresh } => {
            roadmap_status::set_refresh(&db, &project, refresh)?;
            Ok(Reply::Done {})
        }
    }
}

#[cfg(all(test, unix))]
#[path = "roadmap_tests.rs"]
mod tests;
