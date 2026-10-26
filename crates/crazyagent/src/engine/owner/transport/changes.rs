use super::*;
use crate::landing::{self, AcceptOutcome, ChangeDiff, ChangeSetId, ChangeSetView, LIST_MAX};
use crate::protocol::encode_frame;
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = 1024;
pub(super) const CHANGES_TIMEOUT: Duration = Duration::from_secs(120);

/// Every code a Changes request can answer an owner client with: the landing engine's
/// refusals, this protocol's own, and the owner connection's it travels on.
pub const CHANGES_CODES: &[&str] = &[
    "changes_agent_busy",
    "changes_agent_has_pending",
    "changes_branch_moved",
    "changes_conflict",
    "changes_detached_head",
    "changes_git_busy",
    "changes_git_failed",
    "changes_git_unsupported",
    "changes_not_found",
    "changes_outcome_unknown",
    "changes_owner_dirty",
    "changes_owner_identity_missing",
    "changes_owner_unavailable",
    "changes_reply_bounds",
    "changes_request_bounds",
    "changes_request_invalid",
    "changes_restore_expired",
    "changes_stale",
    "changes_too_large",
    "changes_unavailable",
    "owner_authentication_failed",
    "owner_authority_unavailable",
    "owner_busy",
    "owner_capability_content_changed",
    "owner_capability_replaced_or_unsafe",
    "owner_entropy_unavailable",
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
    List {
        project: ProjectId,
    },
    Diff {
        id: ChangeSetId,
        page: u16,
    },
    Accept {
        id: ChangeSetId,
        expected_head: String,
    },
    Discard {
        id: ChangeSetId,
        expected_head: String,
    },
    Restore {
        id: ChangeSetId,
    },
    Update {
        id: ChangeSetId,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    changes: Action,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    List { change_sets: Vec<ChangeSetView> },
    Diff { diff: ChangeDiff },
    Accepted { outcome: AcceptOutcome },
    Discarded { restorable_until_ms: u64 },
    Done,
    Rejected { code: String },
}

fn unknown() -> EngineError {
    failure("changes_outcome_unknown")
}

fn valid_commit(commit: &str) -> bool {
    matches!(commit.len(), 40 | 64)
        && commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl Action {
    fn validate(&self) -> Result<(), EngineError> {
        match self {
            Self::Accept { expected_head, .. } | Self::Discard { expected_head, .. }
                if !valid_commit(expected_head) =>
            {
                Err(failure("changes_stale"))
            }
            _ => Ok(()),
        }
    }
}

impl OwnerClient {
    /// Reads one project's change sets, newest first. Input: the project. Output: each
    /// change set with its agent, state, line counts and restore window.
    pub async fn change_sets(&self, project: ProjectId) -> Result<Vec<ChangeSetView>, EngineError> {
        match self.changes_exchange(Action::List { project }).await? {
            Reply::List { change_sets } if change_sets.len() <= LIST_MAX => Ok(change_sets),
            _ => Err(unknown()),
        }
    }

    /// Reads one page of a change set's diff, as plain text. Inputs: the change set and
    /// the page, 0 first. Output: the page's files and text and the next page.
    pub async fn change_diff(&self, id: ChangeSetId, page: u16) -> Result<ChangeDiff, EngineError> {
        match self.changes_exchange(Action::Diff { id, page }).await? {
            Reply::Diff { diff } if diff.next.is_none_or(|next| next > page) => Ok(diff),
            _ => Err(unknown()),
        }
    }

    /// Lands a change set as one squash commit on the owner's checked-out branch.
    /// Inputs: the change set and the head the owner reviewed. Output: the landed commit
    /// and branch, or the files it conflicts on.
    pub async fn accept_changes(
        &self,
        id: ChangeSetId,
        expected_head: String,
    ) -> Result<AcceptOutcome, EngineError> {
        let action = Action::Accept { id, expected_head };
        match self.changes_exchange(action).await? {
            Reply::Accepted { outcome } => Ok(outcome),
            _ => Err(unknown()),
        }
    }

    /// Discards a change set; it stays restorable for seven days. Inputs: the change set
    /// and the head the owner reviewed. Output: the time it stays restorable until.
    pub async fn discard_changes(
        &self,
        id: ChangeSetId,
        expected_head: String,
    ) -> Result<u64, EngineError> {
        let action = Action::Discard { id, expected_head };
        match self.changes_exchange(action).await? {
            Reply::Discarded {
                restorable_until_ms,
            } => Ok(restorable_until_ms),
            _ => Err(unknown()),
        }
    }

    /// Brings a discarded change set back as pending. Input: the change set.
    pub async fn restore_changes(&self, id: ChangeSetId) -> Result<(), EngineError> {
        match self.changes_exchange(Action::Restore { id }).await? {
            Reply::Done => Ok(()),
            _ => Err(unknown()),
        }
    }

    /// Asks the agent to update a conflicting change set: the engine merges the owner's
    /// branch into the agent's worktree with conflict markers. Input: the change set.
    pub async fn update_changes(&self, id: ChangeSetId) -> Result<(), EngineError> {
        match self.changes_exchange(Action::Update { id }).await? {
            Reply::Done => Ok(()),
            _ => Err(unknown()),
        }
    }

    async fn changes_exchange(&self, action: Action) -> Result<Reply, EngineError> {
        action.validate()?;
        tokio::time::timeout(self.timeout.max(CHANGES_TIMEOUT), async {
            let frame = encode_frame(&Envelope { changes: action })
                .map_err(|_| failure("changes_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("changes_owner_unavailable"))?;
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
                .map_err(|_| failure("changes_owner_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("changes_owner_unavailable"))?;
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

/// The engine root that holds `engine.db`: the parent of the owner runtime's per-start
/// folder. Input: the authority. Output: the root, or `changes_unavailable`.
fn engine_root(authority: &Authority) -> Result<PathBuf, EngineError> {
    authority.root.verify()?;
    authority
        .root
        .path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| failure("changes_unavailable"))
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
            code: "changes_reply_bounds".into(),
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
        return Err(failure("changes_request_bounds"));
    }
    let Envelope { changes: action } =
        serde_json::from_slice(bytes).map_err(|_| failure("changes_request_invalid"))?;
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    action.validate()?;
    let root = engine_root(authority)?;
    tokio::task::spawn_blocking(move || {
        let db = landing::open(&root)?;
        Ok(match action {
            Action::List { project } => {
                let mut change_sets = landing::list(&db, &project)?;
                change_sets.truncate(LIST_MAX);
                Reply::List { change_sets }
            }
            Action::Diff { id, page } => Reply::Diff {
                diff: landing::diff(&db, &id, page)?,
            },
            Action::Accept { id, expected_head } => {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .ok_or_else(|| failure("changes_owner_identity_missing"))?;
                Reply::Accepted {
                    outcome: landing::accept(&db, &home, &id, &expected_head)?,
                }
            }
            Action::Discard { id, expected_head } => Reply::Discarded {
                restorable_until_ms: landing::discard(&db, &id, &expected_head, now_ms()?)?,
            },
            Action::Restore { id } => {
                landing::restore(&db, &id, now_ms()?)?;
                Reply::Done
            }
            Action::Update { id } => {
                landing::update(&db, &id)?;
                Reply::Done
            }
        })
    })
    .await
    .map_err(|_| failure("changes_unavailable"))?
}

fn now_ms() -> Result<u64, EngineError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|since| u64::try_from(since.as_millis()).ok())
        .ok_or_else(|| failure("changes_unavailable"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads the plain codes a source file refuses with, before its tests.
    /// Input: the file text. Output: each `changes_` code once.
    fn refusals(source: &str) -> std::collections::BTreeSet<&str> {
        let code = source.split("mod tests").next().unwrap();
        code.match_indices("\"changes_")
            .filter_map(|(at, _)| {
                let rest = &code[at + 1..];
                rest.find('"').map(|end| &rest[..end])
            })
            .collect()
    }

    /// The listed codes cover every refusal the landing engine and this protocol make,
    /// so a client that passes listed codes through never hides a real one.
    #[test]
    fn changes_codes_cover_every_refusal_of_the_landing_engine_and_protocol() {
        let engine = refusals(include_str!("../../../landing.rs"));
        let protocol = refusals(include_str!("changes.rs"));
        assert!(engine.len() >= 2 && protocol.len() >= 5);
        for code in engine.iter().chain(&protocol) {
            assert!(
                CHANGES_CODES.contains(code),
                "{code} is not in CHANGES_CODES"
            );
        }
        let mut sorted = CHANGES_CODES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, CHANGES_CODES);
        assert!(CHANGES_CODES.iter().all(|code| {
            code.bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        }));
    }
}
