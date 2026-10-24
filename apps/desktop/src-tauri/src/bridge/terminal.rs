use super::team::read_failure;
use super::{BridgeError, NativeBridge, bounded, failure};
use crazyagent::engine::owner::{OwnerData, OwnerRead};
use crazyagent::protocol::ProjectId;
use crazyagent::team::TeamGraph;
use crazyagent::terminal::{
    AgentId, INPUT_BYTES_MAX, SIZE_MAX, SIZE_MIN, TerminalConnection, TerminalFrame,
    TerminalReader, TerminalRequest, TerminalWriter,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use tauri::ipc::{Channel, InvokeResponseBody};
use tokio::sync::mpsc;

pub const VIEWERS_MAX: usize = 4;
const REQUESTS_MAX: usize = 64;
const CODE_BYTES_MAX: usize = 64;

/// Checks a terminal name the window sent. Takes the text.
/// Returns the engine's terminal id, or `terminal_agent_invalid`.
pub fn agent(text: &str) -> Result<AgentId, BridgeError> {
    AgentId::new(text).map_err(|_| failure("terminal_agent_invalid"))
}

fn attach_failure(code: &str) -> BridgeError {
    failure(match code {
        "terminal_unavailable" => "terminal_unavailable",
        "terminal_agent_unknown" => "terminal_agent_unknown",
        "terminal_agent_invalid" => "terminal_agent_invalid",
        "terminal_viewers_full" => "terminal_viewers_full",
        _ => "terminal_attach_failed",
    })
}

/// Keeps an engine code only when it is plain lowercase words joined by underscores.
/// Takes the code and the fallback for anything else.
/// Returns text that is safe to show in the window.
pub fn plain_code(code: &str, fallback: &'static str) -> String {
    let plain = !code.is_empty()
        && code.len() <= CODE_BYTES_MAX
        && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
    if plain {
        code.to_owned()
    } else {
        fallback.to_owned()
    }
}

/// Turns one engine terminal frame into the channel message the window reads.
/// Takes the attached agent and the frame.
/// Returns the message and whether it is the last one of the stream.
pub fn channel_message(agent: &AgentId, frame: TerminalFrame) -> (InvokeResponseBody, bool) {
    let (frame, last) = match frame {
        TerminalFrame::Output { bytes } => return (InvokeResponseBody::Raw(bytes), false),
        TerminalFrame::Attached {
            agent_id,
            cols,
            rows,
        } if &agent_id == agent => (
            TerminalFrame::Attached {
                agent_id,
                cols,
                rows,
            },
            false,
        ),
        TerminalFrame::Attached { .. } => (closed("terminal_agent_mismatch"), true),
        TerminalFrame::Closed { reason } => (closed(&plain_code(&reason, "terminal_closed")), true),
        TerminalFrame::Rejected { code } => (
            TerminalFrame::Rejected {
                code: plain_code(&code, "terminal_attach_failed"),
            },
            true,
        ),
    };
    (json(&frame), last)
}

fn closed(reason: &str) -> TerminalFrame {
    TerminalFrame::Closed {
        reason: reason.to_owned(),
    }
}

fn json(frame: &TerminalFrame) -> InvokeResponseBody {
    let text = serde_json::to_string(frame)
        .unwrap_or_else(|_| r#"{"kind":"closed","reason":"terminal_closed"}"#.to_owned());
    assert!(text.starts_with('{'));
    InvokeResponseBody::Json(text)
}

/// Checks one input or resize request from the window before it reaches the engine.
/// Takes the request; attach and detach never come through this path.
/// Returns whether the bytes or the size are inside the engine's limits.
pub fn request_valid(request: &TerminalRequest) -> bool {
    let size = SIZE_MIN..=SIZE_MAX;
    match request {
        TerminalRequest::Input { bytes } => !bytes.is_empty() && bytes.len() <= INPUT_BYTES_MAX,
        TerminalRequest::Resize { cols, rows } => size.contains(cols) && size.contains(rows),
        TerminalRequest::Attach { .. } | TerminalRequest::Detach => false,
    }
}

impl NativeBridge {
    /// Reads one project's team graph: master, harness groups, agents and task edges.
    /// Takes the project id.
    /// Returns the bounded graph of that project or a plain failure code.
    pub async fn team_graph(&mut self, id: &str) -> Result<TeamGraph, BridgeError> {
        let project_id = ProjectId::new(id).map_err(|_| failure("invalid_project"))?;
        let reply = self
            .controller()?
            .read(OwnerRead::TeamGraph {
                project_id: project_id.clone(),
            })
            .await
            .map_err(|e| read_failure(&e.0))?;
        let OwnerData::TeamGraph { graph } = reply.data else {
            return Err(failure("owner_read_failed"));
        };
        if graph.project_id != project_id {
            return Err(failure("owner_read_failed"));
        }
        bounded(*graph)
    }

    /// Opens one owner terminal connection attached to an agent's terminal.
    /// Takes the terminal name.
    /// Returns the connection, whose first frame is the attach or a rejection.
    pub async fn open_terminal(
        &mut self,
        agent: AgentId,
    ) -> Result<TerminalConnection, BridgeError> {
        self.controller()?
            .open_terminal(agent)
            .await
            .map_err(|e| attach_failure(&e.0))
    }
}

struct Viewer {
    serial: u64,
    requests: mpsc::Sender<TerminalRequest>,
}

#[derive(Default)]
struct Views {
    next: u64,
    viewers: HashMap<AgentId, Viewer>,
}

/// The window's live terminal views, at most one per agent and `VIEWERS_MAX` in all.
/// Typing and resizing go through here without waiting on the owner bridge lock.
/// A dropped view sends the engine a detach; the agent keeps running.
#[derive(Default)]
pub struct Terminals(Mutex<Views>);

impl Terminals {
    fn views(&self) -> Result<MutexGuard<'_, Views>, BridgeError> {
        self.0.lock().map_err(|_| failure("terminal_state_failed"))
    }

    /// Checks there is room for one more view before a connection opens.
    /// Takes the terminal name; a second view of the same agent replaces the first.
    /// Returns nothing, or `terminal_viewers_full`.
    pub fn room(&self, agent: &AgentId) -> Result<(), BridgeError> {
        let views = self.views()?;
        if views.viewers.len() >= VIEWERS_MAX && !views.viewers.contains_key(agent) {
            return Err(failure("terminal_viewers_full"));
        }
        Ok(())
    }

    /// Streams one opened terminal connection to the window's channel in order.
    /// Takes the terminal name, the connection and the channel.
    /// Returns once the reader and writer run; a full set of views is refused.
    pub fn attach(
        self: &Arc<Self>,
        agent: AgentId,
        connection: TerminalConnection,
        output: Channel<InvokeResponseBody>,
    ) -> Result<(), BridgeError> {
        let (requests, queue) = mpsc::channel(REQUESTS_MAX);
        let serial = {
            let mut views = self.views()?;
            if views.viewers.len() >= VIEWERS_MAX && !views.viewers.contains_key(&agent) {
                return Err(failure("terminal_viewers_full"));
            }
            views.next += 1;
            let serial = views.next;
            views
                .viewers
                .insert(agent.clone(), Viewer { serial, requests });
            assert!(views.viewers.len() <= VIEWERS_MAX);
            serial
        };
        let (reader, writer) = connection.into_split();
        tauri::async_runtime::spawn(write(writer, queue));
        tauri::async_runtime::spawn(read(Arc::clone(self), agent, serial, reader, output));
        Ok(())
    }

    /// Queues one input or resize request for an attached agent terminal.
    /// Takes the terminal name and the request.
    /// Returns once queued in order, or a plain code for a bad request or no view.
    pub async fn send(&self, agent: &AgentId, request: TerminalRequest) -> Result<(), BridgeError> {
        if !request_valid(&request) {
            return Err(failure("terminal_request_invalid"));
        }
        let requests = self
            .views()?
            .viewers
            .get(agent)
            .map(|viewer| viewer.requests.clone())
            .ok_or_else(|| failure("terminal_not_attached"))?;
        requests
            .send(request)
            .await
            .map_err(|_| failure("terminal_not_attached"))
    }

    /// Closes the window's view of one agent terminal; the agent keeps running.
    /// Takes the terminal name; an agent without a view is already detached.
    /// Returns nothing.
    pub fn detach(&self, agent: &AgentId) -> Result<(), BridgeError> {
        self.views()?.viewers.remove(agent);
        Ok(())
    }

    fn forget(&self, agent: &AgentId, serial: u64) {
        if let Ok(mut views) = self.0.lock()
            && views.viewers.get(agent).is_some_and(|v| v.serial == serial)
        {
            views.viewers.remove(agent);
        }
    }
}

async fn write(mut writer: TerminalWriter, mut queue: mpsc::Receiver<TerminalRequest>) {
    while let Some(request) = queue.recv().await {
        if writer.send(&request).await.is_err() {
            return;
        }
    }
    let _ = writer.send(&TerminalRequest::Detach).await;
}

async fn read(
    terminals: Arc<Terminals>,
    agent: AgentId,
    serial: u64,
    mut reader: TerminalReader,
    output: Channel<InvokeResponseBody>,
) {
    loop {
        let (message, last) = match reader.next().await {
            Ok(Some(frame)) => channel_message(&agent, frame),
            Ok(None) => (json(&closed("terminal_connection_closed")), true),
            Err(error) => (
                json(&closed(&plain_code(&error.0, "terminal_connection_lost"))),
                true,
            ),
        };
        if output.send(message).is_err() || last {
            break;
        }
    }
    terminals.forget(&agent, serial);
}
