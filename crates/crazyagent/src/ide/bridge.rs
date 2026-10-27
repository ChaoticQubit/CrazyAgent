use crate::engine::EngineError;
use crate::engine::owner::transport::changes::CHANGES_TIMEOUT;
use crate::engine::owner::{OwnerRuntimeHandle, failure};
use crate::landing::ChangeSetId;
use crate::platform::unix_socket::UnixStream;
use crate::protocol::{ProjectId, RequestId, encode_frame, plain_code};
use crate::terminal::AgentId;
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

/// The built-in extension's files by path inside its folder: its manifest and the
/// JavaScript the repository's tsc compiled from `apps/ide-extension/src`.
pub const EXTENSION_FILES: [(&str, &[u8]); 15] = [
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
];

const GRANTS_MAX: usize = 64;
const SUBSCRIBERS_MAX: usize = 4;
const EVENTS_QUEUED: usize = 64;
const WRITE_TIME: Duration = Duration::from_secs(10);
/// How long a Changes read may run: under the IDE extension's 30 s request timer, so a read
/// the extension gave up on never holds one of the `CONNECTIONS_MAX` connections into its
/// next poll, and the extension reads the refusal instead of closing first.
const READ_TIME: Duration = Duration::from_secs(28);
const PATH_BYTES_MAX: usize = 4096;
const TEXT_BYTES_MAX: usize = 16384;
/// The most bytes one diff comment may hold, so with its file and line it fits one typed
/// agent prompt and one team conversation message.
pub(super) const COMMENT_BYTES_MAX: usize = 2048;
const NAME_BYTES_MAX: usize = 128;
const VERSION_BYTES_MAX: usize = 64;
const CAPABILITY: &str = "capability";

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
    ChangeDiff { id: ChangeSetId, page: u16 },
    #[serde(rename = "ide.base_blob")]
    BaseBlob {
        id: ChangeSetId,
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
    #[serde(rename = "ide.events")]
    Events {},
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

/// An event the engine pushes to a project's IDE: something to refresh, or a file the
/// owner asked to reveal from the TUI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "name", rename_all = "snake_case", deny_unknown_fields)]
pub enum IdeEvent {
    WorkspaceChanged,
    ChangeSetsChanged,
    AgentsChanged,
    MemoriesChanged,
    Reveal { request: RequestId, path: String },
}

impl IdeRequest {
    /// Checks every argument's bounds before any handler runs: paths are relative, at most
    /// `PATH_BYTES_MAX` bytes and free of NUL and `..`; texts fit `TEXT_BYTES_MAX`, a comment
    /// fits `COMMENT_BYTES_MAX` and is not blank on a line from 1; extension ids are
    /// `publisher.name`.
    /// Input: the request. Output: Ok, or `ide_request_invalid`.
    pub fn validate(&self) -> Result<(), EngineError> {
        let valid = match &self.call {
            IdeCall::BaseBlob { path, .. } | IdeCall::OwnerEdit { path, .. } => relative(path),
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
            IdeCall::ExtensionRequest { id, version }
            | IdeCall::ExtensionDecide { id, version, .. } => {
                extension_id(id) && extension_version(version)
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
            | IdeCall::RevealAck { .. }
            | IdeCall::Events {} => true,
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

/// Whether an extension id is `publisher.name`, each part 1 to `NAME_BYTES_MAX` ASCII
/// letters, digits, hyphens or underscores.
fn extension_id(id: &str) -> bool {
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
fn extension_version(version: &str) -> bool {
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
/// `GRANTS_MAX` in all. Each holds its token, its file and the sender of its project's
/// events; dropping a grant kills its token and ends its subscriptions.
#[derive(Default)]
pub(crate) struct Grants {
    live: Mutex<Vec<Grant>>,
}

/// One project's capability for one IDE server launch.
struct Grant {
    project: ProjectId,
    token: [u8; 32],
    file: PathBuf,
    identity: (u64, u64),
    events: broadcast::Sender<IdeEvent>,
}

impl Grants {
    /// Issues a new capability for a project, replacing the project's earlier one: a fresh
    /// 32-byte token written as 64 hex digits to a new 0600 file in a 0700 project folder.
    /// Inputs: the engine root and the project. Output: the file path, or the refusal.
    fn issue(&self, root: &Path, project: &ProjectId) -> Result<CapabilityPath, EngineError> {
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
            file: file.clone(),
            identity,
            events,
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
    /// with every live token. Input: the handshake packet. Output: the project its
    /// capability names, or `ide_capability_refused`.
    pub(crate) fn admit(&self, packet: &[u8; 37]) -> Result<ProjectId, EngineError> {
        self.find(packet, |grant| Ok(grant.project.clone()))
    }

    /// Subscribes a handshake's capability to its project's events, at most
    /// `SUBSCRIBERS_MAX` at once. Input: the handshake packet. Output: the receiver, or
    /// `ide_capability_refused` or `ide_events_busy`.
    fn subscribe(&self, packet: &[u8; 37]) -> Result<broadcast::Receiver<IdeEvent>, EngineError> {
        self.find(packet, |grant| {
            if grant.events.receiver_count() >= SUBSCRIBERS_MAX {
                return Err(failure("ide_events_busy"));
            }
            let receiver = grant.events.subscribe();
            assert!(grant.events.receiver_count() <= SUBSCRIBERS_MAX);
            Ok(receiver)
        })
    }

    /// Runs `pick` on the live grant a handshake's token matches, under the grants lock.
    /// Inputs: the packet and the pick. Output: its result, or `ide_capability_refused`.
    fn find<T>(
        &self,
        packet: &[u8; 37],
        pick: impl FnOnce(&Grant) -> Result<T, EngineError>,
    ) -> Result<T, EngineError> {
        if &packet[..4] != MAGIC || packet[4] != 0 {
            return Err(failure("ide_capability_refused"));
        }
        let live = self.live.lock().map_err(|_| failure("ide_unavailable"))?;
        assert!(live.len() <= GRANTS_MAX);
        let mut found = None;
        for (index, grant) in live.iter().enumerate() {
            if bool::from(grant.token.ct_eq(&packet[5..])) {
                found = Some(index);
            }
        }
        match found {
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
        grants.iter().fold(Ok(()), |result, grant| {
            result.and(remove_capability(&grant.file, grant.identity))
        })
    }
}

/// Issues the capability one IDE server launch hands its extension: it allows only the
/// IDE calls, only for this project, until `revoke` or the owner server stops.
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
/// leaving an identical install untouched and replacing any other one whole.
/// Input: the extensions folder. Output: the extension's folder.
pub fn install_extension(extensions: &Path) -> Result<PathBuf, EngineError> {
    let folder = extensions.join(EXTENSION);
    let installed = EXTENSION_FILES
        .iter()
        .all(|(name, bytes)| std::fs::read(folder.join(name)).is_ok_and(|found| found == *bytes));
    if installed {
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
    Ok(folder)
}

/// Serves one IDE capability connection after its handshake: admits the token, reads one
/// request for the capability's own project and answers it, or turns the connection into
/// the project's event subscription. The handshake deadline bounds the request and every
/// call but the subscription and the Changes calls, which run git under `bound`.
/// Accept, Discard or Restore past it is `changes_outcome_unknown`, and what one did is
/// reported even when its capability was revoked while it ran.
/// Inputs: the stream, handshake packet, owner runtime and deadline.
pub(crate) async fn serve(
    mut stream: UnixStream,
    packet: [u8; 37],
    runtime: OwnerRuntimeHandle,
    deadline: Instant,
) {
    let opened = tokio::time::timeout_at(deadline, open(&mut stream, &packet, &runtime)).await;
    let (reply, wrote) = match opened {
        Err(_) => (None, false),
        Ok(Err(error)) => (Some(refusal(&error)), false),
        Ok(Ok((_, IdeCall::Events {}))) => {
            events(&mut stream, &packet, &runtime).await;
            (None, false)
        }
        Ok(Ok((project, call))) => {
            let writes = matches!(
                call,
                IdeCall::Accept { .. } | IdeCall::Discard { .. } | IdeCall::Restore { .. }
            );
            let bound = bound(&call, deadline);
            let answered = tokio::time::timeout_at(bound, answer(&runtime, &project, call)).await;
            let reply = match answered {
                Ok(Ok(reply)) => reply,
                Ok(Err(error)) => refusal(&error),
                Err(_) if writes => refusal(&failure("changes_outcome_unknown")),
                Err(_) => refusal(&failure("ide_request_timeout")),
            };
            (Some(reply), writes)
        }
    };
    if let Some(reply) = reply {
        let reply = match runtime.ide_grants().admit(&packet) {
            Err(error) if !wrote => refusal(&error),
            _ => reply,
        };
        let _ = write(&mut stream, &reply).await;
    }
    let _ = stream.shutdown().await;
}

/// When one IDE call must be answered by. Accept, Discard and Restore run under the
/// desktop's Changes bound, so a slow commit still lands and reports what it did; the
/// three Changes reads run under READ_TIME, since the extension polls them again; every
/// other call keeps the handshake deadline. Inputs: the call and the handshake deadline.
/// Output: the instant, never before the deadline.
fn bound(call: &IdeCall, deadline: Instant) -> Instant {
    let time = match call {
        IdeCall::Accept { .. } | IdeCall::Discard { .. } | IdeCall::Restore { .. } => {
            CHANGES_TIMEOUT
        }
        IdeCall::ChangeSets {} | IdeCall::ChangeDiff { .. } | IdeCall::BaseBlob { .. } => READ_TIME,
        _ => return deadline,
    };
    deadline.max(Instant::now() + time)
}

/// Reads the one request frame, then admits the handshake, so a refusal never leaves
/// unread bytes that could reset the connection before the client reads it. Inputs: the
/// stream, packet and owner runtime. Output: the capability's project and the call, or
/// `ide_capability_refused`, `ide_request_invalid` or `ide_project_refused`.
async fn open(
    stream: &mut UnixStream,
    packet: &[u8; 37],
    runtime: &OwnerRuntimeHandle,
) -> Result<(ProjectId, IdeCall), EngineError> {
    let frame = crate::engine::read_frame(stream)
        .await
        .map_err(|_| failure("ide_request_invalid"))?;
    let project = runtime.ide_grants().admit(packet)?;
    let request: IdeRequest =
        serde_json::from_slice(&frame[4..]).map_err(|_| failure("ide_request_invalid"))?;
    request.validate()?;
    if request.project != project {
        return Err(failure("ide_project_refused"));
    }
    Ok((project, request.call))
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
        | IdeCall::Accept { .. }
        | IdeCall::Discard { .. }
        | IdeCall::Restore { .. }
        | IdeCall::Comment { .. } => change_call(runtime, project, call).await?,
        IdeCall::ExtensionRequest { .. }
        | IdeCall::ExtensionDecide { .. }
        | IdeCall::RevealAck { .. } => return Err(failure("ide_request_unavailable")),
        IdeCall::Events {} => return Err(failure("ide_request_invalid")),
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
fn hex(bytes: &[u8]) -> String {
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
mod tests;
