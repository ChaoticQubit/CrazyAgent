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

pub const VIEWERS_MAX: usize = 9;
pub const VIEWS_MAX: usize = crazyagent::terminal::DESKTOP_VIEWS_MAX;
pub const WINDOWS_MAX: usize = 8;
const WALL_PREFIX: &str = "terminals-";
const WALL_DIGEST_DIGITS: usize = 16;

const _: () = assert!(WINDOWS_MAX < 10);
const _: () = assert!(VIEWERS_MAX <= VIEWS_MAX);
const REQUESTS_MAX: usize = 64;
const CODE_BYTES_MAX: usize = 64;

/// Checks a terminal name the window sent. Takes the text.
/// Returns the engine's terminal id, or `terminal_agent_invalid`.
pub fn agent(text: &str) -> Result<AgentId, BridgeError> {
    AgentId::new(text).map_err(|_| failure("terminal_agent_invalid"))
}

/// Whether a window label names a window that may reach the terminal commands: the main
/// window or a terminal wall window, `terminals-` with 16 lowercase hex digits of its
/// project and its page number from 1 to `WINDOWS_MAX`. Takes the label. Returns the answer.
pub fn window_label_valid(label: &str) -> bool {
    if label == "main" {
        return true;
    }
    let Some((digest, page)) = label
        .strip_prefix(WALL_PREFIX)
        .and_then(|rest| rest.split_once('-'))
    else {
        return false;
    };
    digest.len() == WALL_DIGEST_DIGITS
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && page.len() == 1
        && page
            .parse::<usize>()
            .is_ok_and(|page| (1..=WINDOWS_MAX).contains(&page))
}

/// The label of one terminal wall window. Takes the project id and the page number.
/// Returns `terminals-<16 hex digits of the project>-<page>`.
pub fn wall_label(project: &ProjectId, page: usize) -> String {
    assert!((1..=WINDOWS_MAX).contains(&page));
    let label = format!("{}{page}", wall_prefix(project));
    assert!(window_label_valid(&label));
    label
}

/// The start every terminal wall window label of one project shares. Takes the project id.
/// Returns `terminals-<16 hex digits of the project>-`, which never carries the id's text.
pub fn wall_prefix(project: &ProjectId) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    project.as_str().hash(&mut hasher);
    format!("{WALL_PREFIX}{:016x}-", hasher.finish())
}

/// Whether a window may act on one project: the main window on any project, a terminal wall
/// window only on the project its label was made for. Takes the window label and the project.
/// Returns the answer.
pub fn window_admits(label: &str, project: &ProjectId) -> bool {
    label == "main" || (window_label_valid(label) && label.starts_with(&wall_prefix(project)))
}

/// The terminal wall windows to close among the open windows: every wall window, or only one
/// project's. Takes the open window labels and the project, if any. Returns those labels in
/// the order given.
pub fn walls_to_close<'a>(
    labels: impl IntoIterator<Item = &'a str>,
    project: Option<&ProjectId>,
) -> Vec<&'a str> {
    let prefix = project.map(wall_prefix);
    labels
        .into_iter()
        .filter(|label| *label != "main" && window_label_valid(label))
        .filter(|label| {
            prefix
                .as_ref()
                .is_none_or(|prefix| label.starts_with(prefix))
        })
        .collect()
}

const WALL_TITLE_SEPARATOR: &str = " \u{b7} Terminals ";

/// The project name a wall window's title shows, for the windows a growing wall opens.
/// Takes the title. Returns the name before " · Terminals ", or "Project" without one.
pub fn wall_title_name(title: &str) -> String {
    let name = title
        .rsplit_once(WALL_TITLE_SEPARATOR)
        .map_or("", |(name, _)| name.trim());
    if name.is_empty() {
        "Project".to_owned()
    } else {
        name.to_owned()
    }
}

/// Whether a project's team graph shows a terminal, so a wall window attaches only to its own
/// project's agents. Takes the graph and the terminal name. Returns the answer.
pub fn graph_shows(graph: &TeamGraph, agent: &AgentId) -> bool {
    graph
        .nodes
        .iter()
        .any(|node| node.terminal.as_ref() == Some(agent))
}

/// Checks a pane id the window sent for a native teammate's view, such as `%12`. Takes the
/// text. Returns the pane id, or `terminal_pane_invalid`.
pub fn pane(text: &str) -> Result<String, BridgeError> {
    crazyagent::terminal::pane_id_valid(text)
        .then(|| text.to_owned())
        .ok_or_else(|| failure("terminal_pane_invalid"))
}

fn attach_failure(code: &str) -> BridgeError {
    failure(match code {
        "terminal_unavailable" => "terminal_unavailable",
        "terminal_pane_unknown" => "terminal_pane_unknown",
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
        TerminalRequest::Attach { .. }
        | TerminalRequest::AttachPane { .. }
        | TerminalRequest::AttachSized { .. }
        | TerminalRequest::Detach => false,
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

    /// Opens one owner terminal session attached to an agent's terminal, or to one pane of
    /// its window, such as a native teammate's. Takes the terminal name and the checked pane.
    /// Returns the session with the terminal size, its output and its controls.
    pub async fn open_terminal(
        &mut self,
        agent: AgentId,
        pane: Option<String>,
    ) -> Result<TerminalSession, BridgeError> {
        let controller = self.controller()?;
        let session = match pane {
            Some(pane) => controller.terminal_attach_pane(agent.clone(), pane).await,
            None => controller.terminal_attach(agent.clone()).await,
        }
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
struct Window {
    viewers: HashMap<AgentId, Viewer>,
    opening: HashMap<AgentId, u64>,
    cleared: u64,
}

impl Window {
    fn held(&self) -> usize {
        let opening = self
            .opening
            .keys()
            .filter(|agent| !self.viewers.contains_key(*agent))
            .count();
        self.viewers.len() + opening
    }
}

#[derive(Default)]
struct Views {
    next: u64,
    windows: HashMap<String, Window>,
}

impl Views {
    #[cfg(test)]
    fn serial(&self, window: &str, agent: &AgentId) -> Option<u64> {
        let viewer = self.windows.get(window)?.viewers.get(agent)?;
        Some(viewer.serial)
    }

    fn held(&self) -> usize {
        self.windows.values().map(Window::held).sum()
    }

    fn window(&self, window: &str) -> Result<&Window, BridgeError> {
        self.windows
            .get(window)
            .ok_or_else(|| failure("terminal_window_closed"))
    }

    /// Whether a window has room for a view of the agent: one it already shows or is
    /// opening, or a free place in the window and in all windows together. Takes the window
    /// label and the agent. Returns unit, `terminal_window_closed` or `terminal_viewers_full`.
    fn admits(&self, window: &str, agent: &AgentId) -> Result<(), BridgeError> {
        let shown = self.window(window)?;
        let room = shown.viewers.contains_key(agent)
            || shown.opening.contains_key(agent)
            || (shown.held() < VIEWERS_MAX && self.held() < VIEWS_MAX);
        if room {
            Ok(())
        } else {
            Err(failure("terminal_viewers_full"))
        }
    }

    /// Makes one opened place the agent's view in its window. A place older than a view of
    /// the same agent in that window that opened after it never replaces that view, so a
    /// closed panel's late attach cannot take over the newer one, and a place taken before
    /// the window's last page load never lands. Takes the window label, the agent, the
    /// place's serial and its request queue. Returns unit, `terminal_window_closed`,
    /// `terminal_view_replaced` or `terminal_viewers_full`.
    fn install(
        &mut self,
        window: &str,
        agent: &AgentId,
        serial: u64,
        requests: mpsc::Sender<TerminalRequest>,
    ) -> Result<(), BridgeError> {
        let shown = self.window(window)?;
        let newer = serial <= shown.cleared
            || shown.opening.get(agent).is_some_and(|open| *open > serial)
            || shown
                .viewers
                .get(agent)
                .is_some_and(|viewer| viewer.serial > serial);
        if newer {
            return Err(failure("terminal_view_replaced"));
        }
        self.admits(window, agent)?;
        let shown = self
            .windows
            .get_mut(window)
            .ok_or_else(|| failure("terminal_window_closed"))?;
        if shown.opening.get(agent) == Some(&serial) {
            shown.opening.remove(agent);
        }
        shown
            .viewers
            .insert(agent.clone(), Viewer { serial, requests });
        assert!(shown.held() <= VIEWERS_MAX);
        assert!(self.held() <= VIEWS_MAX);
        Ok(())
    }
}

/// One view's place among its window's views, taken before its engine connection opens
/// so views still opening count toward `VIEWERS_MAX` and `VIEWS_MAX`. Attaching turns it
/// into the view; dropping it first gives the place back.
pub struct Reservation {
    terminals: Arc<Terminals>,
    window: String,
    agent: AgentId,
    serial: u64,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Ok(mut views) = self.terminals.0.lock()
            && let Some(shown) = views.windows.get_mut(&self.window)
            && shown.opening.get(&self.agent) == Some(&self.serial)
        {
            shown.opening.remove(&self.agent);
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
        terminals
            .views()?
            .install(&self.window, &agent, serial, requests)?;
        let attached = TerminalFrame::Attached {
            agent_id: agent.clone(),
            cols: session.cols,
            rows: session.rows,
        };
        let (message, last) = channel_message(&agent, attached);
        assert!(!last);
        if output.send(message).is_err() {
            terminals.forget(&self.window, &agent, serial);
            return Err(failure("terminal_window_closed"));
        }
        tauri::async_runtime::spawn(write(session.control, queue));
        tauri::async_runtime::spawn(read(
            terminals,
            self.window.clone(),
            agent,
            serial,
            session.output,
            output,
        ));
        Ok(serial)
    }
}

/// The live terminal views of every window that shows terminals: the main window and the
/// terminal wall windows, at most one view per agent in a window, `VIEWERS_MAX` in one
/// window and `VIEWS_MAX` in all. Typing and resizing go through here without waiting on
/// the owner bridge lock. A dropped view sends the engine a detach; the agent keeps running.
#[derive(Default)]
pub struct Terminals(Mutex<Views>);

impl Terminals {
    fn views(&self) -> Result<MutexGuard<'_, Views>, BridgeError> {
        self.0.lock().map_err(|_| failure("terminal_state_failed"))
    }

    /// Checks for room and takes a place for one more view in a window in the same step,
    /// before its connection opens. Takes the window label and the terminal name; a second
    /// view of the same agent in the window replaces the first. Returns the held place,
    /// `terminal_window_closed` or `terminal_viewers_full`.
    pub fn room(
        self: &Arc<Self>,
        window: &str,
        agent: &AgentId,
    ) -> Result<Reservation, BridgeError> {
        assert!(window_label_valid(window));
        let mut views = self.views()?;
        views.admits(window, agent)?;
        views.next += 1;
        let serial = views.next;
        views
            .windows
            .get_mut(window)
            .ok_or_else(|| failure("terminal_window_closed"))?
            .opening
            .insert(agent.clone(), serial);
        assert!(views.held() <= VIEWS_MAX);
        Ok(Reservation {
            terminals: Arc::clone(self),
            window: window.to_owned(),
            agent: agent.clone(),
            serial,
        })
    }

    /// Queues one input, prompt or resize request for an agent terminal a window shows.
    /// Takes the window label, the terminal name and the request.
    /// Returns once queued in order, or a plain code for a bad request or no view.
    pub async fn send(
        &self,
        window: &str,
        agent: &AgentId,
        request: TerminalRequest,
    ) -> Result<(), BridgeError> {
        if !request_valid(&request) {
            return Err(failure("terminal_request_invalid"));
        }
        let requests = self
            .views()?
            .windows
            .get(window)
            .and_then(|shown| shown.viewers.get(agent))
            .map(|viewer| viewer.requests.clone())
            .ok_or_else(|| failure("terminal_not_attached"))?;
        requests
            .send(request)
            .await
            .map_err(|_| failure("terminal_not_attached"))
    }

    /// Closes a window's view of one agent terminal; the agent keeps running.
    /// Takes the window label, the terminal name and the serial its attach returned; a view
    /// with another serial is a newer one and stays, and an agent without that view is
    /// already detached. Returns nothing.
    pub fn detach(&self, window: &str, agent: &AgentId, serial: u64) -> Result<(), BridgeError> {
        let mut views = self.views()?;
        if let Some(shown) = views.windows.get_mut(window)
            && shown
                .viewers
                .get(agent)
                .is_some_and(|viewer| viewer.serial == serial)
        {
            shown.viewers.remove(agent);
        }
        Ok(())
    }

    /// Ends every view of the page a window is leaving, as its first load, a reload or a new
    /// page load starts, and makes the window ready to show views: the old page can no
    /// longer read or detach its views, so they would hold the viewer slots for good.
    /// Dropping a view closes its request queue, which detaches it from the engine, and an
    /// attach the old page still had opening is refused when it lands. Other windows' views
    /// stay. Takes the window label. Returns nothing.
    pub fn page_started(&self, window: &str) {
        assert!(window_label_valid(window));
        if let Ok(mut views) = self.0.lock() {
            let next = views.next;
            let shown = views.windows.entry(window.to_owned()).or_default();
            shown.viewers.clear();
            shown.opening.clear();
            shown.cleared = next;
            assert_eq!(shown.held(), 0);
        }
    }

    /// Ends every view of a window that closed and forgets the window, so an attach it still
    /// had opening is refused when it lands and the window takes no new view. Takes the
    /// window label. Returns nothing.
    pub fn window_closed(&self, window: &str) {
        if let Ok(mut views) = self.0.lock() {
            views.windows.remove(window);
            assert!(!views.windows.contains_key(window));
        }
    }

    fn forget(&self, window: &str, agent: &AgentId, serial: u64) {
        if let Ok(mut views) = self.0.lock()
            && let Some(shown) = views.windows.get_mut(window)
            && shown.viewers.get(agent).is_some_and(|v| v.serial == serial)
        {
            shown.viewers.remove(agent);
        }
    }
}

async fn write(mut control: TerminalControl, mut queue: mpsc::Receiver<TerminalRequest>) {
    while let Some(request) = queue.recv().await {
        let sent = match request {
            TerminalRequest::Input { bytes } => control.input(bytes).await,
            TerminalRequest::Prompt { text } => control.prompt(text).await,
            TerminalRequest::Resize { cols, rows } => control.resize(cols, rows).await,
            TerminalRequest::Attach { .. }
            | TerminalRequest::AttachPane { .. }
            | TerminalRequest::AttachSized { .. }
            | TerminalRequest::Detach => continue,
        };
        if sent.is_err() {
            return;
        }
    }
    let _ = control.detach().await;
}

async fn read(
    terminals: Arc<Terminals>,
    window: String,
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
    terminals.forget(&window, &agent, serial);
}

#[cfg(test)]
mod tests {
    use super::*;

    const WALL: &str = "terminals-0123456789abcdef-1";

    fn shown(terminals: &Terminals, window: &str, agent: &AgentId) -> Option<u64> {
        terminals.views().unwrap().serial(window, agent)
    }

    fn team(index: usize) -> AgentId {
        AgentId::team(index as i64).unwrap()
    }

    fn code<T>(result: Result<T, BridgeError>) -> Option<&'static str> {
        result.err().map(|e| e.code)
    }

    /// A panel closed during its first attach and opened again at once: the closed panel's
    /// late attach and detach never end the newer view of the same agent.
    #[test]
    fn a_late_attach_or_detach_of_a_closed_view_leaves_the_newer_view_of_the_agent_open() {
        let terminals = Arc::new(Terminals::default());
        terminals.page_started("main");
        let agent = team(3);
        let other = team(4);
        let old = terminals.room("main", &agent).unwrap();
        let new = terminals.room("main", &agent).unwrap();
        assert!(new.serial > old.serial);
        let (requests, _queue) = mpsc::channel(1);
        let late = terminals
            .views()
            .unwrap()
            .install("main", &agent, old.serial, requests.clone());
        assert_eq!(
            code(late),
            Some("terminal_view_replaced"),
            "an older place never takes the agent while a newer one is opening"
        );
        terminals
            .views()
            .unwrap()
            .install("main", &agent, new.serial, requests.clone())
            .unwrap();
        let late = terminals
            .views()
            .unwrap()
            .install("main", &agent, old.serial, requests.clone());
        assert_eq!(code(late), Some("terminal_view_replaced"));
        assert_eq!(shown(&terminals, "main", &agent), Some(new.serial));
        terminals.detach("main", &agent, old.serial).unwrap();
        assert_eq!(
            shown(&terminals, "main", &agent),
            Some(new.serial),
            "the closed view's detach leaves the newer view"
        );
        let elsewhere = terminals.room("main", &other).unwrap();
        terminals
            .views()
            .unwrap()
            .install("main", &other, elsewhere.serial, requests)
            .unwrap();
        terminals.detach("main", &other, new.serial).unwrap();
        assert_eq!(shown(&terminals, "main", &other), Some(elsewhere.serial));
        terminals.detach("main", &agent, new.serial).unwrap();
        assert_eq!(shown(&terminals, "main", &agent), None);
        terminals.detach("main", &agent, new.serial).unwrap();
    }

    /// The main webview reloads while all but one of its views are open and the last is
    /// still opening: the new page gets every viewer slot back, the old views' request queues
    /// close so their writers detach from the engine, and the old page's late attach never
    /// takes a slot.
    #[test]
    fn a_page_load_frees_every_view_the_old_page_held() {
        let terminals = Arc::new(Terminals::default());
        terminals.page_started("main");
        let mut queues = Vec::new();
        for index in 1..VIEWERS_MAX {
            let agent = team(index);
            let place = terminals.room("main", &agent).unwrap();
            let (requests, queue) = mpsc::channel(1);
            terminals
                .views()
                .unwrap()
                .install("main", &agent, place.serial, requests)
                .unwrap();
            queues.push(queue);
        }
        let opening = team(VIEWERS_MAX);
        let late = terminals.room("main", &opening).unwrap();
        assert_eq!(
            code(terminals.room("main", &team(VIEWERS_MAX + 1))),
            Some("terminal_viewers_full"),
            "the old page holds every slot before the reload"
        );

        terminals.page_started("main");

        assert!(
            queues.iter().all(|queue| queue.is_closed()),
            "every old view's request queue is closed, so its writer detaches"
        );
        let (requests, _queue) = mpsc::channel(1);
        let landed =
            terminals
                .views()
                .unwrap()
                .install("main", &opening, late.serial, requests.clone());
        assert_eq!(
            code(landed),
            Some("terminal_view_replaced"),
            "the old page's attach that lands after the reload is refused"
        );
        assert_eq!(shown(&terminals, "main", &opening), None);
        drop(late);
        let fresh: Vec<_> = (VIEWERS_MAX + 1..=2 * VIEWERS_MAX)
            .map(|index| {
                let agent = team(index);
                terminals.room("main", &agent).map(|place| (agent, place))
            })
            .collect::<Result<_, _>>()
            .expect("the new page opens a full set of other agents");
        for (agent, place) in &fresh {
            terminals
                .views()
                .unwrap()
                .install("main", agent, place.serial, requests.clone())
                .unwrap();
            assert_eq!(shown(&terminals, "main", agent), Some(place.serial));
        }
        assert_eq!(terminals.views().unwrap().held(), VIEWERS_MAX);
    }

    /// The main window and a terminal wall window each hold their own view of one agent:
    /// neither replaces the other, and a wall window's reload or detach touches only its own.
    #[test]
    fn each_window_holds_its_own_view_of_an_agent_and_a_page_load_clears_only_its_window() {
        let terminals = Arc::new(Terminals::default());
        terminals.page_started("main");
        terminals.page_started(WALL);
        let agent = team(3);
        let (requests, _queue) = mpsc::channel(1);
        let main = terminals.room("main", &agent).unwrap();
        let tile = terminals.room(WALL, &agent).unwrap();
        let installed = [("main", main.serial), (WALL, tile.serial)].map(|(window, serial)| {
            code(
                terminals
                    .views()
                    .unwrap()
                    .install(window, &agent, serial, requests.clone()),
            )
        });
        assert_eq!(installed, [None, None], "both windows show the agent");
        assert_eq!(shown(&terminals, "main", &agent), Some(main.serial));
        assert_eq!(shown(&terminals, WALL, &agent), Some(tile.serial));
        terminals.detach(WALL, &agent, main.serial).unwrap();
        assert_eq!(
            shown(&terminals, WALL, &agent),
            Some(tile.serial),
            "a detach names its own window's view"
        );
        terminals.page_started(WALL);
        assert_eq!(
            shown(&terminals, "main", &agent),
            Some(main.serial),
            "a wall window's reload leaves the main window's view"
        );
        assert_eq!(shown(&terminals, WALL, &agent), None);
    }

    /// Views fill a window up to `VIEWERS_MAX` and all windows together up to `VIEWS_MAX`:
    /// the tenth view in one window and the 33rd over all windows are refused by name.
    #[test]
    fn a_window_holds_nine_views_and_all_windows_together_thirty_two() {
        let terminals = Arc::new(Terminals::default());
        let windows = [
            "main".to_owned(),
            wall_label(&ProjectId::new("larkspur").unwrap(), 1),
            wall_label(&ProjectId::new("larkspur").unwrap(), 2),
            wall_label(&ProjectId::new("larkspur").unwrap(), 3),
        ];
        for window in &windows {
            terminals.page_started(window);
        }
        let mut places = Vec::new();
        let mut next = 0;
        for window in &windows[..3] {
            for _ in 0..9 {
                next += 1;
                places.push(terminals.room(window, &team(next)));
            }
        }
        assert_eq!(
            places.iter().filter(|place| place.is_ok()).count(),
            27,
            "three windows take nine views each"
        );
        assert_eq!(
            code(terminals.room(&windows[0], &team(next + 1))),
            Some("terminal_viewers_full"),
            "a tenth view in one window is refused"
        );
        for _ in 27..32 {
            next += 1;
            places.push(terminals.room(&windows[3], &team(next)));
        }
        assert_eq!(places.iter().filter(|place| place.is_ok()).count(), 32);
        assert_eq!(
            code(terminals.room(&windows[3], &team(next + 1))),
            Some("terminal_viewers_full"),
            "the 33rd view over all windows is refused"
        );
        assert_eq!(terminals.views().unwrap().held(), VIEWS_MAX);
        assert_eq!((VIEWERS_MAX, VIEWS_MAX), (9, 32));
    }

    /// A wall window closes while one view is open and another is still opening: the open
    /// view's queue closes so it detaches, the late attach finds no window and is refused,
    /// and the gone window takes no new view.
    #[test]
    fn a_closed_window_frees_its_views_and_refuses_its_late_attach() {
        let terminals = Arc::new(Terminals::default());
        terminals.page_started(WALL);
        let (requests, queue) = mpsc::channel(1);
        let place = terminals.room(WALL, &team(1)).unwrap();
        terminals
            .views()
            .unwrap()
            .install(WALL, &team(1), place.serial, requests)
            .unwrap();
        let late = terminals.room(WALL, &team(2)).unwrap();
        let (requests, _late_queue) = mpsc::channel(1);

        terminals.window_closed(WALL);

        assert!(queue.is_closed(), "the closed window's view detaches");
        assert_eq!(
            code(
                terminals
                    .views()
                    .unwrap()
                    .install(WALL, &team(2), late.serial, requests)
            ),
            Some("terminal_window_closed")
        );
        assert_eq!(
            code(terminals.room(WALL, &team(1))),
            Some("terminal_window_closed"),
            "a window that is gone takes no new view"
        );
        assert_eq!(terminals.views().unwrap().held(), 0);
    }

    /// Only the main window and wall windows named the way `wall_label` names them reach the
    /// terminal commands; the label carries 16 hex digits of the project, never its text.
    #[test]
    fn only_the_main_window_and_well_formed_wall_windows_pass_the_label_check() {
        let larkspur = ProjectId::new("larkspur").unwrap();
        let other = ProjectId::new("other.project:2").unwrap();
        let first = wall_label(&larkspur, 1);
        assert_eq!(
            first,
            wall_label(&larkspur, 1),
            "a project's labels are stable"
        );
        assert_ne!(first, wall_label(&other, 1));
        assert_ne!(first, wall_label(&larkspur, 2));
        assert!(!first.contains("larkspur"));
        for page in 1..=WINDOWS_MAX {
            let label = wall_label(&other, page);
            assert!(window_label_valid(&label), "{label}");
            assert_eq!(
                label.len(),
                "terminals-".len() + 16 + 1 + page.to_string().len()
            );
        }
        assert!(window_label_valid("main"));
        for label in [
            "",
            "Main",
            "main2",
            "terminals",
            "terminals-0123456789abcdef",
            "terminals-0123456789abcdef-0",
            "terminals-0123456789abcdef-9",
            "terminals-0123456789abcdef-01",
            "terminals-0123456789ABCDEF-1",
            "terminals-0123456789abcde-1",
            "terminals-0123456789abcdef0-1",
            "terminals-0123456789abcdef-1-1",
            "terminals-0123456789abcdeg-1",
            "settings",
        ] {
            assert!(!window_label_valid(label), "{label:?}");
        }
    }

    /// A wall window reaches only the project its label was made for; the main window reaches
    /// every project, and any other window none.
    #[test]
    fn a_wall_window_acts_only_on_the_project_its_label_was_made_for() {
        let larkspur = ProjectId::new("larkspur").unwrap();
        let other = ProjectId::new("other.project:2").unwrap();
        for page in [1, WINDOWS_MAX] {
            assert!(window_admits(&wall_label(&larkspur, page), &larkspur));
            assert!(!window_admits(&wall_label(&larkspur, page), &other));
            assert!(!window_admits(&wall_label(&other, page), &larkspur));
        }
        assert!(window_admits("main", &larkspur));
        assert!(window_admits("main", &other));
        let past = format!("{}9", wall_prefix(&larkspur));
        for label in ["settings", "", past.as_str()] {
            assert!(!window_admits(label, &larkspur), "{label:?}");
        }
    }

    /// Closing the main window closes every wall window and nothing else; removing a project
    /// closes only that project's wall windows.
    #[test]
    fn closing_main_closes_every_wall_and_removing_a_project_closes_only_its_walls() {
        let larkspur = ProjectId::new("larkspur").unwrap();
        let other = ProjectId::new("other.project:2").unwrap();
        let (first, second, third) = (
            wall_label(&larkspur, 1),
            wall_label(&larkspur, 2),
            wall_label(&other, 1),
        );
        let open = [
            "main",
            first.as_str(),
            "settings",
            third.as_str(),
            second.as_str(),
        ];
        assert_eq!(
            walls_to_close(open, None),
            vec![first.as_str(), third.as_str(), second.as_str()]
        );
        assert_eq!(
            walls_to_close(open, Some(&larkspur)),
            vec![first.as_str(), second.as_str()]
        );
        assert_eq!(walls_to_close(open, Some(&other)), vec![third.as_str()]);
        assert!(walls_to_close(["main", "settings"], None).is_empty());
    }

    /// A wall that opens the next window titles it with the project name its own title shows.
    #[test]
    fn a_grown_wall_keeps_the_project_name_its_title_shows() {
        assert_eq!(
            wall_title_name("Larkspur Booking \u{b7} Terminals 2 of 3"),
            "Larkspur Booking"
        );
        assert_eq!(
            wall_title_name("A \u{b7} Terminals of B \u{b7} Terminals 1 of 1"),
            "A \u{b7} Terminals of B"
        );
        for title in ["CrazyAgent", "", " \u{b7} Terminals 1 of 1"] {
            assert_eq!(wall_title_name(title), "Project", "{title:?}");
        }
    }
}
