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
pub const VIEWS_MAX: usize = crazyagent::terminal::DESKTOP_VIEWS_MAX;
pub const WINDOWS_MAX: usize = 8;
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
    label == "main"
}

/// The label of one terminal wall window. Takes the project id and the page number.
/// Returns `terminals-<16 hex digits of the project>-<page>`.
pub fn wall_label(project: &ProjectId, page: usize) -> String {
    let _ = (project, page);
    "main".to_owned()
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
struct Views {
    next: u64,
    viewers: HashMap<AgentId, Viewer>,
    opening: HashMap<AgentId, u64>,
    cleared: u64,
}

impl Views {
    #[cfg(test)]
    fn serial(&self, _window: &str, agent: &AgentId) -> Option<u64> {
        self.viewers.get(agent).map(|viewer| viewer.serial)
    }

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
    /// take over the newer one, and a place taken before the last page load never lands.
    /// Takes the agent, the place's serial and its request queue.
    /// Returns unit, `terminal_view_replaced` or `terminal_viewers_full`.
    fn install(
        &mut self,
        _window: &str,
        agent: &AgentId,
        serial: u64,
        requests: mpsc::Sender<TerminalRequest>,
    ) -> Result<(), BridgeError> {
        let newer = serial <= self.cleared
            || self.opening.get(agent).is_some_and(|open| *open > serial)
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
    window: String,
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
    pub fn room(
        self: &Arc<Self>,
        window: &str,
        agent: &AgentId,
    ) -> Result<Reservation, BridgeError> {
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
            window: window.to_owned(),
            agent: agent.clone(),
            serial,
        })
    }

    /// Queues one input, prompt or resize request for an attached agent terminal.
    /// Takes the terminal name and the request.
    /// Returns once queued in order, or a plain code for a bad request or no view.
    pub async fn send(
        &self,
        _window: &str,
        agent: &AgentId,
        request: TerminalRequest,
    ) -> Result<(), BridgeError> {
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
    pub fn detach(&self, _window: &str, agent: &AgentId, serial: u64) -> Result<(), BridgeError> {
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

    /// Ends every view of the page the main webview is leaving, as a reload or a new page
    /// load starts: that page can no longer read or detach its views, so they would hold the
    /// viewer slots for good. Dropping a view closes its request queue, which detaches it
    /// from the engine, and an attach the old page still had opening is refused when it
    /// lands. Takes nothing. Returns nothing.
    pub fn page_started(&self, _window: &str) {
        if let Ok(mut views) = self.0.lock() {
            views.viewers.clear();
            views.opening.clear();
            views.cleared = views.next;
            assert_eq!(views.held(), 0);
        }
    }

    /// Ends every view of a window that closed and forgets the window, so an attach it still
    /// had opening is refused when it lands. Takes the window label. Returns nothing.
    pub fn window_closed(&self, _window: &str) {}

    fn forget(&self, _window: &str, agent: &AgentId, serial: u64) {
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
            .install(WALL, &team(1), place.serial, requests.clone())
            .unwrap();
        let late = terminals.room(WALL, &team(2)).unwrap();

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
}
