use super::{IdeReply, hex, refusal};
use crate::engine::EngineError;
use crate::engine::owner::{OwnerRuntimeHandle, failure};
use crate::harnesses::interactive::InteractiveCommand;
use crate::platform::unix_socket::{OwnedWriteHalf, UnixStream};
use crate::protocol::{ProjectId, encode_frame};
use crate::sandbox::SandboxError;
use crate::sandbox::ide_shell::{self, Launch, TERMINAL_VARIABLES};
use crate::sandbox::policy::AgentAccess;
use crate::terminal::{
    AgentId, CHUNK_BYTES_MAX, Host, HostError, INPUT_BYTES_MAX, SIZE_MAX, SIZE_MIN, TerminalFrame,
    TerminalRequest, TerminalStream,
};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncWrite, AsyncWriteExt};

/// The most Sandboxed IDE terminals one owner server runs at once, across its projects,
/// so they never take every IDE connection the owner server serves.
pub(super) const TERMINALS_MAX: usize = 8;

const PREFIX: &str = "ide-";
const NAME_BYTES: usize = 8;
const GRANT_CHECK: Duration = Duration::from_secs(5);
const WRITE_TIME: Duration = Duration::from_secs(10);
const REQUESTS_QUEUED: usize = 16;

const _: () = assert!(TERMINALS_MAX + super::SUBSCRIBERS_MAX < super::CONNECTIONS_MAX);

/// The Sandboxed IDE terminals one owner server runs, each a pane of the engine's terminal
/// host named `ide-<16 hex digits>`, and whether the panes an earlier engine left were
/// ended, which happens once, before the first new one starts.
#[derive(Default)]
pub(super) struct Terminals {
    swept: bool,
    live: Vec<AgentId>,
}

/// Serves one Sandboxed IDE terminal: starts the owner's shell under the project's agent
/// sandbox in a new pane of the engine's terminal host, outside the IDE server's sandbox,
/// sends `ok` with its name and size, its screen, then its output, and types and resizes
/// what the extension sends, until the extension detaches, the shell ends or the
/// capability stops working. The pane ends with the connection. Inputs: the stream, the
/// handshake packet, the owner runtime, the project, the size and the level asked for.
pub(super) async fn serve(
    mut stream: UnixStream,
    packet: [u8; 37],
    runtime: OwnerRuntimeHandle,
    project: ProjectId,
    size: (u16, u16),
    level: AgentAccess,
) {
    let opened = open(&runtime, &project, size, level).await;
    let (host, registry, mut viewer) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            let _ = send(&mut stream, &refusal(&error)).await;
            let _ = stream.shutdown().await;
            return;
        }
    };
    let name = viewer.agent.clone();
    let (reader, mut writer) = stream.into_split();
    let reason = match greet(&mut writer, &viewer).await {
        Ok(()) => {
            relay(
                reader,
                &mut writer,
                &packet,
                &runtime,
                &project,
                &host,
                &mut viewer,
            )
            .await
        }
        Err(_) => "terminal_viewer_gone",
    };
    let ended = tokio::task::spawn_blocking(move || release(&registry, host.as_ref(), &name)).await;
    assert!(ended.is_ok());
    let closed = TerminalFrame::Closed {
        reason: reason.into(),
    };
    let _ = send(&mut writer, &closed).await;
    let _ = writer.shutdown().await;
}

/// Builds the shell's sandboxed command, then claims a terminal slot and starts it, on a
/// blocking thread, so a system with no qualified sandbox refuses before the terminal host
/// is touched. Inputs: the owner runtime, the project, the size and the level asked for.
/// Output: the host, the registry the pane is recorded in and the attached viewer, or
/// `ide_terminal_unavailable`, `ide_terminal_refused`, `ide_terminals_busy` or the host's
/// code.
async fn open(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
    size: (u16, u16),
    level: AgentAccess,
) -> Result<(Arc<dyn Host>, Arc<Mutex<Terminals>>, TerminalStream), EngineError> {
    let root = runtime.ide()?.root;
    let settings = runtime.ide_servers().settings_files();
    let host = runtime.terminal()?;
    let registry = runtime.ide_grants().terminals.clone();
    let name = pane_name()?;
    let project = project.clone();
    let (started_host, started_registry) = (host.clone(), registry.clone());
    let started = tokio::task::spawn_blocking(move || {
        let command = prepare(&root, &project, level, settings.as_ref())?;
        claim(&started_registry, started_host.as_ref(), &name)?;
        let viewer = start(started_host.as_ref(), &name, &command, size);
        if viewer.is_err() {
            release(&started_registry, started_host.as_ref(), &name);
        }
        viewer
    })
    .await
    .map_err(|_| failure("ide_terminal_unavailable"))??;
    Ok((host, registry, started))
}

/// A fresh pane name: `ide-` and 16 random hex digits. Output: the name, or
/// `ide_entropy_unavailable`.
fn pane_name() -> Result<AgentId, EngineError> {
    let mut suffix = [0; NAME_BYTES];
    getrandom::fill(&mut suffix).map_err(|_| failure("ide_entropy_unavailable"))?;
    let name = AgentId::new(&format!("{PREFIX}{}", hex(&suffix)))?;
    assert!(name.as_str().starts_with(PREFIX));
    Ok(name)
}

/// Records a new terminal, at most `TERMINALS_MAX` at once; the first claim of an owner
/// server first ends every `ide-` pane an earlier engine left. Inputs: the registry, the
/// host and the new pane's name. Output: Ok, or `ide_terminals_busy` or the host's code.
fn claim(registry: &Mutex<Terminals>, host: &dyn Host, name: &AgentId) -> Result<(), EngineError> {
    let mut terminals = registry
        .lock()
        .map_err(|_| failure("ide_terminal_unavailable"))?;
    if !terminals.swept {
        for agent in host.alive()? {
            if agent.as_str().starts_with(PREFIX) && !terminals.live.contains(&agent) {
                ended(host.kill(&agent))?;
            }
        }
        terminals.swept = true;
    }
    if terminals.live.len() >= TERMINALS_MAX {
        return Err(failure("ide_terminals_busy"));
    }
    assert!(!terminals.live.contains(name));
    terminals.live.push(name.clone());
    assert!(terminals.live.len() <= TERMINALS_MAX);
    Ok(())
}

/// Forgets a terminal and ends its pane; a pane already gone is fine. Inputs: the
/// registry, the host and the pane's name.
fn release(registry: &Mutex<Terminals>, host: &dyn Host, name: &AgentId) {
    if let Ok(mut terminals) = registry.lock() {
        terminals.live.retain(|live| live != name);
    }
    let _ = ended(host.kill(name));
}

/// Treats a pane that is already gone as ended. Input: the host's answer. Output: Ok, or
/// the host's other code.
fn ended(killed: Result<(), HostError>) -> Result<(), EngineError> {
    match killed {
        Err(problem) if problem.0 != "terminal_agent_unknown" => Err(problem.into()),
        _ => Ok(()),
    }
}

/// The level a terminal the extension asked for runs at. The engine decides from the level it
/// read itself, since the extension's ask is untrusted input. Inputs: the project's level and
/// the level asked. Output: the level, or `ide_terminal_above_project` for a wider ask, or
/// `ide_terminal_level_refused` for Full, which only the IDE server's own shell runs.
pub(super) fn terminal_level(
    project: AgentAccess,
    asked: AgentAccess,
) -> Result<AgentAccess, &'static str> {
    if asked == AgentAccess::Full {
        return Err("ide_terminal_level_refused");
    }
    if asked.wider_than(project) {
        return Err("ide_terminal_above_project");
    }
    assert!(!asked.wider_than(project));
    assert_ne!(asked, AgentAccess::Full);
    Ok(asked)
}

/// Builds the pane command for the owner's login shell under the project's agent sandbox,
/// starting in the project folder. Inputs: the engine root, the project, the level asked for
/// and the engine's settings files. Output: the command, or
/// `ide_terminal_project_unavailable`, `ide_terminal_above_project`,
/// `ide_terminal_level_refused`, `ide_settings_unavailable`, `ide_terminal_shell_unknown`,
/// `ide_terminal_unavailable` or `ide_terminal_refused`.
pub(super) fn prepare(
    root: &Path,
    project: &ProjectId,
    asked: AgentAccess,
    settings: Option<&(PathBuf, PathBuf)>,
) -> Result<InteractiveCommand, EngineError> {
    let place = ide_shell::project_place(root, project)
        .map_err(|_| failure("ide_terminal_project_unavailable"))?;
    let level = terminal_level(place.access, asked).map_err(failure)?;
    let kept_ports = crate::ide::workspace::kept_ports(settings)?;
    let agent_files = crate::ide::workspace::agent_files_rule(
        root,
        project,
        &place.folder,
        place.access,
        place.agent_files,
    )?;
    let shell = ide_shell::owner_shell().map_err(|_| failure("ide_terminal_shell_unknown"))?;
    let incoming: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(variable, _)| {
            TERMINAL_VARIABLES
                .iter()
                .any(|known| variable.as_os_str() == *known)
        })
        .collect();
    let command = ide_shell::interactive(
        &Launch {
            root,
            project,
            folder: &place.folder,
            cwd: &place.folder,
            shell: &shell,
            arguments: &[],
            incoming: &incoming,
            extra_reads: &place.extra_reads,
            level,
            network: place.network,
            kept_ports: &kept_ports,
            agent_files,
        },
        place.access,
    )
    .map_err(|error| match error {
        SandboxError::Unavailable(_) => failure("ide_terminal_unavailable"),
        SandboxError::Invalid(_) | SandboxError::Io(_) => failure("ide_terminal_refused"),
    })?;
    assert_eq!(
        command.argv.first().map(String::as_str),
        Some(crate::sandbox::SANDBOX_EXEC)
    );
    Ok(command)
}

/// Starts a pane command in a new pane, sized and attached; a pane that cannot be sized or
/// attached is ended. Inputs: the host, the pane's name, the command and the size.
/// Output: the viewer, or the host's code.
fn start(
    host: &dyn Host,
    name: &AgentId,
    command: &InteractiveCommand,
    (cols, rows): (u16, u16),
) -> Result<TerminalStream, EngineError> {
    host.spawn(name, command)?;
    let attached = host
        .resize(name, cols, rows)
        .and_then(|()| host.attach(name));
    if attached.is_err() {
        let _ = ended(host.kill(name));
    }
    Ok(attached?)
}

/// Sends `ok` with the pane's name and size, then its scrollback and screen. Inputs: the
/// writer and the viewer. Output: Ok once written.
async fn greet(writer: &mut OwnedWriteHalf, viewer: &TerminalStream) -> Result<(), EngineError> {
    let opened = IdeReply::Ok {
        data: serde_json::json!({
            "terminal": viewer.agent.as_str(),
            "cols": viewer.cols,
            "rows": viewer.rows,
        }),
    };
    send(writer, &opened).await?;
    for chunk in viewer
        .history
        .chunks(CHUNK_BYTES_MAX)
        .chain(viewer.screen.chunks(CHUNK_BYTES_MAX))
    {
        let output = TerminalFrame::Output {
            bytes: chunk.to_vec(),
        };
        send(writer, &output).await?;
    }
    Ok(())
}

/// Streams the pane's output and applies the extension's input and resizes until one side
/// ends or the capability no longer admits this project, which each input and resize is
/// checked against first (CA-SEC-005); a revoked, replaced or ended grant stops it before
/// any further output, input or resize (V21-SEC-2-L1). Inputs: the read half, the writer,
/// the packet, the owner runtime, the project, the host and the viewer. Output: the plain
/// reason it ended.
async fn relay(
    mut reader: crate::platform::unix_socket::OwnedReadHalf,
    writer: &mut OwnedWriteHalf,
    packet: &[u8; 37],
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
    host: &Arc<dyn Host>,
    viewer: &mut TerminalStream,
) -> &'static str {
    let name = viewer.agent.clone();
    let (requests, mut asked) = tokio::sync::mpsc::channel(REQUESTS_QUEUED);
    let listener = tokio::spawn(async move {
        while let Ok(frame) = crate::engine::read_frame(&mut reader).await {
            let Ok(request) = serde_json::from_slice::<TerminalRequest>(&frame[4..]) else {
                break;
            };
            if requests.send(request).await.is_err() {
                break;
            }
        }
    });
    let admitted = || runtime.ide_grants().admit(packet).ok().as_ref() == Some(project);
    let Ok(mut ended) = runtime.ide_grants().watch(packet) else {
        listener.abort();
        return "ide_capability_revoked";
    };
    let mut check = tokio::time::interval(GRANT_CHECK);
    let reason = loop {
        tokio::select! {
            biased;
            _ = ended.changed() => break "ide_capability_revoked",
            request = asked.recv() => match request {
                None | Some(TerminalRequest::Detach) => break "terminal_detached",
                Some(
                    TerminalRequest::Attach { .. }
                    | TerminalRequest::AttachPane { .. }
                    | TerminalRequest::AttachSized { .. }
                    | TerminalRequest::Prompt { .. },
                ) => {
                    break "terminal_request_invalid";
                }
                Some(TerminalRequest::Input { bytes }) if bytes.is_empty() || bytes.len() > INPUT_BYTES_MAX => {
                    break "terminal_request_invalid";
                }
                Some(TerminalRequest::Resize { cols, rows })
                    if !(SIZE_MIN..=SIZE_MAX).contains(&cols) || !(SIZE_MIN..=SIZE_MAX).contains(&rows) =>
                {
                    break "terminal_request_invalid";
                }
                Some(request) => {
                    if !admitted() {
                        break "ide_capability_revoked";
                    }
                    if act(host, &name, request).await.is_err_and(|error| error.0 == "terminal_agent_unknown") {
                        break "terminal_ended";
                    }
                }
            },
            chunk = viewer.next() => match chunk {
                Some(bytes) => {
                    if send(writer, &TerminalFrame::Output { bytes }).await.is_err() {
                        break "terminal_viewer_gone";
                    }
                }
                None => break "terminal_ended",
            },
            _ = check.tick() => {
                if !admitted() {
                    break "ide_capability_revoked";
                }
            }
        }
    };
    listener.abort();
    reason
}

/// Types or resizes on a blocking thread. Inputs: the host, the pane and an `Input` or
/// `Resize` request. Output: the host's answer.
async fn act(
    host: &Arc<dyn Host>,
    name: &AgentId,
    request: TerminalRequest,
) -> Result<(), HostError> {
    let (host, name) = (host.clone(), name.clone());
    tokio::task::spawn_blocking(move || match request {
        TerminalRequest::Input { bytes } => host.input(&name, &bytes),
        TerminalRequest::Resize { cols, rows } => host.resize(&name, cols, rows),
        TerminalRequest::Attach { .. }
        | TerminalRequest::AttachPane { .. }
        | TerminalRequest::AttachSized { .. }
        | TerminalRequest::Prompt { .. }
        | TerminalRequest::Detach => Ok(()),
    })
    .await
    .unwrap_or_else(|_| Err(HostError("terminal_unavailable".into())))
}

/// Writes one frame within `WRITE_TIME`. Inputs: the writer and the frame. Output: Ok once
/// written, or `ide_reply_bounds`, `ide_reader_slow` or `ide_connection_closed`.
async fn send(
    stream: &mut (impl AsyncWrite + Unpin),
    frame: &impl serde::Serialize,
) -> Result<(), EngineError> {
    let bytes = encode_frame(frame).map_err(|_| failure("ide_reply_bounds"))?;
    tokio::time::timeout(WRITE_TIME, stream.write_all(&bytes))
        .await
        .map_err(|_| failure("ide_reader_slow"))?
        .map_err(|_| failure("ide_connection_closed"))
}
