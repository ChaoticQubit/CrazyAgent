use super::*;
use crate::protocol::{ProjectId, encode_frame};
use crate::team::cleanup::{self, FinishedAgent, RECLAIMABLE_MAX};
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = 2048;
pub(super) const CLEANUP_TIMEOUT: Duration = Duration::from_secs(120);

/// Every code a Cleanup request can answer an owner client with: the reasons a finished
/// agent is kept, this protocol's own, the change store's it reads through, and the owner
/// connection's it travels on.
pub const CLEANUP_CODES: &[&str] = &[
    "changes_git_failed",
    "changes_git_unsupported",
    "changes_unavailable",
    "cleanup_agent_has_task",
    "cleanup_agent_not_found",
    "cleanup_agent_running",
    "cleanup_changes_open",
    "cleanup_changes_restorable",
    "cleanup_commits_unlanded",
    "cleanup_folder_failed",
    "cleanup_outcome_unknown",
    "cleanup_reply_bounds",
    "cleanup_request_bounds",
    "cleanup_request_invalid",
    "cleanup_unavailable",
    "cleanup_worktree_dirty",
    "cleanup_worktree_invalid",
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
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Finished {
        project: ProjectId,
    },
    Remove {
        project: ProjectId,
        agents: Vec<i64>,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    cleanup: Action,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Finished { agents: Vec<FinishedAgent> },
    Removed { agents: Vec<i64> },
    Rejected { code: String },
}

fn unknown() -> EngineError {
    failure("cleanup_outcome_unknown")
}

impl OwnerClient {
    /// Reads a project's finished agents whose worktree and agent folder the owner may
    /// remove, newest first, with their size on disk. Input: the project. Output: at most
    /// `RECLAIMABLE_MAX` agents.
    pub async fn finished_agents(
        &self,
        project: ProjectId,
    ) -> Result<Vec<FinishedAgent>, EngineError> {
        match self.cleanup_exchange(Action::Finished { project }).await? {
            Reply::Finished { agents } if agents.len() <= RECLAIMABLE_MAX => Ok(agents),
            _ => Err(unknown()),
        }
    }

    /// Removes the worktrees and agent folders of finished agents the owner confirmed; the
    /// engine checks every one again first. Inputs: the project and the agents.
    /// Output: the agents removed, or the plain code of the first one kept.
    pub async fn remove_finished_agents(
        &self,
        project: ProjectId,
        agents: Vec<i64>,
    ) -> Result<Vec<i64>, EngineError> {
        match self
            .cleanup_exchange(Action::Remove { project, agents })
            .await?
        {
            Reply::Removed { agents } if agents.len() <= RECLAIMABLE_MAX => Ok(agents),
            _ => Err(unknown()),
        }
    }

    async fn cleanup_exchange(&self, action: Action) -> Result<Reply, EngineError> {
        tokio::time::timeout(self.timeout.max(CLEANUP_TIMEOUT), async {
            let frame = encode_frame(&Envelope { cleanup: action })
                .map_err(|_| failure("cleanup_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("cleanup_unavailable"))?;
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
                .map_err(|_| failure("cleanup_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("cleanup_unavailable"))?;
            let frame = read_frame(&mut stream).await.map_err(|_| unknown())?;
            match serde_json::from_slice(&frame[4..]).map_err(|_| unknown())? {
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
            code: "cleanup_reply_bounds".into(),
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
    if bytes.len() > REQUEST_BYTES_MAX {
        return Err(failure("cleanup_request_bounds"));
    }
    let Envelope { cleanup: action } =
        serde_json::from_slice(bytes).map_err(|_| failure("cleanup_request_invalid"))?;
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    let root = super::changes::engine_root(authority)?;
    tokio::task::spawn_blocking(move || {
        let db = crate::landing::open(&root).map_err(|_| failure("cleanup_unavailable"))?;
        crate::team::initialize(&db).map_err(|_| failure("cleanup_unavailable"))?;
        let team = root.join("team");
        let now = crate::team::now_ms();
        Ok(match action {
            Action::Finished { project } => Reply::Finished {
                agents: cleanup::finished(&db, &team, &project, now)?,
            },
            Action::Remove { project, agents } => {
                match cleanup::remove(&db, &team, &project, &agents, now)? {
                    Ok(agents) => Reply::Removed { agents },
                    Err(code) => Reply::Rejected { code: code.into() },
                }
            }
        })
    })
    .await
    .map_err(|_| failure("cleanup_unavailable"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The listed codes cover every plain code the cleanup and retire steps can answer
    /// with, so a client that passes listed codes through never hides a real one.
    #[test]
    fn cleanup_codes_cover_every_refusal_of_the_cleanup_steps() {
        let sources = [
            include_str!("../../../team/cleanup.rs"),
            include_str!("../../../landing/retire.rs"),
            include_str!("cleanup.rs"),
        ];
        let mut found = 0;
        for source in sources {
            let code = source.split("mod tests").next().unwrap();
            for (at, _) in code.match_indices("\"cleanup_") {
                let rest = &code[at + 1..];
                let name = &rest[..rest.find('"').unwrap()];
                assert!(
                    CLEANUP_CODES.contains(&name),
                    "{name} is not in CLEANUP_CODES"
                );
                found += 1;
            }
        }
        assert!(found >= 12);
        let mut sorted = CLEANUP_CODES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, CLEANUP_CODES);
    }
}
