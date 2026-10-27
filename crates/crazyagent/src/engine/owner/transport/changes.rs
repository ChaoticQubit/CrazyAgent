use super::*;
use crate::landing::{
    self, AcceptOutcome, ChangeDiff, ChangeSetId, ChangeSetView, LIST_MAX, MESSAGE_BYTES_MAX,
};
use crate::protocol::encode_frame;
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = 1024 + 4 * MESSAGE_BYTES_MAX;
pub(super) const CHANGES_TIMEOUT: Duration = Duration::from_secs(120);
const UPKEEP_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

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
    "changes_ignored_files",
    "changes_master_closed",
    "changes_master_not_started",
    "changes_master_tools_off",
    "changes_message_invalid",
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
    "changes_workspace_invalid",
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
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
            Self::Accept {
                message: Some(message),
                ..
            } if message.len() > 2 * MESSAGE_BYTES_MAX => Err(failure("changes_message_invalid")),
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
    /// Inputs: the change set, the head the owner reviewed and the owner's edited
    /// message, None for the proposed one. Output: the landed commit and branch, or the
    /// files it conflicts on.
    pub async fn accept_changes(
        &self,
        id: ChangeSetId,
        expected_head: String,
        message: Option<String>,
    ) -> Result<AcceptOutcome, EngineError> {
        let action = Action::Accept {
            id,
            expected_head,
            message,
        };
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
pub(super) fn engine_root(authority: &Authority) -> Result<PathBuf, EngineError> {
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
    let updated = match &action {
        Action::Update { id } => Some((id.clone(), root.clone())),
        _ => None,
    };
    let (reply, merged) = tokio::task::spawn_blocking(move || {
        let db = landing::open(&root)?;
        Ok::<_, EngineError>(match action {
            Action::List { project } => {
                let mut change_sets = landing::list(&db, &project)?;
                change_sets.truncate(LIST_MAX);
                (Reply::List { change_sets }, false)
            }
            Action::Diff { id, page } => (
                Reply::Diff {
                    diff: landing::diff(&db, &id, page)?,
                },
                false,
            ),
            Action::Accept {
                id,
                expected_head,
                message,
            } => {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .ok_or_else(|| failure("changes_owner_identity_missing"))?;
                let outcome = landing::accept(&db, &home, &id, &expected_head, message.as_deref())?;
                (Reply::Accepted { outcome }, false)
            }
            Action::Discard { id, expected_head } => (
                Reply::Discarded {
                    restorable_until_ms: landing::discard(&db, &id, &expected_head, now_ms()?)?,
                },
                false,
            ),
            Action::Restore { id } => {
                landing::restore(&db, &id, now_ms()?)?;
                (Reply::Done, false)
            }
            Action::Update { id } => (Reply::Done, crate::landing_turns::update(&db, &id)?),
        })
    })
    .await
    .map_err(|_| failure("changes_unavailable"))??;
    if let Some((id, root)) = updated {
        let runtime = super::OwnerRuntimeHandle {
            engine: engine.clone(),
            authority: authority.clone(),
        };
        match runtime
            .resolve_after_update(packet, admission, root, &id)
            .await
        {
            Err(problem) if merged => {
                eprintln!("resolve_not_started_after_update cause={}", problem.0);
            }
            started => started?,
        }
    }
    Ok(reply)
}

/// Keeps change sets tidy while the owner server runs: at start, and once a day after,
/// it finishes or undoes Accepts and worktree resets an engine stop interrupted and
/// deletes discarded refs past their restore window. Input: the authority, for the
/// engine root. Runs until the server aborts it.
pub(super) async fn upkeep(authority: Arc<Authority>) {
    loop {
        let tended = match engine_root(&authority) {
            Ok(root) => tokio::task::spawn_blocking(move || {
                let db = landing::open(&root)?;
                landing::reconcile(&db)?;
                landing::prune(&db, now_ms()?)
            })
            .await
            .unwrap_or_else(|_| Err(failure("changes_unavailable"))),
            Err(error) => Err(error),
        };
        if let Err(error) = tended {
            eprintln!("landing_upkeep_failed cause={}", error.0);
        }
        tokio::time::sleep(UPKEEP_EVERY).await;
    }
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
    #[cfg(unix)]
    use crate::engine::bootstrap::EngineService;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    /// Runs git as a test owner would, with an empty home and no system config.
    /// Inputs: the home, the folder and the arguments. Output: standard output.
    #[cfg(unix)]
    fn git(home: &Path, dir: &Path, args: &[&str]) -> String {
        let output = crate::spawn_gate::output(
            std::process::Command::new("/usr/bin/git")
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("LC_ALL", "C")
                .arg("-C")
                .arg(dir)
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped()),
        )
        .unwrap();
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8(output.stdout).unwrap()
    }

    /// Starts an engine on a root, as the owner server tests do.
    /// Inputs: the root and its stop signal. Output: the running service.
    #[cfg(unix)]
    async fn started(root: &Path, stop: &tokio::sync::watch::Receiver<bool>) -> EngineService {
        EngineService::start_with_settings_sources(
            root,
            "e.sock",
            &root.join("global.toml"),
            &root.join("project.toml"),
            stop,
        )
        .await
        .unwrap()
    }

    /// An engine that starts prunes the refs of change sets discarded more than seven
    /// days before, so an owner who discarded work and restarted later keeps no refs.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_engine_start_prunes_change_sets_discarded_more_than_seven_days_before() {
        let tmp = tempfile::Builder::new()
            .prefix("ca-chg-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let root = tmp.path();
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(root.join("global.toml"), "version = 1\ntimeout_ms = 5000\n").unwrap();
        std::fs::set_permissions(
            root.join("global.toml"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let mut service = started(root, &stopped).await;
        assert!(root.join("engine.db").is_file());
        let (repo, home, worktree) = (root.join("repo"), root.join("home"), root.join("agent-1"));
        for folder in [&repo, &home] {
            std::fs::create_dir(folder).unwrap();
        }
        git(&home, &repo, &["init", "-q", "--template=", "-b", "main"]);
        std::fs::write(repo.join("README"), "one\n").unwrap();
        git(&home, &repo, &["add", "README"]);
        git(
            &home,
            &repo,
            &[
                "-c",
                "user.name=O",
                "-c",
                "user.email=o@example.invalid",
                "commit",
                "-q",
                "-m",
                "Start",
            ],
        );
        git(
            &home,
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );
        std::fs::write(worktree.join("NEW"), "new\n").unwrap();
        let db = landing::open(root).unwrap();
        let workspace = landing::Workspace {
            project: crate::protocol::ProjectId::new("project-1").unwrap(),
            agent: landing::ChangeAgent {
                kind: landing::AgentKind::Sub,
                id: "agent-1".into(),
                label: "Agent 1".into(),
            },
            repository: repo.clone(),
            worktree,
            git_dir: repo.join(".git/worktrees/agent-1"),
        };
        let turn = landing::TurnEnd {
            outcome: landing::TurnOutcome::Succeeded,
            turn: "turn-1".into(),
            prompt_line: "Add a file".into(),
        };
        let id = landing::record_turn_end(&db, &workspace, &turn)
            .unwrap()
            .unwrap();
        let head = landing::list(&db, &workspace.project).unwrap()[0]
            .head
            .clone();
        let eight_days = 8 * 24 * 60 * 60 * 1000;
        landing::discard(&db, &id, &head, now_ms().unwrap() - eight_days).unwrap();
        drop(db);
        let discarded = || {
            git(
                &home,
                &repo,
                &["for-each-ref", "refs/crazyagent/discarded/"],
            )
        };
        assert!(discarded().contains(id.as_str()));
        tokio::time::timeout(Duration::from_secs(10), service.shutdown())
            .await
            .unwrap()
            .unwrap();
        let mut service = started(root, &stopped).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !discarded().is_empty() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the start kept the ref"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let db = landing::open(root).unwrap();
        assert_eq!(
            landing::restore(&db, &id, now_ms().unwrap()).unwrap_err().0,
            "changes_restore_expired"
        );
        tokio::time::timeout(Duration::from_secs(10), service.shutdown())
            .await
            .unwrap()
            .unwrap();
    }

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
        let engine: std::collections::BTreeSet<&str> = [
            include_str!("../../../landing.rs"),
            include_str!("../../../landing/diff.rs"),
            include_str!("../../../landing/flow.rs"),
            include_str!("../../../landing/git.rs"),
            include_str!("../../../landing/owner.rs"),
            include_str!("../../../landing/store.rs"),
        ]
        .into_iter()
        .flat_map(refusals)
        .collect();
        let protocol = refusals(include_str!("changes.rs"));
        assert!(engine.len() >= 15 && protocol.len() >= 5);
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
