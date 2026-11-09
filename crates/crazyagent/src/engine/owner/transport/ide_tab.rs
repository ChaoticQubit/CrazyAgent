use super::*;
use crate::ide::fetch::{self, BuildState};
use crate::protocol::encode_frame;
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = 512;

/// How long one IDE tab request may run: an open waits for the IDE server to start, which
/// the server register bounds at 90 seconds.
pub(super) const IDE_TAB_TIMEOUT: Duration = Duration::from_secs(120);

/// Every code an IDE tab request can answer an owner client with: this protocol's own, the
/// IDE server register's, workspace writer's, settings lock's and sandbox's, the extension
/// installs, the capability issue and the agents exclude line every open runs, and the
/// owner connection's it travels on.
pub const IDE_TAB_CODES: &[&str] = &[
    "git_exclude_invalid",
    "ide_agent_invalid",
    "ide_build_invalid",
    "ide_build_not_ready",
    "ide_capability_capacity",
    "ide_capability_unavailable",
    "ide_entropy_unavailable",
    "ide_extension_download_failed",
    "ide_extension_hash_invalid",
    "ide_extension_hash_mismatch",
    "ide_extension_hash_missing",
    "ide_extension_install_failed",
    "ide_extension_invalid",
    "ide_extension_list_invalid",
    "ide_extension_metadata_invalid",
    "ide_extension_not_found",
    "ide_extension_source_refused",
    "ide_extension_too_large",
    "ide_extensions_unavailable",
    "ide_folder_unavailable",
    "ide_folder_unsafe",
    "ide_path_unreadable",
    "ide_platform_unqualified",
    "ide_port_unavailable",
    "ide_project_folder_invalid",
    "ide_project_invalid",
    "ide_project_unknown",
    "ide_sandbox_invalid",
    "ide_server_check_failed",
    "ide_server_exited",
    "ide_server_not_loopback",
    "ide_server_port_taken",
    "ide_server_start_failed",
    "ide_server_start_timeout",
    "ide_server_stopped",
    "ide_servers_closed",
    "ide_servers_full",
    "ide_settings_invalid",
    "ide_settings_unwritable",
    "ide_state_unavailable",
    "ide_tab_outcome_unknown",
    "ide_tab_owner_unavailable",
    "ide_tab_reply_bounds",
    "ide_tab_request_bounds",
    "ide_tab_request_invalid",
    "ide_token_unavailable",
    "ide_unavailable",
    "ide_workspace_unavailable",
    "ide_workspace_unwritable",
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

/// Where the engine's IDE stands for the IDE tab: the build state, and the platform's
/// name when the IDE server cannot run on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdeTabState {
    pub build: BuildState,
    pub unqualified: Option<String>,
}

/// Where one project's IDE is reached: the workbench URL on 127.0.0.1, the port its
/// webview hosts answer on, the per-launch token file the desktop reads the session token
/// from, and the workspace file the workbench opens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdeTabServer {
    pub url: String,
    pub webview_port: u16,
    pub token_path: PathBuf,
    pub workspace: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    ide_tab: Action,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    State,
    Retry,
    Open { project: ProjectId },
    Touch { project: ProjectId },
    Stop { project: ProjectId },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    State { state: IdeTabState },
    Opened { server: IdeTabServer },
    Touched { running: bool },
    Stopped { stopped: bool },
    Rejected { code: String },
}

/// Names the requests that only read, for the log of replies this client could not read.
/// Input: the request. Output: its name, None for a request that changes something.
fn read_kind(action: &Action) -> Option<&'static str> {
    match action {
        Action::State => Some("ide_tab.state"),
        _ => None,
    }
}

fn unknown() -> EngineError {
    failure("ide_tab_outcome_unknown")
}

/// The platform's name when the engine's IDE server does not run on it, the rule the server
/// register applies when it opens one and the IDE build fetch applies before it downloads.
/// Output: the name, or none where the IDE is qualified.
fn unqualified() -> Option<String> {
    (!fetch::qualified()).then(|| std::env::consts::OS.to_owned())
}

impl OwnerClient {
    /// Reads where the engine's IDE build stands, and with `retry` first starts fetching
    /// it again unless it is ready or already being fetched. Input: whether to retry.
    /// Output: the state, or an `IDE_TAB_CODES` refusal.
    pub async fn ide_tab_state(&self, retry: bool) -> Result<IdeTabState, EngineError> {
        let action = if retry { Action::Retry } else { Action::State };
        match self.ide_tab_exchange(action).await? {
            Reply::State { state } => Ok(state),
            _ => Err(unknown()),
        }
    }

    /// Opens a project's IDE server from the ready build, starting it unless it runs, and
    /// writes the project's workspace. Input: the project. Output: where the IDE is
    /// reached, or an `IDE_TAB_CODES` refusal such as `ide_build_not_ready`.
    pub async fn ide_tab_open(&self, project: ProjectId) -> Result<IdeTabServer, EngineError> {
        match self.ide_tab_exchange(Action::Open { project }).await? {
            Reply::Opened { server } => Ok(server),
            _ => Err(unknown()),
        }
    }

    /// Tells the engine the owner still uses a project's IDE, so its server does not stop
    /// for idleness. Input: the project. Output: whether its server runs.
    pub async fn ide_tab_touch(&self, project: ProjectId) -> Result<bool, EngineError> {
        match self.ide_tab_exchange(Action::Touch { project }).await? {
            Reply::Touched { running } => Ok(running),
            _ => Err(unknown()),
        }
    }

    /// Stops a project's IDE server once its project page closes, after any open still
    /// starting it. Input: the project. Output: whether a server was running.
    pub async fn ide_tab_stop(&self, project: ProjectId) -> Result<bool, EngineError> {
        match self.ide_tab_exchange(Action::Stop { project }).await? {
            Reply::Stopped { stopped } => Ok(stopped),
            _ => Err(unknown()),
        }
    }

    async fn ide_tab_exchange(&self, action: Action) -> Result<Reply, EngineError> {
        tokio::time::timeout(self.timeout.max(IDE_TAB_TIMEOUT), async {
            let read = read_kind(&action);
            let frame = encode_frame(&Envelope { ide_tab: action })
                .map_err(|_| failure("ide_tab_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let unavailable = |_| failure("ide_tab_owner_unavailable");
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(unavailable)?;
            verify_peer(&stream)?;
            let mut packet = [0; 37];
            packet[..4].copy_from_slice(b"CAO1");
            packet[4] = match self.kind {
                OwnerClientKind::Tui => 0,
                OwnerClientKind::Desktop => 1,
            };
            packet[5..].copy_from_slice(&self.token);
            stream.write_all(&packet).await.map_err(unavailable)?;
            stream.write_all(&frame).await.map_err(unavailable)?;
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
        Err(error) => Reply::Rejected {
            code: public_code(error),
        },
    };
    let frame = match encode_frame(&reply) {
        Ok(frame) => frame,
        Err(_) => encode_frame(&Reply::Rejected {
            code: "ide_tab_reply_bounds".into(),
        })
        .map_err(|_| unknown())?,
    };
    stream.write_all(&frame).await.map_err(io_error)
}

fn public_code(error: EngineError) -> String {
    if IDE_TAB_CODES.contains(&error.0.as_str()) {
        error.0
    } else {
        "ide_tab_outcome_unknown".into()
    }
}

async fn handle(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<Reply, EngineError> {
    if bytes.len() > REQUEST_BYTES_MAX {
        return Err(failure("ide_tab_request_bounds"));
    }
    let Envelope { ide_tab: action } =
        serde_json::from_slice(bytes).map_err(|_| failure("ide_tab_request_invalid"))?;
    {
        let _barrier = tokio::time::timeout(engine.timeout, engine.owner_egress.write())
            .await
            .map_err(|_| failure("owner_busy"))?;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    let root = super::changes::engine_root(authority)?;
    match action {
        Action::State | Action::Retry => {
            let retry = matches!(action, Action::Retry);
            let build = tokio::task::spawn_blocking(move || {
                if retry {
                    fetch::ensure(&root);
                }
                fetch::state(&root)
            })
            .await
            .map_err(|_| unknown())?;
            Ok(Reply::State {
                state: IdeTabState {
                    build,
                    unqualified: unqualified(),
                },
            })
        }
        Action::Open { project } => open(authority, root, project).await,
        Action::Touch { project } => Ok(Reply::Touched {
            running: touch(authority, &project),
        }),
        Action::Stop { project } => Ok(Reply::Stopped {
            stopped: stop(authority, &project).await,
        }),
    }
}

#[cfg(unix)]
async fn open(
    authority: &Arc<Authority>,
    root: PathBuf,
    project: ProjectId,
) -> Result<Reply, EngineError> {
    if unqualified().is_some() {
        return Err(failure("ide_platform_unqualified"));
    }
    let (build, written, folder, hooks) = tokio::task::spawn_blocking({
        let project = project.clone();
        move || match fetch::state(&root) {
            BuildState::Ready { root: build, .. } => {
                let written = crate::ide::workspace::write(&root, &project)?;
                let folder = crate::ide::workspace::folder(&root, &project)?;
                crate::projects_home::exclude_agents(&folder)?;
                let hooks = crate::ide::workspace::project_hooks(&root, &project)?;
                Ok((build, written, folder, hooks))
            }
            _ => Err(failure("ide_build_not_ready")),
        }
    })
    .await
    .map_err(|_| unknown())??;
    let info = authority
        .ide_servers
        .open(&project, &folder, &build, hooks)
        .await?;
    if !info.url.starts_with("http://127.0.0.1:") {
        return Err(failure("ide_server_not_loopback"));
    }
    Ok(Reply::Opened {
        server: IdeTabServer {
            url: info.url,
            webview_port: info.port,
            token_path: info.token_path,
            workspace: written.path,
        },
    })
}

#[cfg(not(unix))]
async fn open(
    _authority: &Arc<Authority>,
    _root: PathBuf,
    _project: ProjectId,
) -> Result<Reply, EngineError> {
    Err(failure("ide_platform_unqualified"))
}

#[cfg(unix)]
fn touch(authority: &Authority, project: &ProjectId) -> bool {
    authority.ide_servers.touch(project)
}

#[cfg(not(unix))]
fn touch(_authority: &Authority, _project: &ProjectId) -> bool {
    false
}

#[cfg(unix)]
async fn stop(authority: &Authority, project: &ProjectId) -> bool {
    authority.ide_servers.stop(project).await
}

#[cfg(not(unix))]
async fn stop(_authority: &Authority, _project: &ProjectId) -> bool {
    false
}

#[cfg(all(test, unix))]
#[path = "ide_tab/tests.rs"]
mod tests;
