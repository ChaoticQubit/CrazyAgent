use super::*;
use crate::landing::{
    self, AcceptOutcome, BRANCH_BYTES_MAX, BRANCHES_MAX, ChangeDiff, ChangeSetId, ChangeSetView,
    LIST_MAX, MESSAGE_BYTES_MAX, TargetBranches, WaitingChanges,
};
use crate::protocol::encode_frame;
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = 1024 + 4 * MESSAGE_BYTES_MAX;
pub(crate) const CHANGES_TIMEOUT: Duration = Duration::from_secs(120);
const UPKEEP_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Every code a Changes request can answer an owner client with: the landing engine's
/// refusals, this protocol's own, the owner connection's it travels on, and the team's
/// refusals of the resolve work an Update hands a team agent.
pub const CHANGES_CODES: &[&str] = &[
    "changes_agent_branch_exists",
    "changes_agent_branch_invalid",
    "changes_agent_busy",
    "changes_agent_has_pending",
    "changes_bare_repository",
    "changes_branch_moved",
    "changes_branch_not_above_base",
    "changes_conflict",
    "changes_detached_head",
    "changes_embedded_repository",
    "changes_git_busy",
    "changes_git_failed",
    "changes_git_unsupported",
    "changes_ignored_files",
    "changes_link_outside",
    "changes_master_closed",
    "changes_master_not_started",
    "changes_master_tools_off",
    "changes_merge_unsafe",
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
    "changes_target_checked_out",
    "changes_target_invalid",
    "changes_too_large",
    "changes_unavailable",
    "changes_undo_unavailable",
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
    "team_agent_not_found",
    "team_conversation_full",
    "team_task_invalid",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    List {
        project: ProjectId,
    },
    Diff {
        id: ChangeSetId,
        head: String,
        page: u16,
    },
    Accept {
        id: ChangeSetId,
        expected_head: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
    },
    Branches {
        id: ChangeSetId,
    },
    Discard {
        id: ChangeSetId,
        expected_head: String,
    },
    Restore {
        id: ChangeSetId,
    },
    Undo {
        id: ChangeSetId,
    },
    Update {
        id: ChangeSetId,
    },
    Waiting {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<ProjectId>,
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
    Branches { branches: TargetBranches },
    Discarded { restorable_until_ms: u64 },
    Waiting { waiting: WaitingChanges },
    Done,
    Rejected { code: String },
}

/// Names the requests that only read, for the log of replies this client could not read.
/// Input: the request. Output: its name, None for a request that changes something.
fn read_kind(action: &Action) -> Option<&'static str> {
    match action {
        Action::List { .. } => Some("changes.list"),
        Action::Diff { .. } => Some("changes.diff"),
        Action::Branches { .. } => Some("changes.branches"),
        Action::Waiting { .. } => Some("changes.waiting"),
        _ => None,
    }
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
            Self::Accept { expected_head, .. }
            | Self::Discard { expected_head, .. }
            | Self::Diff {
                head: expected_head,
                ..
            } if !valid_commit(expected_head) => Err(failure("changes_stale")),
            Self::Accept {
                message: Some(message),
                ..
            } if message.len() > 2 * MESSAGE_BYTES_MAX => Err(failure("changes_message_invalid")),
            Self::Accept {
                target: Some(target),
                ..
            } if target.is_empty() || target.len() > BRANCH_BYTES_MAX => {
                Err(failure("changes_target_invalid"))
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

    /// Counts the change sets waiting for the owner, ready to review or conflicting, across
    /// the projects the engine has, and the given project's among them. Input: the project,
    /// if any. Output: both counts; a reply whose project count is missing, unasked or above
    /// the total is refused.
    pub async fn waiting_changes(
        &self,
        project: Option<ProjectId>,
    ) -> Result<WaitingChanges, EngineError> {
        let asked = project.is_some();
        match self.changes_exchange(Action::Waiting { project }).await? {
            Reply::Waiting { waiting }
                if waiting.here.is_some() == asked
                    && waiting.here.is_none_or(|here| here <= waiting.all) =>
            {
                Ok(waiting)
            }
            _ => Err(unknown()),
        }
    }

    /// Reads one page of a change set's diff, as plain text, at the head the owner is
    /// reading. Inputs: the change set, that head and the page, 0 first. Output: the
    /// page's files and text and the next page, or `changes_stale` once the set moved.
    pub async fn change_diff(
        &self,
        id: ChangeSetId,
        head: String,
        page: u16,
    ) -> Result<ChangeDiff, EngineError> {
        match self
            .changes_exchange(Action::Diff { id, head, page })
            .await?
        {
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
        self.accept_changes_into(id, expected_head, message, None)
            .await
    }

    /// Lands a change set as one squash commit on a chosen local branch, the checked-out
    /// one when none is named; a branch not checked out moves as a ref and the owner's
    /// files stay as they are. Inputs: the change set, the reviewed head, the edited
    /// message and the target branch. Output: the landed commit and branch, or the files
    /// it conflicts on.
    pub async fn accept_changes_into(
        &self,
        id: ChangeSetId,
        expected_head: String,
        message: Option<String>,
        target: Option<String>,
    ) -> Result<AcceptOutcome, EngineError> {
        let action = Action::Accept {
            id,
            expected_head,
            message,
            target,
        };
        match self.changes_exchange(action).await? {
            Reply::Accepted { outcome } => Ok(outcome),
            _ => Err(unknown()),
        }
    }

    /// Undoes a change set its own Accept merged, with one revert commit on the branch it
    /// landed on. Input: the change set. Output: the undo commit and branch, or the files
    /// the branch changed since, when nothing changed.
    pub async fn undo_changes(&self, id: ChangeSetId) -> Result<AcceptOutcome, EngineError> {
        match self.changes_exchange(Action::Undo { id }).await? {
            Reply::Accepted { outcome } => Ok(outcome),
            _ => Err(unknown()),
        }
    }

    /// Reads the local branches a change set can be accepted onto and the checked-out
    /// one, the default target. Input: the change set. Output: the branches.
    pub async fn change_branches(&self, id: ChangeSetId) -> Result<TargetBranches, EngineError> {
        match self.changes_exchange(Action::Branches { id }).await? {
            Reply::Branches { branches } if branches.branches.len() <= BRANCHES_MAX => Ok(branches),
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
            let read = read_kind(&action);
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

/// A changes request read and checked once: the reply of a read already made, or a
/// change still to admit, with the engine folder whose store it changes.
enum Prepared {
    Read(Box<Reply>),
    Change(PathBuf, Action),
}

/// Serves one changes request. A list or diff is read first, then under the owner barrier
/// the grant is checked again and the reply is written. A change is admitted under the
/// barrier and made without it, and its reply is what it did.
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
        Ok(Prepared::Change(root, action)) => (
            change(engine, authority, packet, admission, root, action).await,
            None,
        ),
        Ok(Prepared::Read(reply)) => {
            let barrier = engine.owner_egress.write().await;
            (
                checked(authority, packet, admission).map(|()| *reply),
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
            code: "changes_reply_bounds".into(),
        })
        .map_err(|_| unknown())?,
    };
    write_reply(stream, &frame).await
}

/// Reads and checks a changes request, and makes a list or diff outside the owner barrier.
/// Inputs: the engine, authority, packet, admission and request body.
/// Output: the prepared request, or the refusal.
async fn prepare(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<Prepared, EngineError> {
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
    if !matches!(
        action,
        Action::List { .. }
            | Action::Diff { .. }
            | Action::Waiting { .. }
            | Action::Branches { .. }
    ) {
        return Ok(Prepared::Change(root, action));
    }
    tokio::task::spawn_blocking(move || {
        run(&root, action).map(|(reply, _)| Prepared::Read(Box::new(reply)))
    })
    .await
    .map_err(|_| failure("changes_unavailable"))?
}

/// Checks the grant again, under the owner barrier its caller holds, before a read's reply
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

/// Makes one change: the grant is checked and the work handed over under the owner barrier,
/// which is let go while it runs, so a long Accept stalls no owner request or revocation. A
/// change admitted before a revocation finishes and answers what it did, unchecked; an
/// Update then hands a master's resolve over. Inputs: the engine, authority, packet,
/// admission, engine folder and change. Output: the reply, or the refusal.
async fn change(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    root: PathBuf,
    action: Action,
) -> Result<Reply, EngineError> {
    let updated = match &action {
        Action::Update { id } => Some(id.clone()),
        _ => None,
    };
    let work = {
        let _barrier = engine.owner_egress.write().await;
        checked(authority, packet, admission)?;
        let store = root.clone();
        tokio::task::spawn_blocking(move || run(&store, action))
    };
    let (reply, merged) = work.await.map_err(|_| failure("changes_unavailable"))??;
    let Some(id) = updated else {
        return Ok(reply);
    };
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
    Ok(reply)
}

/// Runs one changes request on the change store of an engine folder.
/// Inputs: the folder and the request. Output: the reply, and whether an Update merged.
fn run(root: &Path, action: Action) -> Result<(Reply, bool), EngineError> {
    let db = landing::open(root)?;
    Ok(match action {
        Action::List { project } => {
            if let Err(error) = landing::observe(&db, Some(&project)) {
                eprintln!("landing_observe_failed cause={}", error.0);
            }
            let mut change_sets = landing::owner_list(&db, &project)?;
            change_sets.truncate(LIST_MAX);
            (Reply::List { change_sets }, false)
        }
        Action::Diff { id, head, page } => (
            Reply::Diff {
                diff: landing::diff(&db, &id, &head, page)?,
            },
            false,
        ),
        Action::Waiting { project } => (
            Reply::Waiting {
                waiting: landing::waiting(&db, project.as_ref())?,
            },
            false,
        ),
        Action::Accept {
            id,
            expected_head,
            message,
            target,
        } => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| failure("changes_owner_identity_missing"))?;
            let outcome = match target.as_deref() {
                None => landing::accept(
                    &db,
                    &home,
                    &id,
                    &expected_head,
                    message.as_deref(),
                    crate::activity::AcceptPlace::Changes,
                )?,
                Some(target) => landing::accept_into(
                    &db,
                    &home,
                    &id,
                    &expected_head,
                    message.as_deref(),
                    target,
                    crate::activity::AcceptPlace::Changes,
                )?,
            };
            (Reply::Accepted { outcome }, false)
        }
        Action::Branches { id } => (
            Reply::Branches {
                branches: landing::target_branches(&db, &id)?,
            },
            false,
        ),
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
        Action::Undo { id } => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| failure("changes_owner_identity_missing"))?;
            (
                Reply::Accepted {
                    outcome: landing::undo_landing(&db, &home, &id)?,
                },
                false,
            )
        }
        Action::Update { id } => (Reply::Done, crate::landing_turns::update(&db, &id)?),
    })
}

/// Keeps change sets tidy while the owner server runs: at start, and once a day after,
/// it finishes or undoes Accepts and worktree resets an engine stop interrupted, numbers
/// the change sets an engine without numbers stored, closes open change sets the owner
/// landed or removed outside the app and deletes discarded refs past their restore
/// window, each step run on its own so a failing one never skips the
/// others. Inputs: the authority, held weakly so a server dropped without its stop path
/// still releases it, and the server's stop, a sender dropped or sent. Runs until the stop
/// or until the authority is gone; a pass already running ends first, since its blocking
/// work cannot be cancelled and holds its own connection to the engine database.
pub(super) async fn upkeep(authority: std::sync::Weak<Authority>, mut stop: oneshot::Receiver<()>) {
    loop {
        let Some(root) = authority.upgrade().map(|authority| engine_root(&authority)) else {
            return;
        };
        let failures = match root {
            Ok(root) => tokio::task::spawn_blocking(move || match landing::open(&root) {
                Ok(db) => [
                    landing::reconcile(&db).err(),
                    landing::number_change_sets(&db).err(),
                    landing::observe(&db, None).err(),
                    now_ms().and_then(|now| landing::prune(&db, now)).err(),
                ]
                .into_iter()
                .flatten()
                .collect(),
                Err(error) => vec![error],
            })
            .await
            .unwrap_or_else(|_| vec![failure("changes_unavailable")]),
            Err(error) => vec![error],
        };
        for error in failures {
            eprintln!("landing_upkeep_failed cause={}", error.0);
        }
        tokio::select! {
            _ = &mut stop => return,
            () = tokio::time::sleep(UPKEEP_EVERY) => {}
        }
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

    /// An engine stop waits for the landing upkeep its start began, so no connection of the
    /// stopped engine is left on its database when the next engine opens it, even when the
    /// stop comes while the upkeep still prunes thousands of discarded change sets.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_engine_stop_waits_for_its_landing_upkeep_to_leave_the_database() {
        const DISCARDED: i64 = 5000;
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
        let gone = root.join("gone");
        let db = landing::open(root).unwrap();
        db.execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1) \
             INSERT INTO landing_change_sets(id, project, agent_kind, agent_id, label, \
             repository, worktree, git_dir, base, head, state, last_outcome, turn, prompt_line, \
             files, added, removed, conflicts, created_ms, updated_ms, restorable_until_ms) \
             SELECT printf('%032x', i), 'project-1', 'sub', 'agent-' || i, 'Agent', ?2, \
             ?2 || '/agent-' || i, ?2 || '/.git/worktrees/agent-' || i, ?3, ?3, 'discarded', \
             'succeeded', 'turn-1', 'Add a file', 1, 1, 0, '[]', 1, 1, 1 FROM n",
            rusqlite::params![DISCARDED, gone.to_str().unwrap(), "a".repeat(40)],
        )
        .unwrap();
        drop(db);
        tokio::time::timeout(Duration::from_secs(10), service.shutdown())
            .await
            .unwrap()
            .unwrap();
        let mut service = started(root, &stopped).await;
        tokio::time::timeout(Duration::from_secs(60), service.shutdown())
            .await
            .unwrap()
            .unwrap();
        let probe = rusqlite::Connection::open(root.join("engine.db")).unwrap();
        probe.busy_timeout(Duration::ZERO).unwrap();
        let alone = probe.execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE; COMMIT;");
        assert!(
            alone.is_ok(),
            "a connection of the stopped engine was left on its database: {alone:?}"
        );
        let pruned: i64 = probe
            .query_row(
                "SELECT count(*) FROM landing_change_sets WHERE pruned=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            pruned, DISCARDED,
            "the pass the stop waited for ran to its end"
        );
    }

    /// The owner socket lists the local branches a change set can be accepted onto with the
    /// checked-out one as the default, and refuses an unknown change set.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn branches_lists_local_branches_and_the_current_one() {
        let tmp = tempfile::Builder::new()
            .prefix("ca-brn-")
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
        git(&home, &repo, &["branch", "release"]);
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
        drop(db);
        let (_, controller) = crate::client_controller::OwnerController::attach_configured(
            crate::engine::Client::new(root.join("e.sock"), Duration::from_secs(10)).unwrap(),
            crate::engine::owner::OwnerClientKind::Tui,
        )
        .await
        .unwrap();
        assert_eq!(
            controller.change_branches(id).await.unwrap(),
            TargetBranches {
                branches: vec!["main".to_owned(), "release".to_owned()],
                current: Some("main".to_owned()),
                land_on: Some("main".to_owned()),
                land_on_problem: None,
            }
        );
        let unknown = ChangeSetId::new("0".repeat(32)).unwrap();
        assert_eq!(
            controller.change_branches(unknown).await.unwrap_err().0,
            "changes_not_found"
        );
        drop(controller);
        tokio::time::timeout(Duration::from_secs(10), service.shutdown())
            .await
            .unwrap()
            .unwrap();
    }

    /// An owner List after the owner merged an open change set by hand replies with that
    /// set closed as landed outside the app, so every client shows it without an action.
    #[cfg(unix)]
    #[test]
    fn a_list_after_a_hand_merge_replies_with_the_set_landed_outside() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (repo, home, worktree) = (root.join("repo"), root.join("home"), root.join("agent-1"));
        for folder in [&repo, &home] {
            std::fs::create_dir(folder).unwrap();
        }
        let owner = |args: &[&str]| {
            let mut full = vec!["-c", "user.name=O", "-c", "user.email=o@example.invalid"];
            full.extend_from_slice(args);
            git(&home, &repo, &full)
        };
        owner(&["init", "-q", "--template=", "-b", "main"]);
        std::fs::write(repo.join("README"), "one\n").unwrap();
        owner(&["add", "README"]);
        owner(&["commit", "-q", "-m", "Start"]);
        owner(&[
            "worktree",
            "add",
            "-q",
            "--detach",
            worktree.to_str().unwrap(),
            "HEAD",
        ]);
        std::fs::write(worktree.join("NEW"), "new\n").unwrap();
        let project = crate::protocol::ProjectId::new("project-1").unwrap();
        let db = landing::open(&root).unwrap();
        let workspace = landing::Workspace {
            project: project.clone(),
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
        let head = landing::list(&db, &project).unwrap()[0].head.clone();
        drop(db);
        let listed = |root: &Path| match run(
            root,
            Action::List {
                project: project.clone(),
            },
        )
        .unwrap()
        {
            (Reply::List { change_sets }, false) => change_sets,
            _ => panic!("expected a list reply"),
        };
        let open = listed(&root);
        assert_eq!(
            (open.len(), open[0].state, open[0].outside),
            (1, landing::ChangeState::Pending, false)
        );
        owner(&["merge", "--no-ff", "-q", "-m", "Merge by hand", &head]);
        let closed = listed(&root);
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].id, id);
        assert_eq!(
            (closed[0].state, closed[0].outside),
            (landing::ChangeState::Accepted, true)
        );
    }
    /// Makes an owner repository under an engine root with one agent worktree whose turn
    /// added a file, recorded as a change set. Input: the root. Output: the workspace and
    /// the change set.
    #[cfg(unix)]
    fn recorded(root: &Path) -> (landing::Workspace, ChangeSetId) {
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
        std::fs::write(worktree.join("NEW"), "first\n").unwrap();
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
        let db = landing::open(root).unwrap();
        let id = landing::record_turn_end(&db, &workspace, &turn)
            .unwrap()
            .unwrap();
        (workspace, id)
    }

    /// A diff page is read at the head the owner is reading, named in the request as the
    /// desktop and the TUI send it, and refused as `changes_stale` once the set moved on, so
    /// no page of another head is shown under the head Accept names (RV-DS-13, RV-DS-79).
    #[cfg(unix)]
    #[test]
    fn a_diff_page_is_read_only_at_the_head_the_owner_is_reading() {
        let tmp = tempfile::Builder::new()
            .prefix("ca-chg-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let root = tmp.path();
        let (workspace, id) = recorded(root);
        let db = landing::open(root).unwrap();
        let head_of = || landing::change_set(&db, &id).unwrap().unwrap().1.head;
        let first = head_of();
        let asked = |head: &str| -> Result<Reply, EngineError> {
            let request = format!(
                r#"{{"changes":{{"kind":"diff","id":"{}","head":"{head}","page":0}}}}"#,
                id.as_str()
            );
            let Envelope { changes } =
                serde_json::from_str(&request).map_err(|_| failure("changes_request_invalid"))?;
            changes.validate()?;
            run(root, changes).map(|(reply, _)| reply)
        };
        let shown = |reply: Result<Reply, EngineError>| match reply {
            Ok(Reply::Diff { diff }) => diff.text,
            Ok(_) => "another reply".into(),
            Err(problem) => problem.0,
        };
        assert!(
            shown(asked(&first)).contains("+first"),
            "page at the head read"
        );

        std::fs::write(workspace.worktree.join("NEW"), "second\n").unwrap();
        let turn = landing::TurnEnd {
            outcome: landing::TurnOutcome::Succeeded,
            turn: "turn-2".into(),
            prompt_line: "Change the file".into(),
        };
        landing::record_turn_end(&db, &workspace, &turn).unwrap();
        let second = head_of();
        assert_ne!(second, first);
        assert_eq!(shown(asked(&first)), "changes_stale");
        assert!(shown(asked(&second)).contains("+second"));
        assert_eq!(shown(asked("not-a-commit")), "changes_stale");
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
            include_str!("../../../landing/clone.rs"),
            include_str!("../../../landing/diff.rs"),
            include_str!("../../../landing/flow.rs"),
            include_str!("../../../landing/git.rs"),
            include_str!("../../../landing/owner.rs"),
            include_str!("../../../landing/retire.rs"),
            include_str!("../../../landing/store.rs"),
        ]
        .into_iter()
        .flat_map(refusals)
        .collect();
        let protocol = refusals(include_str!("changes.rs"));
        assert!(engine.len() >= 15 && protocol.len() >= 5);
        let import = include_str!("../../../instruction_files.rs");
        let import_only = ["changes_import_none", "changes_import_staged"];
        for code in import_only {
            assert!(engine.contains(code), "{code}");
            assert!(
                import.contains(&format!("\"{code}\" => \"process_commit_")),
                "{code} reaches the owner as a process_commit code"
            );
        }
        for code in engine
            .iter()
            .chain(&protocol)
            .filter(|code| !import_only.contains(*code))
        {
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

    /// RVA-L1 guard (051798248's red on today's head-only API): the owner read a change set,
    /// committed on main and asked the agent to update, and the agent's next turn checked the
    /// reviewed head out again. An Accept naming the head the owner read must never land a
    /// revert of the owner's commit.
    #[cfg(unix)]
    #[test]
    fn a_reviewed_head_checked_out_after_an_update_lands_nothing_unseen() {
        let tmp = tempfile::Builder::new()
            .prefix("ca-chg-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let root = tmp.path();
        let (repo, home) = (root.join("repo"), root.join("home"));
        let (workspace, id) = recorded(root);
        let db = landing::open(root).unwrap();
        let (_, read) = landing::change_set(&db, &id).unwrap().unwrap();
        std::fs::write(repo.join("OWNER"), "owner work\n").unwrap();
        git(&home, &repo, &["add", "OWNER"]);
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
                "Owner work",
            ],
        );
        let owner_tip = git(&home, &repo, &["rev-parse", "HEAD"]);
        landing::update(&db, &id).unwrap();
        let (_, updated) = landing::change_set(&db, &id).unwrap().unwrap();
        assert_eq!(updated.base, owner_tip.trim());
        git(
            &home,
            &workspace.worktree,
            &["checkout", "-q", "--detach", &read.head],
        );
        let turn = landing::TurnEnd {
            outcome: landing::TurnOutcome::Succeeded,
            turn: "turn-2".into(),
            prompt_line: "Resolve the update".into(),
        };
        let turned = landing::record_turn_end(&db, &workspace, &turn).map_err(|problem| problem.0);
        let after = landing::change_set(&db, &id).unwrap().unwrap().1;
        std::fs::write(
            home.join(".gitconfig"),
            "[user]\n\tname = O\n\temail = o@example.invalid\n",
        )
        .unwrap();
        let accepted = landing::accept(
            &db,
            &home,
            &id,
            &read.head,
            None,
            crate::activity::AcceptPlace::Changes,
        )
        .map_err(|problem| problem.0);
        let kept = git(&home, &repo, &["ls-tree", "--name-only", "HEAD", "OWNER"]);
        assert_eq!(
            kept.trim(),
            "OWNER",
            "the owner's commit is reverted unseen: turn end {turned:?}, set now base {} head {} (read base {} head {}), accept {accepted:?}",
            after.base,
            after.head,
            read.base,
            read.head
        );
    }
}
