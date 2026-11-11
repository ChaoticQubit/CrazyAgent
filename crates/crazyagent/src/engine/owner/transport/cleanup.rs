use super::*;
use crate::protocol::{ProjectId, encode_frame};
use crate::team::cleanup::{
    self, CANDIDATES_MAX, FinishedAgent, RECLAIMABLE_MAX, Removal, RemovalPreview,
};
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = 2048;
pub(super) const CLEANUP_TIMEOUT: Duration = Duration::from_secs(120);
const SWEEP_EVERY: Duration = Duration::from_secs(30 * 60);

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
    "cleanup_refs_unlanded",
    "cleanup_reply_bounds",
    "cleanup_request_bounds",
    "cleanup_request_invalid",
    "cleanup_unavailable",
    "cleanup_worktree_dirty",
    "cleanup_worktree_invalid",
    "cleanup_worktree_leftover",
    "cleanup_worktree_unreadable",
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
    Removal {
        project: ProjectId,
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
    Finished {
        agents: Vec<FinishedAgent>,
    },
    Removed {
        agents: Vec<i64>,
        kept: Option<String>,
    },
    Removal {
        preview: RemovalPreview,
    },
    Rejected {
        code: String,
    },
}

/// Names the requests that only read, for the log of replies this client could not read.
/// Input: the request. Output: its name, None for a request that changes something.
fn read_kind(action: &Action) -> Option<&'static str> {
    match action {
        Action::Finished { .. } => Some("cleanup.finished"),
        Action::Removal { .. } => Some("cleanup.removal"),
        _ => None,
    }
}

fn unknown() -> EngineError {
    failure("cleanup_outcome_unknown")
}

impl OwnerClient {
    /// Reads a project's finished agents that still hold a worktree, newest first: the ones
    /// the owner may remove with their worktree's size on disk, and the ones a check keeps
    /// with its code. Input: the project. Output: at most `CANDIDATES_MAX` agents.
    pub async fn finished_agents(
        &self,
        project: ProjectId,
    ) -> Result<Vec<FinishedAgent>, EngineError> {
        match self.cleanup_exchange(Action::Finished { project }).await? {
            Reply::Finished { agents } if agents.len() <= CANDIDATES_MAX => Ok(agents),
            _ => Err(unknown()),
        }
    }

    /// Removes the worktrees of finished agents the owner confirmed; the engine checks every
    /// one again first, and their folders, conversations and history stay. Inputs: the
    /// project and the agents.
    /// Output: the agents removed with the code of the first one kept when it stopped part
    /// way, or the refusal when it removed none.
    pub async fn remove_finished_agents(
        &self,
        project: ProjectId,
        agents: Vec<i64>,
    ) -> Result<Removal, EngineError> {
        match self
            .cleanup_exchange(Action::Remove { project, agents })
            .await?
        {
            Reply::Removed { agents, kept }
                if !agents.is_empty() && agents.len() <= RECLAIMABLE_MAX =>
            {
                Ok(Removal { agents, kept })
            }
            _ => Err(unknown()),
        }
    }

    /// Reads what removing a project would do: its name, the folders Delete everything
    /// would move and whether it may, its running agents, open change sets and the work
    /// each agent would lose. Input: the project. Output: the preview.
    pub async fn removal_preview(&self, project: ProjectId) -> Result<RemovalPreview, EngineError> {
        match self.cleanup_exchange(Action::Removal { project }).await? {
            Reply::Removal { preview } => Ok(preview),
            _ => Err(unknown()),
        }
    }

    async fn cleanup_exchange(&self, action: Action) -> Result<Reply, EngineError> {
        tokio::time::timeout(self.timeout.max(CLEANUP_TIMEOUT), async {
            let read = read_kind(&action);
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

/// A cleanup request read and checked once: a list of finished agents already read, or a
/// removal still to admit, with the engine folder it changes.
enum Prepared {
    Finished(Vec<FinishedAgent>),
    Removal(Box<RemovalPreview>),
    Remove(PathBuf, ProjectId, Vec<i64>),
}

/// Serves one cleanup request. A list is read first, then under the owner barrier the
/// grant is checked again and the reply is written. A removal is admitted under the barrier
/// and made without it, and its reply is what it removed.
/// Inputs: the connection, engine, authority, packet, admission and request body.
/// Output: done once the reply is written.
pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    let (answered, _barrier) = match prepare(engine, authority, packet, admission, bytes).await {
        Ok(Prepared::Remove(root, project, agents)) => (
            remove(engine, authority, packet, admission, root, project, agents).await,
            None,
        ),
        Ok(Prepared::Finished(agents)) => {
            let barrier = engine.owner_egress.write().await;
            (
                checked(authority, packet, admission).map(|()| Reply::Finished { agents }),
                Some(barrier),
            )
        }
        Ok(Prepared::Removal(preview)) => {
            let barrier = engine.owner_egress.write().await;
            (
                checked(authority, packet, admission)
                    .map(|()| Reply::Removal { preview: *preview }),
                Some(barrier),
            )
        }
        Err(error) => (Err(error), None),
    };
    let reply = match answered {
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
    write_reply(stream, &frame).await
}

/// Removes finished agents' worktrees on the engine's own schedule while the owner server
/// runs, as the owner's setting says: a pass at start, which also takes agents that finished
/// before this build once their wait is over, then one every `SWEEP_EVERY`, each on a
/// blocking thread off the request path, reading the setting again each time, and each
/// starting where the last one stopped. Inputs: the authority, held weakly so a server
/// dropped without its stop path still releases it, and the server's stop. Runs until the
/// stop or until the authority is gone; a pass already running ends first.
pub(super) async fn sweep(authority: std::sync::Weak<Authority>, mut stop: oneshot::Receiver<()>) {
    let mut after = 0;
    loop {
        let Some(root) = authority
            .upgrade()
            .map(|authority| super::changes::engine_root(&authority))
        else {
            return;
        };
        let passed = match root {
            Ok(root) => tokio::task::spawn_blocking(move || {
                let db = open(&root)?;
                crate::cleanup_settings::initialize(&db)?;
                let settings = crate::cleanup_settings::load(&db)?;
                cleanup::sweep(&db, &root, &settings, after, crate::team::now_ms())
            })
            .await
            .unwrap_or_else(|_| Err(failure("cleanup_unavailable"))),
            Err(error) => Err(error),
        };
        match passed {
            Ok(swept) => {
                after = swept.after;
                for line in swept.failures {
                    eprintln!("finished_agents_sweep_skipped {line}");
                }
                for line in swept.kept {
                    eprintln!("finished_agents_sweep_kept {line}");
                }
            }
            Err(error) => {
                after = 0;
                eprintln!(
                    "finished_agents_sweep_failed cause={}",
                    crate::protocol::plain_code(&error.0)
                );
            }
        }
        tokio::select! {
            _ = &mut stop => return,
            () = tokio::time::sleep(SWEEP_EVERY) => {}
        }
    }
}

/// Opens the change store of an engine folder with the team tables in place.
/// Input: the engine folder. Output: the connection, or `cleanup_unavailable`.
fn open(root: &Path) -> Result<rusqlite::Connection, EngineError> {
    let db = crate::landing::open(root).map_err(|_| failure("cleanup_unavailable"))?;
    crate::team::initialize(&db).map_err(|_| failure("cleanup_unavailable"))?;
    Ok(db)
}

/// Reads and checks a cleanup request, and reads a list of finished agents outside the
/// owner barrier. Inputs: the engine, authority, packet, admission and request body.
/// Output: the prepared request, or the refusal.
async fn prepare(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<Prepared, EngineError> {
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
    match action {
        Action::Finished { project } => tokio::task::spawn_blocking(move || {
            let db = open(&root)?;
            let now = crate::team::now_ms();
            cleanup::finished(&db, &project, now).map(Prepared::Finished)
        })
        .await
        .map_err(|_| failure("cleanup_unavailable"))?,
        Action::Removal { project } => tokio::task::spawn_blocking(move || {
            let mut db = open(&root)?;
            let now = crate::team::now_ms();
            cleanup::removal_preview(&mut db, &project, now)
                .map(|preview| Prepared::Removal(Box::new(preview)))
        })
        .await
        .map_err(|_| failure("cleanup_unavailable"))?,
        Action::Remove { project, agents } => Ok(Prepared::Remove(root, project, agents)),
    }
}

/// Checks the grant again, under the owner barrier its caller holds, before a list's reply
/// is written. Inputs: the authority, packet and admission. Output: Ok, or the refusal.
fn checked(
    authority: &Authority,
    packet: &[u8; 37],
    admission: &Admission,
) -> Result<(), EngineError> {
    if &authority.authenticate(packet)? != admission {
        return Err(failure("owner_grant_stale_or_revoked"));
    }
    Ok(())
}

/// Removes finished agents: the grant is checked and the work handed over under the owner
/// barrier, which is let go while whole worktrees are deleted, so a long removal stalls no
/// owner request or revocation. A removal admitted before a revocation finishes and answers
/// what it removed, unchecked. Inputs: the engine, authority, packet, admission, engine
/// folder, project and agents. Output: the reply, or the refusal.
async fn remove(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    root: PathBuf,
    project: ProjectId,
    agents: Vec<i64>,
) -> Result<Reply, EngineError> {
    let work = {
        let _barrier = engine.owner_egress.write().await;
        checked(authority, packet, admission)?;
        tokio::task::spawn_blocking(move || {
            let db = open(&root)?;
            let now = crate::team::now_ms();
            Ok(match cleanup::remove(&db, &root, &project, &agents, now)? {
                Removal {
                    agents,
                    kept: Some(code),
                } if agents.is_empty() => Reply::Rejected { code },
                Removal { agents, kept } => Reply::Removed { agents, kept },
            })
        })
    };
    work.await.map_err(|_| failure("cleanup_unavailable"))?
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
