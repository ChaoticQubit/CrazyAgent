use super::team::read_failure;
use super::{BridgeError, NativeBridge, bounded, failure};
use crazyagent::engine::owner::{TerminalControl, TerminalOutput, TerminalSession};
use crazyagent::protocol::ProjectId;
use crazyagent::team::TeamGraph;
use crazyagent::terminal::{
    AgentId, INPUT_BYTES_MAX, PROMPT_BYTES_MAX, SIZE_MAX, SIZE_MIN, TerminalFrame, TerminalRequest,
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
        "controller_disconnected" => "not_connected",
        "terminal_owner_unavailable" | "owner_socket_replaced" => "owner_unavailable",
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

/// Checks one input, prompt or resize request from the window before it reaches the engine.
/// Takes the request; attach and detach never come through this path.
/// Returns whether the bytes, the prompt or the size are inside the engine's limits.
pub fn request_valid(request: &TerminalRequest) -> bool {
    let size = SIZE_MIN..=SIZE_MAX;
    match request {
        TerminalRequest::Input { bytes } => !bytes.is_empty() && bytes.len() <= INPUT_BYTES_MAX,
        TerminalRequest::Prompt { text } => {
            !text.trim().is_empty() && text.len() <= PROMPT_BYTES_MAX
        }
        TerminalRequest::Resize { cols, rows } => size.contains(cols) && size.contains(rows),
        TerminalRequest::Attach { .. } | TerminalRequest::Detach => false,
    }
}

impl NativeBridge {
    /// Reads one project's team graph: master, harness groups, agents and task edges.
    /// Takes the project id.
    /// Returns the bounded graph of that project or a plain failure code.
    pub async fn team_graph(&self, id: &str) -> Result<TeamGraph, BridgeError> {
        let project_id = ProjectId::new(id).map_err(|_| failure("invalid_project"))?;
        let graph = self
            .reader()?
            .team_graph(project_id.clone())
            .await
            .map_err(|e| read_failure(&e.0))?;
        if graph.project_id != project_id {
            return Err(failure("owner_read_failed"));
        }
        bounded(graph)
    }

    /// Opens one owner terminal session attached to an agent's terminal.
    /// Takes the terminal name.
    /// Returns the session with the terminal size, its output and its controls.
    pub async fn open_terminal(&mut self, agent: AgentId) -> Result<TerminalSession, BridgeError> {
        let session = self
            .controller()?
            .terminal_attach(agent.clone())
            .await
            .map_err(|e| attach_failure(&e.0))?;
        if session.agent != agent {
            return Err(failure("terminal_agent_mismatch"));
        }
        Ok(session)
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
    opening: HashMap<AgentId, u64>,
}

impl Views {
    fn held(&self) -> usize {
        let opening = self
            .opening
            .keys()
            .filter(|agent| !self.viewers.contains_key(*agent))
            .count();
        self.viewers.len() + opening
    }

    fn admits(&self, agent: &AgentId) -> bool {
        self.viewers.contains_key(agent)
            || self.opening.contains_key(agent)
            || self.held() < VIEWERS_MAX
    }

    /// Makes one opened place the agent's view. A place older than a view of the same agent
    /// that opened after it never replaces that view, so a closed panel's late attach cannot
    /// take over the newer one. Takes the agent, the place's serial and its request queue.
    /// Returns unit, `terminal_view_replaced` or `terminal_viewers_full`.
    fn install(
        &mut self,
        agent: &AgentId,
        serial: u64,
        requests: mpsc::Sender<TerminalRequest>,
    ) -> Result<(), BridgeError> {
        let newer = self.opening.get(agent).is_some_and(|open| *open > serial)
            || self
                .viewers
                .get(agent)
                .is_some_and(|viewer| viewer.serial > serial);
        if newer {
            return Err(failure("terminal_view_replaced"));
        }
        if !self.admits(agent) {
            return Err(failure("terminal_viewers_full"));
        }
        if self.opening.get(agent) == Some(&serial) {
            self.opening.remove(agent);
        }
        self.viewers
            .insert(agent.clone(), Viewer { serial, requests });
        assert!(self.held() <= VIEWERS_MAX);
        Ok(())
    }
}

/// One view's place among the window's views, taken before its engine connection opens
/// so views still opening count toward `VIEWERS_MAX`. Attaching turns it into the view;
/// dropping it first gives the place back.
pub struct Reservation {
    terminals: Arc<Terminals>,
    agent: AgentId,
    serial: u64,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Ok(mut views) = self.terminals.0.lock()
            && views.opening.get(&self.agent) == Some(&self.serial)
        {
            views.opening.remove(&self.agent);
        }
    }
}

impl Reservation {
    /// Streams one opened terminal session to the window's channel in order: the attach
    /// frame with the size first, then the screen and live output.
    /// Takes the session of this place's agent and the channel.
    /// Returns the view's serial once the reader and writer run, which its detach names; a
    /// place lost to a full set of views or to a newer view of the agent is refused.
    pub fn attach(
        self,
        session: TerminalSession,
        output: Channel<InvokeResponseBody>,
    ) -> Result<u64, BridgeError> {
        let terminals = Arc::clone(&self.terminals);
        let (agent, serial) = (self.agent.clone(), self.serial);
        assert_eq!(session.agent, agent);
        let (requests, queue) = mpsc::channel(REQUESTS_MAX);
        terminals.views()?.install(&agent, serial, requests)?;
        let attached = TerminalFrame::Attached {
            agent_id: agent.clone(),
            cols: session.cols,
            rows: session.rows,
        };
        let (message, last) = channel_message(&agent, attached);
        assert!(!last);
        if output.send(message).is_err() {
            terminals.forget(&agent, serial);
            return Err(failure("terminal_window_closed"));
        }
        tauri::async_runtime::spawn(write(session.control, queue));
        tauri::async_runtime::spawn(read(terminals, agent, serial, session.output, output));
        Ok(serial)
    }
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

    /// Checks for room and takes a place for one more view in the same step, before its
    /// connection opens. Takes the terminal name; a second view of the same agent replaces
    /// the first. Returns the held place, or `terminal_viewers_full`.
    pub fn room(self: &Arc<Self>, agent: &AgentId) -> Result<Reservation, BridgeError> {
        let mut views = self.views()?;
        if !views.admits(agent) {
            return Err(failure("terminal_viewers_full"));
        }
        views.next += 1;
        let serial = views.next;
        views.opening.insert(agent.clone(), serial);
        assert!(views.held() <= VIEWERS_MAX);
        Ok(Reservation {
            terminals: Arc::clone(self),
            agent: agent.clone(),
            serial,
        })
    }

    /// Queues one input, prompt or resize request for an attached agent terminal.
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
    /// Takes the terminal name and the serial its attach returned; a view with another
    /// serial is a newer one and stays, and an agent without that view is already detached.
    /// Returns nothing.
    pub fn detach(&self, agent: &AgentId, serial: u64) -> Result<(), BridgeError> {
        let mut views = self.views()?;
        if views
            .viewers
            .get(agent)
            .is_some_and(|viewer| viewer.serial == serial)
        {
            views.viewers.remove(agent);
        }
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

async fn write(mut control: TerminalControl, mut queue: mpsc::Receiver<TerminalRequest>) {
    while let Some(request) = queue.recv().await {
        let sent = match request {
            TerminalRequest::Input { bytes } => control.input(bytes).await,
            TerminalRequest::Prompt { text } => control.prompt(text).await,
            TerminalRequest::Resize { cols, rows } => control.resize(cols, rows).await,
            TerminalRequest::Attach { .. } | TerminalRequest::Detach => continue,
        };
        if sent.is_err() {
            return;
        }
    }
    let _ = control.detach().await;
}

async fn read(
    terminals: Arc<Terminals>,
    agent: AgentId,
    serial: u64,
    mut stream: TerminalOutput,
    output: Channel<InvokeResponseBody>,
) {
    loop {
        let (message, last) = match stream.next().await {
            Ok(frame) => channel_message(&agent, frame),
            Err(error) if error.0 == "terminal_closed" => {
                (json(&closed("terminal_connection_closed")), true)
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(terminals: &Terminals, agent: &AgentId) -> Option<u64> {
        terminals
            .views()
            .unwrap()
            .viewers
            .get(agent)
            .map(|viewer| viewer.serial)
    }

    /// A panel closed during its first attach and opened again at once: the closed panel's
    /// late attach and detach never end the newer view of the same agent.
    #[test]
    fn a_late_attach_or_detach_of_a_closed_view_leaves_the_newer_view_of_the_agent_open() {
        let terminals = Arc::new(Terminals::default());
        let agent = AgentId::team(3).unwrap();
        let other = AgentId::team(4).unwrap();
        let old = terminals.room(&agent).unwrap();
        let new = terminals.room(&agent).unwrap();
        assert!(new.serial > old.serial);
        let (requests, _queue) = mpsc::channel(1);
        let late = terminals
            .views()
            .unwrap()
            .install(&agent, old.serial, requests.clone());
        assert_eq!(
            late.err().map(|e| e.code),
            Some("terminal_view_replaced"),
            "an older place never takes the agent while a newer one is opening"
        );
        terminals
            .views()
            .unwrap()
            .install(&agent, new.serial, requests.clone())
            .unwrap();
        let late = terminals
            .views()
            .unwrap()
            .install(&agent, old.serial, requests.clone());
        assert_eq!(late.err().map(|e| e.code), Some("terminal_view_replaced"));
        assert_eq!(shown(&terminals, &agent), Some(new.serial));
        terminals.detach(&agent, old.serial).unwrap();
        assert_eq!(
            shown(&terminals, &agent),
            Some(new.serial),
            "the closed view's detach leaves the newer view"
        );
        let elsewhere = terminals.room(&other).unwrap();
        terminals
            .views()
            .unwrap()
            .install(&other, elsewhere.serial, requests)
            .unwrap();
        terminals.detach(&other, new.serial).unwrap();
        assert_eq!(shown(&terminals, &other), Some(elsewhere.serial));
        terminals.detach(&agent, new.serial).unwrap();
        assert_eq!(shown(&terminals, &agent), None);
        terminals.detach(&agent, new.serial).unwrap();
    }
}
