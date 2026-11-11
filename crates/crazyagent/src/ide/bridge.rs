use crate::engine::EngineError;
use crate::engine::owner::transport::changes::CHANGES_TIMEOUT;
use crate::engine::owner::{OwnerRuntimeHandle, failure};
use crate::landing::ChangeSetId;
use crate::platform::unix_socket::UnixStream;
use crate::protocol::{ProjectId, RequestId, encode_frame, plain_code};
use crate::sandbox::policy::AgentAccess;
use crate::terminal::{AgentId, SIZE_MAX, SIZE_MIN};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::broadcast;
use tokio::time::Instant;

pub mod agents;
pub mod memory;
mod owner_edit;
pub mod reveal;
pub mod run_command;

/// The first four handshake bytes of an IDE capability connection on the owner socket.
pub const MAGIC: &[u8; 4] = b"CAI1";

/// The IDE server environment variable naming its capability file.
pub const CAPABILITY_ENV: &str = "CRAZYAGENT_IDE_CAPABILITY_FILE";

/// The IDE server environment variable naming the owner socket its extension connects to.
pub const SOCKET_ENV: &str = "CRAZYAGENT_IDE_SOCKET";

/// The most IDE connections the owner server serves at once, apart from its owner clients.
pub const CONNECTIONS_MAX: usize = 16;

/// The built-in extension's folder name in an extensions folder: publisher.name-version.
pub const EXTENSION: &str = "crazyagent.crazyagent-ide-0.1.0";

/// The built-in extension's files by path inside its folder: its manifest, the
/// JavaScript the repository's tsc compiled from `apps/ide-extension/src` and the two
/// color themes its manifest contributes.
pub const EXTENSION_FILES: [(&str, &[u8]); 30] = [
    (
        "package.json",
        include_bytes!("../../../../apps/ide-extension/package.json"),
    ),
    (
        "out/engine.js",
        include_bytes!("../../../../apps/ide-extension/out/engine.js"),
    ),
    (
        "out/extension.js",
        include_bytes!("../../../../apps/ide-extension/out/extension.js"),
    ),
    (
        "out/sidebar/agents.js",
        include_bytes!("../../../../apps/ide-extension/out/sidebar/agents.js"),
    ),
    (
        "out/sidebar/changes.js",
        include_bytes!("../../../../apps/ide-extension/out/sidebar/changes.js"),
    ),
    (
        "out/sidebar/context.js",
        include_bytes!("../../../../apps/ide-extension/out/sidebar/context.js"),
    ),
    (
        "out/sidebar/memories.js",
        include_bytes!("../../../../apps/ide-extension/out/sidebar/memories.js"),
    ),
    (
        "out/sidebar/model.js",
        include_bytes!("../../../../apps/ide-extension/out/sidebar/model.js"),
    ),
    (
        "out/sidebar/tree.js",
        include_bytes!("../../../../apps/ide-extension/out/sidebar/tree.js"),
    ),
    (
        "out/base-content.js",
        include_bytes!("../../../../apps/ide-extension/out/base-content.js"),
    ),
    (
        "out/comments.js",
        include_bytes!("../../../../apps/ide-extension/out/comments.js"),
    ),
    (
        "out/scm-model.js",
        include_bytes!("../../../../apps/ide-extension/out/scm-model.js"),
    ),
    (
        "out/scm.js",
        include_bytes!("../../../../apps/ide-extension/out/scm.js"),
    ),
    (
        "out/readonly.js",
        include_bytes!("../../../../apps/ide-extension/out/readonly.js"),
    ),
    (
        "out/worktree.js",
        include_bytes!("../../../../apps/ide-extension/out/worktree.js"),
    ),
    (
        "out/lock.js",
        include_bytes!("../../../../apps/ide-extension/out/lock.js"),
    ),
    (
        "out/lock-model.js",
        include_bytes!("../../../../apps/ide-extension/out/lock-model.js"),
    ),
    (
        "out/extensions.js",
        include_bytes!("../../../../apps/ide-extension/out/extensions.js"),
    ),
    (
        "out/reveal.js",
        include_bytes!("../../../../apps/ide-extension/out/reveal.js"),
    ),
    (
        "out/reveal-path.js",
        include_bytes!("../../../../apps/ide-extension/out/reveal-path.js"),
    ),
    (
        "out/pty.js",
        include_bytes!("../../../../apps/ide-extension/out/pty.js"),
    ),
    (
        "out/terminal.js",
        include_bytes!("../../../../apps/ide-extension/out/terminal.js"),
    ),
    (
        "out/trust.js",
        include_bytes!("../../../../apps/ide-extension/out/trust.js"),
    ),
    (
        "out/trust-model.js",
        include_bytes!("../../../../apps/ide-extension/out/trust-model.js"),
    ),
    (
        "themes/crazyagent-dark.json",
        include_bytes!("../../../../apps/ide-extension/themes/crazyagent-dark.json"),
    ),
    (
        "themes/crazyagent-light.json",
        include_bytes!("../../../../apps/ide-extension/themes/crazyagent-light.json"),
    ),
    (
        "out/git.js",
        include_bytes!("../../../../apps/ide-extension/out/git.js"),
    ),
    (
        "out/git-forward.js",
        include_bytes!("../../../../apps/ide-extension/out/git-forward.js"),
    ),
    (
        "out/run-command.js",
        include_bytes!("../../../../apps/ide-extension/out/run-command.js"),
    ),
    (
        "out/run-command-model.js",
        include_bytes!("../../../../apps/ide-extension/out/run-command-model.js"),
    ),
];

const GRANTS_MAX: usize = 64;
const SUBSCRIBERS_MAX: usize = 4;
const EVENTS_QUEUED: usize = 64;
const WRITE_TIME: Duration = Duration::from_secs(10);
/// How long a Changes read may run: under the IDE extension's 30 s request timer, so a read
/// the extension gave up on never holds one of the `CONNECTIONS_MAX` connections into its
/// next poll, and the extension reads the refusal instead of closing first.
const READ_TIME: Duration = Duration::from_secs(28);
/// How long a diff comment, an owner edit, a message to an agent, a memory or extension
/// decision, an extension request or the show-all switch may take before the IDE is told
/// its outcome is unknown: past the handshake deadline, so a slow write reports what it
/// did, and before the extension stops waiting.
const IDE_WRITE_TIME: Duration = Duration::from_secs(28);
const PATH_BYTES_MAX: usize = 4096;
const TEXT_BYTES_MAX: usize = 16384;
/// The most bytes one diff comment may hold, so with its file and line it fits one typed
/// agent prompt and one team conversation message.
pub(super) const COMMENT_BYTES_MAX: usize = 2048;
const NAME_BYTES_MAX: usize = 128;
const VERSION_BYTES_MAX: usize = 64;
/// The most arguments one `ide.git` call carries after `git`, and the most bytes each holds.
pub(super) const GIT_ARGS_MAX: usize = 16;
pub(super) const GIT_ARG_BYTES_MAX: usize = 1024;
/// How long one IDE git network command may run in the engine before its process group is
/// killed: a push of a long history over a slow link still lands.
pub(super) const GIT_TIME: Duration = Duration::from_secs(600);
/// How long past `GIT_TIME` an `ide.git` call may take to answer: its checks before git
/// runs and the reply. The bound stays under the extension's git request timer.
const GIT_ANSWER_MARGIN: Duration = Duration::from_secs(60);
const CAPABILITY: &str = "capability";
/// The IDE server's profile file in its extensions folder: the extensions it loads. At start
/// the server deletes every extension folder this file does not list.
const PROFILE: &str = "extensions.json";
const PROFILE_BYTES_MAX: u64 = 1024 * 1024;
const PROFILE_ENTRIES_MAX: usize = 256;

/// The path of a capability file: `<engine root>/ide/<project>/capability`.
pub type CapabilityPath = PathBuf;

/// One request from the IDE extension: the project its capability names and one call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdeRequest {
    pub project: ProjectId,
    pub call: IdeCall,
}

/// The calls an IDE capability may make, by their wire names, each with its arguments.
/// Nothing else is served on an IDE connection: no owner request, no other project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "name", content = "args", deny_unknown_fields)]
pub enum IdeCall {
    #[serde(rename = "ide.workspace")]
    Workspace {},
    #[serde(rename = "ide.show_all_agents")]
    ShowAllAgents { on: bool },
    #[serde(rename = "ide.change_sets")]
    ChangeSets {},
    #[serde(rename = "ide.change_diff")]
    ChangeDiff {
        id: ChangeSetId,
        head: String,
        page: u16,
    },
    #[serde(rename = "ide.base_blob")]
    BaseBlob {
        id: ChangeSetId,
        head: String,
        path: String,
        page: u16,
    },
    #[serde(rename = "ide.config_blob")]
    ConfigBlob {
        id: ChangeSetId,
        head: String,
        path: String,
        page: u16,
    },
    #[serde(rename = "ide.head_blob")]
    HeadBlob {
        id: ChangeSetId,
        head: String,
        path: String,
        page: u16,
    },
    #[serde(rename = "ide.accept")]
    Accept {
        id: ChangeSetId,
        head: String,
        message: String,
    },
    #[serde(rename = "ide.discard")]
    Discard { id: ChangeSetId, head: String },
    #[serde(rename = "ide.restore")]
    Restore { id: ChangeSetId },
    #[serde(rename = "ide.comment")]
    Comment {
        id: ChangeSetId,
        path: String,
        line: u32,
        text: String,
    },
    #[serde(rename = "ide.owner_edit")]
    OwnerEdit { agent: AgentId, path: String },
    #[serde(rename = "ide.memories")]
    Memories {
        #[serde(default)]
        after: Option<RequestId>,
    },
    #[serde(rename = "ide.memory_decide")]
    MemoryDecide {
        id: RequestId,
        revision: u64,
        decision: Decision,
    },
    #[serde(rename = "ide.agents")]
    Agents {},
    #[serde(rename = "ide.agent_message")]
    AgentMessage { agent: i64, text: String },
    #[serde(rename = "ide.extensions")]
    Extensions {},
    #[serde(rename = "ide.extension_request")]
    ExtensionRequest { id: String, version: String },
    #[serde(rename = "ide.extension_decide")]
    ExtensionDecide {
        id: String,
        version: String,
        approve: bool,
    },
    #[serde(rename = "ide.reveal_ack")]
    RevealAck { request: RequestId },
    #[serde(rename = "ide.run_ack")]
    RunAck { request: RequestId },
    #[serde(rename = "ide.events")]
    Events {},
    #[serde(rename = "ide.session")]
    Session {},
    #[serde(rename = "ide.terminal")]
    Terminal {
        cols: u16,
        rows: u16,
        level: AgentAccess,
    },
    #[serde(rename = "ide.git")]
    Git { args: Vec<String>, folder: String },
}

/// The owner's decision on a memory nomination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approve,
    Reject,
}

/// One engine frame to the extension: a call's data, a stable refusal code, a stale write
/// refused with the current version, value and writer (code `state_stale`), or an event
/// on an `ide.events` subscription.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum IdeReply {
    Ok {
        data: serde_json::Value,
    },
    Rejected {
        code: String,
    },
    Stale {
        code: String,
        version: u64,
        current: serde_json::Value,
        writer: String,
    },
    Event {
        event: IdeEvent,
    },
}

/// An event the engine pushes to a project's IDE: something to refresh, a file the owner
/// asked to reveal from the TUI, or one command the owner confirmed in the desktop to type
/// into a new terminal, with or without Enter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "name", rename_all = "snake_case", deny_unknown_fields)]
pub enum IdeEvent {
    WorkspaceChanged,
    ChangeSetsChanged,
    AgentsChanged,
    MemoriesChanged,
    Reveal {
        request: RequestId,
        path: String,
    },
    RunCommand {
        request: RequestId,
        command: String,
        press_enter: bool,
    },
}

impl IdeRequest {
    /// Checks every argument's bounds before any handler runs: paths are relative, at most
    /// `PATH_BYTES_MAX` bytes and free of NUL and `..`; texts fit `TEXT_BYTES_MAX`, a comment
    /// fits `COMMENT_BYTES_MAX` and is not blank on a line from 1; extension ids are
    /// `publisher.name`; a git call has 1 to `GIT_ARGS_MAX` arguments of 1 to
    /// `GIT_ARG_BYTES_MAX` bytes without NUL and an absolute folder path.
    /// Input: the request. Output: Ok, or `ide_request_invalid`.
    pub fn validate(&self) -> Result<(), EngineError> {
        let valid = match &self.call {
            IdeCall::BaseBlob { path, .. }
            | IdeCall::ConfigBlob { path, .. }
            | IdeCall::HeadBlob { path, .. }
            | IdeCall::OwnerEdit { path, .. } => relative(path),
            IdeCall::Comment {
                path, line, text, ..
            } => {
                relative(path)
                    && *line > 0
                    && text.len() <= COMMENT_BYTES_MAX
                    && !text.trim().is_empty()
            }
            IdeCall::Accept { message, .. } => message.len() <= TEXT_BYTES_MAX,
            IdeCall::AgentMessage { text, .. } => text.len() <= TEXT_BYTES_MAX,
            IdeCall::Terminal { cols, rows, .. } => {
                (SIZE_MIN..=SIZE_MAX).contains(cols) && (SIZE_MIN..=SIZE_MAX).contains(rows)
            }
            IdeCall::ExtensionRequest { id, version }
            | IdeCall::ExtensionDecide { id, version, .. } => {
                extension_id(id) && extension_version(version)
            }
            IdeCall::Git { args, folder } => {
                (1..=GIT_ARGS_MAX).contains(&args.len())
                    && args.iter().all(|arg| {
                        !arg.is_empty() && arg.len() <= GIT_ARG_BYTES_MAX && !arg.contains('\0')
                    })
                    && folder.starts_with('/')
                    && folder.len() <= PATH_BYTES_MAX
                    && !folder.contains('\0')
            }
            IdeCall::Workspace {}
            | IdeCall::ShowAllAgents { .. }
            | IdeCall::ChangeSets {}
            | IdeCall::ChangeDiff { .. }
            | IdeCall::Discard { .. }
            | IdeCall::Restore { .. }
            | IdeCall::Memories { .. }
            | IdeCall::MemoryDecide { .. }
            | IdeCall::Agents {}
            | IdeCall::Extensions {}
            | IdeCall::RevealAck { .. }
            | IdeCall::RunAck { .. }
            | IdeCall::Events {}
            | IdeCall::Session {} => true,
        };
        if !valid {
            return Err(failure("ide_request_invalid"));
        }
        Ok(())
    }
}

/// Whether a path is relative, 1 to `PATH_BYTES_MAX` bytes, without NUL or a `..` part.
fn relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= PATH_BYTES_MAX
        && !path.starts_with('/')
        && !path.contains('\0')
        && path.split('/').all(|part| part != "..")
}

/// The most bytes of a path an engine-written line names, as the IDE comment frame and the
/// owner-edit note show it.
pub(super) const NOTE_PATH_BYTES_MAX: usize = 1024;

/// Engine or agent text as one plain line inside an engine-written message: control
/// characters become U+FFFD, square brackets and their look-alikes become parentheses so no
/// `[owner]` tag can show, and it is cut to `max` bytes. Pure, so every platform's owner-edit
/// note uses it. Inputs: the text and the bound. Output: the line.
pub(super) fn shown(text: &str, max: usize) -> String {
    let plain: String = text
        .chars()
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect();
    let line = crate::team::bound_text(&crate::team::events::plain_agent_text(&plain), max);
    assert!(
        !line.chars().any(char::is_control),
        "a shown line has no control character"
    );
    assert!(line.len() <= max, "a shown line keeps its bound");
    line
}

/// Whether an extension id is `publisher.name`, each part 1 to `NAME_BYTES_MAX` ASCII
/// letters, digits, hyphens or underscores.
pub(super) fn extension_id(id: &str) -> bool {
    let part = |text: &str| {
        !text.is_empty()
            && text.len() <= NAME_BYTES_MAX
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    };
    id.split_once('.')
        .is_some_and(|(publisher, name)| part(publisher) && part(name))
}

/// Whether an extension version is 1 to `VERSION_BYTES_MAX` ASCII letters, digits, dots,
/// hyphens or plus signs.
pub(super) fn extension_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= VERSION_BYTES_MAX
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte))
}

/// What the bridge needs from a running owner server: its capability grants, the engine
/// root that holds `ide/`, and the owner socket the extension connects to.
pub(crate) struct IdeHost<'a> {
    pub(crate) grants: &'a Grants,
    pub(crate) root: PathBuf,
    pub(crate) socket: PathBuf,
}

/// The live IDE capabilities of one owner server, at most one per project and
/// `GRANTS_MAX` in all. Each holds its token, its file, whether the extension took its
/// session yet and the sender of its project's events; dropping a grant kills its token
/// and ends its subscriptions. The reveals the TUI sent wait here for their IDE to
/// acknowledge them, and beside them run the Sandboxed IDE terminals and the `ide.git`
/// calls of the owner server.
#[derive(Default)]
pub(crate) struct Grants {
    live: Mutex<Vec<Grant>>,
    reveals: reveal::Reveals,
    #[cfg(unix)]
    terminals: std::sync::Arc<Mutex<terminal::Terminals>>,
    #[cfg(unix)]
    git_calls: std::sync::Arc<Mutex<remote_git::GitCalls>>,
    #[cfg(unix)]
    index_calls: std::sync::Arc<Mutex<remote_git::GitCalls>>,
    #[cfg(unix)]
    index_gates: remote_git::index::Gates,
}

/// One project's capability for one IDE server launch. Until `taken`, the token is the
/// file's and admits only `ide.session`; after it, the token is the session's, the file
/// is gone, every other call is admitted and `spent` keeps the file's token only to know
/// a second try to take the session. Dropping `ended` wakes the grant's terminals.
struct Grant {
    project: ProjectId,
    token: [u8; 32],
    spent: Option<[u8; 32]>,
    file: PathBuf,
    identity: (u64, u64),
    taken: bool,
    events: broadcast::Sender<IdeEvent>,
    #[cfg(unix)]
    ended: tokio::sync::watch::Sender<()>,
}

impl Grants {
    /// Issues a new capability for a project, replacing the project's earlier one: a fresh
    /// 32-byte token written as 64 hex digits to a new 0600 file in a 0700 project folder.
    /// Inputs: the engine root and the project. Output: the file path, or the refusal.
    pub(crate) fn issue(
        &self,
        root: &Path,
        project: &ProjectId,
    ) -> Result<CapabilityPath, EngineError> {
        if project.as_str() == "." {
            return Err(failure("ide_project_invalid"));
        }
        let mut live = self.live.lock().map_err(|_| failure("ide_unavailable"))?;
        let earlier = live.iter().position(|grant| &grant.project == project);
        if earlier.is_none() && live.len() >= GRANTS_MAX {
            return Err(failure("ide_capability_capacity"));
        }
        let folder = root.join("ide");
        private_folder(&folder)?;
        let folder = folder.join(project.as_str());
        private_folder(&folder)?;
        let mut token = [0; 32];
        getrandom::fill(&mut token).map_err(|_| failure("ide_entropy_unavailable"))?;
        if let Some(index) = earlier {
            live.swap_remove(index);
        }
        let file = folder.join(CAPABILITY);
        let identity = write_capability(&file, &token)?;
        let (events, _) = broadcast::channel(EVENTS_QUEUED);
        live.push(Grant {
            project: project.clone(),
            token,
            spent: None,
            file: file.clone(),
            identity,
            taken: false,
            events,
            #[cfg(unix)]
            ended: tokio::sync::watch::channel(()).0,
        });
        assert!(live.len() <= GRANTS_MAX);
        assert_eq!(
            live.iter()
                .filter(|grant| &grant.project == project)
                .count(),
            1
        );
        Ok(file)
    }

    /// Admits an IDE handshake: `CAI1`, slot zero and a token compared in constant time
    /// with every live token. A capability file's token used after its swap ends its grant,
    /// as `second_session` does. Input: the handshake packet. Output: the project its
    /// capability names and whether its session was taken, or `second_session`'s refusal.
    fn known(&self, packet: &[u8; 37]) -> Result<(ProjectId, bool), EngineError> {
        let found = {
            let live = self.live.lock().map_err(|_| failure("ide_unavailable"))?;
            matching(&live, packet).map(|index| (live[index].project.clone(), live[index].taken))
        };
        found.ok_or_else(|| self.second_session(packet))
    }

    /// Admits an IDE handshake with a session token. Input: the handshake packet. Output:
    /// the project its capability names, `ide_capability_refused` or, for the capability
    /// file's token, `ide_session_required`.
    pub(crate) fn admit(&self, packet: &[u8; 37]) -> Result<ProjectId, EngineError> {
        match self.known(packet)? {
            (project, true) => Ok(project),
            (_, false) => Err(failure("ide_session_required")),
        }
    }

    /// Swaps the capability file's token for a fresh session token, then removes the file
    /// while it is still the file the engine wrote, so a process that reads it later gets
    /// a dead token. Each grant has one session: a second try ends the grant through
    /// `second_session`. Input: the handshake packet. Output: the session token, or
    /// `second_session`'s refusal, `ide_entropy_unavailable` or the file's cleanup failure.
    fn session(&self, packet: &[u8; 37]) -> Result<[u8; 32], EngineError> {
        let mut token = [0; 32];
        getrandom::fill(&mut token).map_err(|_| failure("ide_entropy_unavailable"))?;
        let swapped = {
            let mut live = self.live.lock().map_err(|_| failure("ide_unavailable"))?;
            match matching(&live, packet) {
                Some(index) if !live[index].taken => {
                    let grant = &mut live[index];
                    grant.spent = Some(grant.token);
                    grant.token = token;
                    grant.taken = true;
                    assert!(live.iter().filter(|grant| grant.token == token).count() == 1);
                    Some((live[index].file.clone(), live[index].identity))
                }
                _ => None,
            }
        };
        let Some((file, identity)) = swapped else {
            return Err(self.second_session(packet));
        };
        remove_capability(&file, identity)?;
        Ok(token)
    }

    /// Ends the grant a handshake names when it tries to take a session the grant already
    /// gave, with the file's spent token or with the session token itself: two holders of
    /// one capability mean it leaked, so the grant is dropped, which kills the session token
    /// and ends its subscriptions and terminals. Input: the packet. Output:
    /// `ide_session_taken` once ended, `ide_capability_refused` when no grant matches, or
    /// the file's cleanup failure.
    fn second_session(&self, packet: &[u8; 37]) -> EngineError {
        let grant = match self.live.lock() {
            Ok(mut live) => matching(&live, packet)
                .or_else(|| spent(&live, packet))
                .map(|index| live.swap_remove(index)),
            Err(_) => return failure("ide_unavailable"),
        };
        let Some(grant) = grant else {
            return failure("ide_capability_refused");
        };
        assert!(grant.taken && grant.spent.is_some());
        remove_capability(&grant.file, grant.identity)
            .err()
            .unwrap_or_else(|| failure("ide_session_taken"))
    }

    /// Subscribes a handshake's capability to its project's events, at most
    /// `SUBSCRIBERS_MAX` at once. Input: the handshake packet. Output: the receiver, or
    /// `ide_capability_refused` or `ide_events_busy`.
    fn subscribe(&self, packet: &[u8; 37]) -> Result<broadcast::Receiver<IdeEvent>, EngineError> {
        self.find(packet, |grant| {
            if !grant.taken {
                return Err(failure("ide_session_required"));
            }
            if grant.events.receiver_count() >= SUBSCRIBERS_MAX {
                return Err(failure("ide_events_busy"));
            }
            let receiver = grant.events.subscribe();
            assert!(grant.events.receiver_count() <= SUBSCRIBERS_MAX);
            Ok(receiver)
        })
    }

    /// Watches the grant a handshake's session token names, for one of its terminals: the
    /// receiver's `changed` ends once the grant is revoked, replaced or ended, so the
    /// terminal stops then, not at its next grant check (V21-SEC-2-L1). Input: the packet.
    /// Output: the receiver, `ide_session_required` or `ide_capability_refused`.
    #[cfg(unix)]
    fn watch(&self, packet: &[u8; 37]) -> Result<tokio::sync::watch::Receiver<()>, EngineError> {
        self.find(packet, |grant| {
            if !grant.taken {
                return Err(failure("ide_session_required"));
            }
            Ok(grant.ended.subscribe())
        })
    }

    /// Runs `pick` on the live grant a handshake's token matches, under the grants lock.
    /// Inputs: the packet and the pick. Output: its result, or `ide_capability_refused`.
    fn find<T>(
        &self,
        packet: &[u8; 37],
        pick: impl FnOnce(&Grant) -> Result<T, EngineError>,
    ) -> Result<T, EngineError> {
        let live = self.live.lock().map_err(|_| failure("ide_unavailable"))?;
        match matching(&live, packet) {
            Some(index) => pick(&live[index]),
            None => Err(failure("ide_capability_refused")),
        }
    }

    /// Sends an event to a project's live subscriptions; nothing happens without a
    /// capability or a subscriber. Inputs: the project and the event.
    fn publish(&self, project: &ProjectId, event: IdeEvent) {
        if let Ok(live) = self.live.lock()
            && let Some(grant) = live.iter().find(|grant| &grant.project == project)
        {
            let _ = grant.events.send(event);
        }
    }

    /// Revokes a project's capability: its token stops working, its subscriptions end and
    /// its file is removed while it is still the file the engine wrote. Revoking a project
    /// without one is Ok. Input: the project. Output: Ok, or the cleanup failure.
    pub(crate) fn revoke(&self, project: &ProjectId) -> Result<(), EngineError> {
        let grant = {
            let mut live = self.live.lock().map_err(|_| failure("ide_unavailable"))?;
            let index = live.iter().position(|grant| &grant.project == project);
            index.map(|index| live.swap_remove(index))
        };
        grant.map_or(Ok(()), |grant| {
            remove_capability(&grant.file, grant.identity)
        })
    }

    /// Revokes every capability, as the owner server does when it stops, trying every
    /// file even after one fails. Output: Ok, or the first cleanup failure.
    pub(crate) fn revoke_all(&self) -> Result<(), EngineError> {
        let grants =
            std::mem::take(&mut *self.live.lock().map_err(|_| failure("ide_unavailable"))?);
        let mut first = Ok(());
        for grant in &grants {
            let removed = remove_capability(&grant.file, grant.identity);
            if first.is_ok() {
                first = removed;
            }
        }
        first
    }
}

/// Finds the live grant a handshake names: `CAI1`, slot zero and a token compared in
/// constant time with every live token. Inputs: the grants and the packet. Output: the
/// grant's index, or none.
fn matching(live: &[Grant], packet: &[u8; 37]) -> Option<usize> {
    holding(live, packet, |grant| Some(&grant.token))
}

/// Finds the grant whose capability file's spent token a handshake carries, compared in
/// constant time with every spent token. Inputs: the grants and the packet. Output: the
/// grant's index, or none.
fn spent(live: &[Grant], packet: &[u8; 37]) -> Option<usize> {
    holding(live, packet, |grant| grant.spent.as_ref())
}

/// Finds the grant one of whose tokens a handshake carries: `CAI1`, slot zero and every
/// grant's token compared in constant time. Inputs: the grants, the packet and the token
/// to compare. Output: the grant's index, or none.
fn holding(
    live: &[Grant],
    packet: &[u8; 37],
    token: impl Fn(&Grant) -> Option<&[u8; 32]>,
) -> Option<usize> {
    assert!(live.len() <= GRANTS_MAX);
    if &packet[..4] != MAGIC || packet[4] != 0 {
        return None;
    }
    let mut found = None;
    for (index, grant) in live.iter().enumerate() {
        if token(grant).is_some_and(|held| bool::from(held.ct_eq(&packet[5..]))) {
            found = Some(index);
        }
    }
    found
}

/// Issues the capability one IDE server launch hands its extension: it allows only the
/// IDE calls, only for this project, until `revoke` or the owner server stops. The file
/// holds a one-time token the extension swaps for its session with `ide.session`.
/// Inputs: the owner runtime and the project. Output: the capability file path.
pub fn issue(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
) -> Result<CapabilityPath, EngineError> {
    let host = runtime.ide()?;
    host.grants.issue(&host.root, project)
}

/// Revokes a project's capability when its IDE server stops; see `Grants::revoke`.
/// Inputs: the owner runtime and the project. Output: Ok, or the cleanup failure.
pub fn revoke(runtime: &OwnerRuntimeHandle, project: &ProjectId) -> Result<(), EngineError> {
    runtime.ide_grants().revoke(project)
}

/// Pushes an event to the project's IDE subscriptions; the extension refreshes on it.
/// Inputs: the owner runtime, the project and the event. Output: none.
pub fn publish(runtime: &OwnerRuntimeHandle, project: &ProjectId, event: IdeEvent) {
    runtime.ide_grants().publish(project, event);
}

/// The two variables an IDE server launch adds to its environment: the capability file
/// path and the owner socket path. The token itself never enters the environment.
/// Inputs: the owner runtime and the issued capability path. Output: the pairs.
pub fn launch_env(
    runtime: &OwnerRuntimeHandle,
    capability: &Path,
) -> Result<Vec<(OsString, OsString)>, EngineError> {
    let host = runtime.ide()?;
    Ok(vec![
        (CAPABILITY_ENV.into(), capability.into()),
        (SOCKET_ENV.into(), host.socket.into()),
    ])
}

/// Installs the built-in extension into an IDE server's extensions folder as one folder,
/// leaving an identical install untouched and replacing any other one whole, then lists it
/// in the folder's profile file ([`register_extension`]) so the server loads it.
/// Input: the extensions folder. Output: the extension's folder.
pub fn install_extension(extensions: &Path) -> Result<PathBuf, EngineError> {
    let folder = extensions.join(EXTENSION);
    let installed = EXTENSION_FILES
        .iter()
        .all(|(name, bytes)| std::fs::read(folder.join(name)).is_ok_and(|found| found == *bytes));
    if installed {
        register_extension(extensions, &folder)?;
        return Ok(folder);
    }
    let failed = |_| failure("ide_extension_install_failed");
    let mut suffix = [0; 8];
    getrandom::fill(&mut suffix).map_err(|_| failure("ide_entropy_unavailable"))?;
    let staging = extensions.join(format!(".{EXTENSION}-{}", hex(&suffix)));
    let aside = extensions.join(format!(".{EXTENSION}-old-{}", hex(&suffix)));
    let staged = EXTENSION_FILES.iter().try_for_each(|(name, bytes)| {
        let path = staging.join(name);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, bytes)
    });
    if staged.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(failure("ide_extension_install_failed"));
    }
    let moved = match std::fs::rename(&folder, &aside) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(failure("ide_extension_install_failed"));
        }
    };
    if std::fs::rename(&staging, &folder).is_err() {
        let _ = std::fs::remove_dir_all(&staging);
        if moved {
            let _ = std::fs::rename(&aside, &folder);
        }
        return Err(failure("ide_extension_install_failed"));
    }
    if moved {
        std::fs::remove_dir_all(&aside).map_err(failed)?;
    }
    register_extension(extensions, &folder)?;
    Ok(folder)
}

/// Lists the built-in extension in an IDE server's profile file, which the server loads its
/// extensions from and without which it deletes the built-in's folder at start. Every other
/// entry is kept, any earlier entry for the built-in is replaced, and a changed list
/// replaces the file whole through a new file. Inputs: the extensions folder and the
/// built-in's folder in it. Output: Ok, or `ide_extension_install_failed`.
fn register_extension(extensions: &Path, folder: &Path) -> Result<(), EngineError> {
    let failed = || failure("ide_extension_install_failed");
    assert_eq!(folder.parent(), Some(extensions));
    let (id, version) = EXTENSION
        .rsplit_once('-')
        .expect("the built-in extension folder is its id, a hyphen and its version");
    let location = folder.to_str().ok_or_else(failed)?;
    let profile = extensions.join(PROFILE);
    let earlier: Vec<serde_json::Value> = match std::fs::symlink_metadata(&profile) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Ok(found) if found.is_file() && found.len() <= PROFILE_BYTES_MAX => {
            let text = std::fs::read(&profile).map_err(|_| failed())?;
            serde_json::from_slice(&text).map_err(|_| failed())?
        }
        _ => return Err(failed()),
    };
    if earlier.len() > PROFILE_ENTRIES_MAX {
        return Err(failed());
    }
    let built_in = |entry: &serde_json::Value| {
        entry
            .pointer("/identifier/id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|known| known.eq_ignore_ascii_case(id))
    };
    let mut entries: Vec<serde_json::Value> = earlier
        .iter()
        .filter(|entry| !built_in(entry))
        .cloned()
        .collect();
    entries.push(serde_json::json!({
        "identifier": { "id": id },
        "version": version,
        "location": { "$mid": 1, "path": location, "scheme": "file" },
        "relativeLocation": EXTENSION,
    }));
    assert_eq!(entries.iter().filter(|entry| built_in(entry)).count(), 1);
    assert!(entries.len() <= PROFILE_ENTRIES_MAX + 1);
    if entries == earlier {
        return Ok(());
    }
    let text = serde_json::to_vec(&entries).map_err(|_| failed())?;
    let mut suffix = [0; 8];
    getrandom::fill(&mut suffix).map_err(|_| failure("ide_entropy_unavailable"))?;
    let staging = extensions.join(format!(".{PROFILE}-{}", hex(&suffix)));
    let written = std::fs::write(&staging, text).and_then(|()| std::fs::rename(&staging, &profile));
    if written.is_err() {
        let _ = std::fs::remove_file(&staging);
        return Err(failed());
    }
    Ok(())
}

/// Serves one IDE capability connection after its handshake: admits the token, reads one
/// request for the capability's own project and answers it, swaps the capability file for
/// the extension's session, or turns the connection into the project's event subscription
/// or a Sandboxed terminal. An answer is sent only while the capability still admits the
/// handshake, except what a write or git command did, which is reported even when its
/// capability was revoked while it ran; a refusal is sent as it is. The handshake deadline
/// bounds the request and every call but the subscription, the terminal, the Changes reads,
/// the writes and `ide.git`, which run under `bound`; a write or git command past it answers
/// its `outcome_unknown` code.
/// Inputs: the stream, handshake packet, owner runtime and deadline.
pub(crate) async fn serve(
    stream: UnixStream,
    packet: [u8; 37],
    runtime: OwnerRuntimeHandle,
    deadline: Instant,
) {
    serve_within(stream, packet, runtime, deadline, IDE_WRITE_TIME).await;
}

/// [`serve`] with the time a write other than Accept, Discard, Restore and a git command
/// may take before its outcome is unknown, which [`serve`] sets to `IDE_WRITE_TIME`.
/// Inputs: the stream, handshake packet, owner runtime, deadline and that time.
async fn serve_within(
    mut stream: UnixStream,
    packet: [u8; 37],
    runtime: OwnerRuntimeHandle,
    deadline: Instant,
    write_time: Duration,
) {
    let opened = tokio::time::timeout_at(deadline, open(&mut stream, &packet, &runtime)).await;
    let (reply, wrote) = match opened {
        Err(_) => (None, false),
        Ok(Err(error)) => (Some(refusal(&error)), false),
        Ok(Ok((_, IdeCall::Events {}))) => {
            events(&mut stream, &packet, &runtime).await;
            (None, false)
        }
        Ok(Ok((_, IdeCall::Session {}))) => {
            let reply = match runtime.ide_grants().session(&packet) {
                Ok(token) => IdeReply::Ok {
                    data: serde_json::json!({ "token": hex(&token) }),
                },
                Err(error) => refusal(&error),
            };
            let _ = write(&mut stream, &reply).await;
            let _ = stream.shutdown().await;
            return;
        }
        #[cfg(unix)]
        Ok(Ok((project, IdeCall::Terminal { cols, rows, level }))) => {
            terminal::serve(stream, packet, runtime, project, (cols, rows), level).await;
            return;
        }
        #[cfg(not(unix))]
        Ok(Ok((_, IdeCall::Terminal { .. }))) => {
            (Some(refusal(&failure("ide_terminal_unavailable"))), false)
        }
        Ok(Ok((project, call))) => {
            let unknown = outcome_unknown(&call);
            let bound = bound(&call, deadline, write_time);
            let answered = tokio::time::timeout_at(bound, answer(&runtime, &project, call)).await;
            let reply = match (answered, unknown) {
                (Ok(Ok(reply)), _) => reply,
                (Ok(Err(error)), _) => refusal(&error),
                (Err(_), Some(code)) => refusal(&failure(code)),
                (Err(_), None) => refusal(&failure("ide_request_timeout")),
            };
            (Some(reply), unknown.is_some())
        }
    };
    if let Some(reply) = reply {
        let reply = match reply {
            IdeReply::Rejected { .. } => reply,
            reply => match runtime.ide_grants().admit(&packet) {
                Err(error) if !wrote => refusal(&error),
                _ => reply,
            },
        };
        let _ = write(&mut stream, &reply).await;
    }
    let _ = stream.shutdown().await;
}

/// The code a write answers when its bound passes before it reports: it may already have
/// taken effect, so the owner checks before trying again rather than reading a plain
/// timeout. Accept, Discard and Restore keep the Changes code; a diff comment, an owner
/// edit, a message to an agent, a memory or extension decision, an extension request, the
/// show-all switch and a git command each have their own.
/// Input: the call. Output: the code, or None for a call that writes nothing.
fn outcome_unknown(call: &IdeCall) -> Option<&'static str> {
    Some(match call {
        IdeCall::Accept { .. } | IdeCall::Discard { .. } | IdeCall::Restore { .. } => {
            "changes_outcome_unknown"
        }
        IdeCall::Comment { .. } => "ide_comment_outcome_unknown",
        IdeCall::OwnerEdit { .. } => "ide_owner_edit_outcome_unknown",
        IdeCall::AgentMessage { .. } => "ide_agent_message_outcome_unknown",
        IdeCall::MemoryDecide { .. } => "ide_memory_decide_outcome_unknown",
        IdeCall::ExtensionDecide { .. } => "ide_extension_decide_outcome_unknown",
        IdeCall::ExtensionRequest { .. } => "ide_extension_request_outcome_unknown",
        IdeCall::ShowAllAgents { .. } => "ide_show_all_agents_outcome_unknown",
        IdeCall::Git { .. } => "ide_git_outcome_unknown",
        _ => return None,
    })
}

/// When one IDE call must be answered by. Accept, Discard and Restore run under the
/// desktop's Changes bound, so a slow commit still lands and reports what it did; the
/// other writes run under the write time for the same reason; the four Changes reads run
/// under READ_TIME, since the extension polls them again; a git command runs under
/// `GIT_TIME` and `GIT_ANSWER_MARGIN`; every other call keeps the
/// handshake deadline. Inputs: the call, the handshake deadline and the write time.
/// Output: the instant, never before the deadline.
fn bound(call: &IdeCall, deadline: Instant, write_time: Duration) -> Instant {
    let time = match call {
        IdeCall::Accept { .. } | IdeCall::Discard { .. } | IdeCall::Restore { .. } => {
            CHANGES_TIMEOUT
        }
        IdeCall::Comment { .. }
        | IdeCall::OwnerEdit { .. }
        | IdeCall::AgentMessage { .. }
        | IdeCall::MemoryDecide { .. }
        | IdeCall::ExtensionDecide { .. }
        | IdeCall::ExtensionRequest { .. }
        | IdeCall::ShowAllAgents { .. } => write_time,
        IdeCall::ChangeSets {}
        | IdeCall::ChangeDiff { .. }
        | IdeCall::BaseBlob { .. }
        | IdeCall::ConfigBlob { .. }
        | IdeCall::HeadBlob { .. } => READ_TIME,
        IdeCall::Git { .. } => GIT_TIME + GIT_ANSWER_MARGIN,
        _ => return deadline,
    };
    deadline.max(Instant::now() + time)
}

/// Reads the one request frame, then admits the handshake, so a refusal never leaves
/// unread bytes that could reset the connection before the client reads it. The capability
/// file's token admits only `ide.session`, and a session token admits everything else; a
/// session token asking for `ide.session` again ends its grant. Inputs: the stream, packet
/// and owner runtime. Output: the capability's project and the call, or
/// `ide_capability_refused`, `ide_request_invalid`, `ide_project_refused`,
/// `ide_session_required` or `ide_session_taken`.
async fn open(
    stream: &mut UnixStream,
    packet: &[u8; 37],
    runtime: &OwnerRuntimeHandle,
) -> Result<(ProjectId, IdeCall), EngineError> {
    let frame = crate::engine::read_frame(stream)
        .await
        .map_err(|_| failure("ide_request_invalid"))?;
    let (project, taken) = runtime.ide_grants().known(packet)?;
    let request: IdeRequest =
        serde_json::from_slice(&frame[4..]).map_err(|_| failure("ide_request_invalid"))?;
    request.validate()?;
    if request.project != project {
        return Err(failure("ide_project_refused"));
    }
    let session = matches!(request.call, IdeCall::Session {});
    match (session, taken) {
        (true, true) => Err(runtime.ide_grants().second_session(packet)),
        (true, false) | (false, true) => Ok((project, request.call)),
        (false, false) => Err(failure("ide_session_required")),
    }
}

/// Answers one call. Each later IDE slice moves its call from the unavailable list to its
/// own handler. Inputs: the owner runtime, the project and the call. Output: the reply.
async fn answer(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
    call: IdeCall,
) -> Result<IdeReply, EngineError> {
    let data = match call {
        IdeCall::Workspace {} => workspace(runtime, project, None).await?,
        IdeCall::ShowAllAgents { on } => workspace(runtime, project, Some(on)).await?,
        IdeCall::Extensions {} => extensions::listing(runtime, project).await?,
        IdeCall::ExtensionRequest { id, version } => {
            extensions::request(runtime, project, id, version).await?
        }
        IdeCall::ExtensionDecide {
            id,
            version,
            approve,
        } => extensions::decide(runtime, project, id, version, approve).await?,
        IdeCall::Memories { after } => memory::memories(runtime, project, after).await?,
        IdeCall::MemoryDecide {
            id,
            revision,
            decision,
        } => {
            let decide = memory::Decide {
                id,
                revision,
                decision,
            };
            return memory::decide(runtime, project, &decide).await;
        }
        IdeCall::Agents {} => agents::listed(runtime, project).await?,
        IdeCall::AgentMessage { agent, text } => {
            agents::sent(runtime, project, agent, text).await?
        }
        IdeCall::OwnerEdit { agent, path } => {
            owner_edit::answer(runtime, project, agent, path).await?
        }
        IdeCall::ChangeSets {}
        | IdeCall::ChangeDiff { .. }
        | IdeCall::BaseBlob { .. }
        | IdeCall::ConfigBlob { .. }
        | IdeCall::HeadBlob { .. }
        | IdeCall::Accept { .. }
        | IdeCall::Discard { .. }
        | IdeCall::Restore { .. }
        | IdeCall::Comment { .. } => change_call(runtime, project, call).await?,
        IdeCall::RevealAck { request } => reveal::acked(runtime, project, &request)?,
        IdeCall::RunAck { request } => run_command::acked(runtime, project, &request)?,
        IdeCall::Git { args, folder } => git_call(runtime, project, args, folder).await?,
        IdeCall::Events {} | IdeCall::Session {} | IdeCall::Terminal { .. } => {
            return Err(failure("ide_request_invalid"));
        }
    };
    Ok(IdeReply::Ok { data })
}

/// Answers a Changes call or a diff comment where the engine lands change sets; elsewhere
/// it is unavailable. Inputs: the owner runtime, the project and the call. Output: the
/// reply data.
async fn change_call(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
    call: IdeCall,
) -> Result<serde_json::Value, EngineError> {
    #[cfg(unix)]
    {
        let root = runtime.ide()?.root;
        match call {
            IdeCall::Comment { .. } => {
                comments::answer(root, runtime.terminal().ok(), project.clone(), call).await
            }
            call => changes::answer(root, project.clone(), call).await,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (runtime, project, call);
        Err(failure("ide_request_unavailable"))
    }
}

/// Runs one git network command of the IDE in the project folder outside the IDE sandbox
/// ([`remote_git::answer`]) once it has a place among the running ones
/// ([`remote_git::place`]); where the engine has no IDE sandbox it is unavailable. The shim's
/// [`remote_git::index::VERB`] call carries staged changes instead, with places of its own, so
/// a long push cannot fail a stage.
/// Inputs: the owner runtime, the project, git's arguments and the folder git ran in.
/// Output: the reply data, or `ide_git_busy` when the places are taken.
async fn git_call(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
    args: Vec<String>,
    folder: String,
) -> Result<serde_json::Value, EngineError> {
    #[cfg(unix)]
    {
        let root = runtime.ide()?.root;
        if args.first().map(String::as_str) == Some(remote_git::index::VERB) {
            let grants = runtime.ide_grants();
            let place = remote_git::place(&grants.index_calls, project)?;
            let gate = remote_git::index::gate(&grants.index_gates, project)?;
            return remote_git::index::answer(root, project.clone(), args, folder, place, gate)
                .await;
        }
        let place = remote_git::place(&runtime.ide_grants().git_calls, project)?;
        remote_git::answer(root, project.clone(), args, folder, place).await
    }
    #[cfg(not(unix))]
    {
        let _ = (runtime, project, args, folder);
        Err(failure("ide_request_unavailable"))
    }
}

/// The engine's index step for a row that plays the relay. Inputs: the private folder, the
/// engine's folder, the project folder and the slot. Output: the exit code the shim reads.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn carry_index(private: &Path, engine: &Path, project: &Path, slot: &str) -> i32 {
    remote_git::index::carry(&Mutex::new(()), private, engine, project, slot)
        .unwrap()
        .code
}

/// The first argument of the shim's index call.
pub(super) const INDEX_VERB: &str = "crazyagent-index";

/// Writes the project's generated workspace file on a blocking thread, setting the
/// show-all switch first when given. Inputs: the owner runtime, project and switch.
/// Output: `{path, hidden, show_all}`, `hidden` counting the agents the cap left out.
async fn workspace(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
    show_all: Option<bool>,
) -> Result<serde_json::Value, EngineError> {
    let root = runtime.ide()?.root;
    let project = project.clone();
    let written = tokio::task::spawn_blocking(move || match show_all {
        Some(on) => super::workspace::show_all(&root, &project, on),
        None => super::workspace::write(&root, &project),
    })
    .await
    .map_err(|_| failure("ide_workspace_unavailable"))??;
    let path = written
        .path
        .to_str()
        .ok_or_else(|| failure("ide_path_unreadable"))?;
    Ok(serde_json::json!({
        "path": path,
        "hidden": written.hidden,
        "show_all": written.show_all,
    }))
}

/// Streams the project's events to a subscribed connection: `ok` first, then one frame
/// per event, until the client sends anything or closes, the capability is revoked
/// (`ide_capability_revoked`) or the client falls behind (`ide_events_lagged`).
/// Inputs: the stream, handshake packet and owner runtime. Output: none.
async fn events(stream: &mut UnixStream, packet: &[u8; 37], runtime: &OwnerRuntimeHandle) {
    let mut events = match runtime.ide_grants().subscribe(packet) {
        Ok(events) => events,
        Err(error) => {
            let _ = write(stream, &refusal(&error)).await;
            return;
        }
    };
    let (mut reader, mut writer) = stream.split();
    let subscribed = IdeReply::Ok {
        data: serde_json::Value::Null,
    };
    if write(&mut writer, &subscribed).await.is_err() {
        return;
    }
    loop {
        let reply = tokio::select! {
            received = events.recv() => match received {
                Ok(event) => IdeReply::Event { event },
                Err(broadcast::error::RecvError::Lagged(_)) => refusal(&failure("ide_events_lagged")),
                Err(broadcast::error::RecvError::Closed) => refusal(&failure("ide_capability_revoked")),
            },
            _ = reader.read_u8() => return,
        };
        let last = !matches!(reply, IdeReply::Event { .. });
        if write(&mut writer, &reply).await.is_err() || last {
            return;
        }
    }
}

/// The refusal reply for an error, carrying its code only when it is a plain code.
fn refusal(error: &EngineError) -> IdeReply {
    IdeReply::Rejected {
        code: plain_code(&error.0).to_owned(),
    }
}

/// Writes one reply frame within `WRITE_TIME`; a reply too large for a frame goes out
/// as `ide_reply_bounds`. Inputs: the writer and reply. Output: Ok once written.
async fn write(
    stream: &mut (impl AsyncWrite + Unpin),
    reply: &IdeReply,
) -> Result<(), EngineError> {
    let frame = match encode_frame(reply) {
        Ok(frame) => frame,
        Err(_) => encode_frame(&refusal(&failure("ide_reply_bounds")))
            .map_err(|_| failure("ide_reply_bounds"))?,
    };
    tokio::time::timeout(WRITE_TIME, stream.write_all(&frame))
        .await
        .map_err(|_| failure("ide_reader_slow"))?
        .map_err(|_| failure("ide_connection_closed"))
}

/// Lowercase hexadecimal text of some bytes.
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Makes sure a folder exists as a private directory of this user, creating it 0700.
/// Input: the path. Output: Ok, or `ide_folder_unavailable` or `ide_folder_unsafe`.
#[cfg(unix)]
pub(super) fn private_folder(path: &Path) -> Result<(), EngineError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(failure("ide_folder_unavailable")),
    }
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| failure("ide_folder_unavailable"))?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(failure("ide_folder_unsafe"));
    }
    Ok(())
}

/// Refuses IDE folders where the engine has no Unix owner and mode checks.
/// Takes the path, unused. Returns `ide_unavailable`.
#[cfg(not(unix))]
pub(super) fn private_folder(_path: &Path) -> Result<(), EngineError> {
    Err(failure("ide_unavailable"))
}

/// Writes a capability file: removes any earlier file at the path, creates a new one
/// exclusively, sets it 0600 and writes the token as 64 hex digits. Inputs: the path and
/// token. Output: the new file's device and inode, which `revoke` checks before removing.
#[cfg(unix)]
fn write_capability(path: &Path, token: &[u8; 32]) -> Result<(u64, u64), EngineError> {
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let unavailable = |_| failure("ide_capability_unavailable");
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(failure("ide_capability_unavailable")),
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(unavailable)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(unavailable)?;
    file.write_all(hex(token).as_bytes()).map_err(unavailable)?;
    file.sync_all().map_err(unavailable)?;
    let metadata = file.metadata().map_err(unavailable)?;
    assert!(metadata.is_file());
    assert_eq!(metadata.len(), 64);
    assert_eq!(metadata.mode() & 0o7777, 0o600);
    Ok((metadata.dev(), metadata.ino()))
}

/// Refuses capability files where the engine has no Unix mode checks.
/// Takes the path and token, unused. Returns `ide_unavailable`.
#[cfg(not(unix))]
fn write_capability(_path: &Path, _token: &[u8; 32]) -> Result<(u64, u64), EngineError> {
    Err(failure("ide_unavailable"))
}

/// Removes a capability file while it is still the file the engine wrote; a missing file
/// is Ok and a replaced one is kept. Inputs: the path and its device and inode.
/// Output: Ok, or `ide_capability_replaced_preserved` or `ide_capability_cleanup_failed`.
#[cfg(unix)]
fn remove_capability(path: &Path, identity: (u64, u64)) -> Result<(), EngineError> {
    use std::os::unix::fs::MetadataExt;
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(failure("ide_capability_cleanup_failed")),
        Ok(metadata) if metadata.is_file() && (metadata.dev(), metadata.ino()) == identity => {
            std::fs::remove_file(path).map_err(|_| failure("ide_capability_cleanup_failed"))
        }
        Ok(_) => Err(failure("ide_capability_replaced_preserved")),
    }
}

/// Has nothing to remove where no capability file can be written.
/// Takes the path and identity, unused. Returns Ok.
#[cfg(not(unix))]
fn remove_capability(_path: &Path, _identity: (u64, u64)) -> Result<(), EngineError> {
    Ok(())
}

#[cfg(unix)]
mod changes;
#[cfg(unix)]
mod comments;
#[cfg(all(test, unix))]
pub(crate) mod engine_step;
mod extensions;
#[cfg(unix)]
mod remote_git;
#[cfg(unix)]
mod terminal;
#[cfg(all(test, unix))]
mod tests;
