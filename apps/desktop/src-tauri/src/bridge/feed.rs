use super::terminal::{
    NATIVE_OVERFLOW, Reservation, Sink, Terminals, ViewKey, input_request, plain_code,
};
use super::{BridgeError, failure};
use crazyagent::engine::owner::TerminalSession;
use crazyagent::terminal::{
    AgentId, INPUT_BYTES_MAX, PASTE_BYTES_MAX, SIZE_MAX, SIZE_MIN, TerminalFrame, TerminalRequest,
    paste_keys,
};
use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::mpsc;

/// One event a native view's host reads through `EngineFeed::take`, the native form of the
/// frames a page view's channel carries: the size once attached, the screen and then live
/// output in order, and one `Ended` with a plain code for every way the view stops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedEvent {
    Attached { view: u64, cols: u16, rows: u16 },
    Output { view: u64, bytes: Vec<u8> },
    Ended { view: u64, code: String },
}

impl FeedEvent {
    fn view(&self) -> u64 {
        match self {
            FeedEvent::Attached { view, .. }
            | FeedEvent::Output { view, .. }
            | FeedEvent::Ended { view, .. } => *view,
        }
    }
}

/// One inbox message: an event under the token of the open that sent it, which only
/// `EngineFeed::take` reads, so an event of an open the host closed or opened again, maybe in
/// another window, never reaches the view now holding its id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fed {
    token: u64,
    event: FeedEvent,
}

impl Fed {
    pub(super) fn new(token: u64, event: FeedEvent) -> Fed {
        Fed { token, event }
    }
}

/// Runs after each event is queued, on any thread, so a host can reach its UI thread: the
/// macOS host dispatches to the main queue, the Windows host posts to its window.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// The engine session one bound native view opens, at its grid, input-only for a view that
/// draws the pane from the engine's tmux server directly.
pub type Opening = Pin<Box<dyn Future<Output = Result<TerminalSession, BridgeError>> + Send>>;

/// Opens the engine session of a bound native view. Takes the view's key, its grid and
/// whether the session is input-only. Returns the opening session.
pub type Opener = Arc<dyn Fn(ViewKey, (u16, u16), bool) -> Opening + Send + Sync>;

/// Turns one engine terminal frame into the event a native view's host reads, under the
/// rules `channel_message` keeps for a page view. Takes the view, its agent and the frame.
/// Returns the event and whether it is the view's last.
pub fn feed_event(view: u64, agent: &AgentId, frame: TerminalFrame) -> (FeedEvent, bool) {
    let ended = |code: String| (FeedEvent::Ended { view, code }, true);
    match frame {
        TerminalFrame::Output { bytes } => (FeedEvent::Output { view, bytes }, false),
        TerminalFrame::Attached {
            agent_id,
            cols,
            rows,
        } if &agent_id == agent => (FeedEvent::Attached { view, cols, rows }, false),
        TerminalFrame::Attached { .. } => ended("terminal_agent_mismatch".to_owned()),
        TerminalFrame::Closed { reason } => ended(plain_code(&reason, "terminal_closed")),
        TerminalFrame::Rejected { code } => ended(plain_code(&code, "terminal_attach_failed")),
    }
}

/// The `Paste` requests that carry one owner paste's filtered keys: chunks of at most
/// `INPUT_BYTES_MAX` bytes in order, the last one marked. The engine joins them before it
/// reads any text, so a chunk may end inside a character. Takes 1 to `PASTE_BYTES_MAX` keys.
/// Returns the requests.
pub fn paste_requests(keys: &[u8]) -> Vec<TerminalRequest> {
    assert!(!keys.is_empty() && keys.len() <= PASTE_BYTES_MAX);
    let count = keys.len().div_ceil(INPUT_BYTES_MAX);
    let requests: Vec<TerminalRequest> = keys
        .chunks(INPUT_BYTES_MAX)
        .enumerate()
        .map(|(at, chunk)| TerminalRequest::Paste {
            bytes: chunk.to_vec(),
            last: at + 1 == count,
        })
        .collect();
    assert_eq!(requests.len(), count);
    assert!(count <= PASTE_BYTES_MAX / INPUT_BYTES_MAX);
    requests
}

/// Where one native view stands: bound to its gated place, opening its engine session under
/// the token of that one open (its task holds the window and key), or open as the window's
/// view with this serial.
enum Slot {
    Bound {
        place: Reservation,
        input_only: bool,
    },
    Opening {
        task: tauri::async_runtime::JoinHandle<()>,
        token: u64,
    },
    Open {
        window: String,
        key: ViewKey,
        serial: u64,
    },
}

type Slots = Arc<Mutex<HashMap<u64, Slot>>>;

/// The engine feed of the native terminal views, for every OS: each view is one owner
/// terminal session that sits among the window's views like a page view, so the window's
/// counts, a page load and a closed window end it as they end a page view. Keys, wheel keys,
/// resizes and pastes are queued without waiting, whole or not at all; a full queue ends the
/// view with `NATIVE_OVERFLOW`, never a silent drop.
pub struct EngineFeed {
    terminals: Arc<Terminals>,
    opener: Opener,
    inbox: mpsc::Sender<Fed>,
    wake: Wake,
    slots: Slots,
    opened: u64,
    live: HashMap<u64, u64>,
}

/// An `io` error carrying a plain code. Takes the error kind and the code. Returns it.
fn refused(kind: io::ErrorKind, code: &'static str) -> io::Error {
    io::Error::new(kind, code)
}

impl EngineFeed {
    /// Makes the feed. Takes the terminals, the opener of engine sessions, the bounded inbox
    /// every view's events go to and the wake its host runs after each one. Returns the feed.
    pub fn new(
        terminals: Arc<Terminals>,
        opener: Opener,
        inbox: mpsc::Sender<Fed>,
        wake: Wake,
    ) -> EngineFeed {
        EngineFeed {
            terminals,
            opener,
            inbox,
            wake,
            slots: Arc::new(Mutex::new(HashMap::new())),
            opened: 0,
            live: HashMap::new(),
        }
    }

    /// Hands over one inbox message's event while its open is the view's live one, the last
    /// opened and not closed since; any other is stale and dropped. Takes the message.
    /// Returns the event, or none.
    pub fn take(&self, fed: Fed) -> Option<FeedEvent> {
        let view = fed.event.view();
        (self.live.get(&view) == Some(&fed.token)).then_some(fed.event)
    }

    fn slots(&self) -> io::Result<MutexGuard<'_, HashMap<u64, Slot>>> {
        self.slots
            .lock()
            .map_err(|_| refused(io::ErrorKind::Other, "terminal_state_failed"))
    }

    /// Binds a native view to the place the shared gate took for it, before the host opens
    /// it. Takes the view, the place and whether its session is input-only. Returns unit, or
    /// `native_view_taken` for a view already bound, which gives the place back.
    pub fn bind(
        &mut self,
        view: u64,
        place: Reservation,
        input_only: bool,
    ) -> Result<(), BridgeError> {
        let mut slots = self.slots().map_err(|_| failure("terminal_state_failed"))?;
        if slots.contains_key(&view) {
            return Err(failure("native_view_taken"));
        }
        slots.insert(view, Slot::Bound { place, input_only });
        Ok(())
    }

    /// Opens a bound view's engine session at its grid without waiting: its events, the
    /// `Attached` first, reach the inbox, and a failed open sends `Ended` with the code and
    /// gives the place back. Takes the view and its grid in `SIZE_MIN..=SIZE_MAX`. Returns
    /// once started, or an error for a view not bound or a grid out of bounds.
    pub fn open(&mut self, view: u64, grid: (u16, u16)) -> io::Result<()> {
        let size = SIZE_MIN..=SIZE_MAX;
        if !size.contains(&grid.0) || !size.contains(&grid.1) {
            return Err(refused(io::ErrorKind::InvalidInput, "terminal_size_bounds"));
        }
        self.opened += 1;
        let token = self.opened;
        let mut slots = self.slots()?;
        let (place, input_only) = match slots.remove(&view) {
            Some(Slot::Bound { place, input_only }) => (place, input_only),
            Some(other) => {
                slots.insert(view, other);
                return Err(refused(io::ErrorKind::AlreadyExists, "native_view_opened"));
            }
            None => return Err(refused(io::ErrorKind::NotFound, "native_view_unbound")),
        };
        let (window, key) = {
            let (window, key) = place.place();
            (window.to_owned(), key.clone())
        };
        let opening = (self.opener)(key.clone(), grid, input_only);
        let sink = Sink::Native {
            view,
            token,
            inbox: self.inbox.clone(),
            wake: Arc::clone(&self.wake),
        };
        let (terminals, ended, wake, shared) = (
            Arc::clone(&self.terminals),
            self.inbox.clone(),
            Arc::clone(&self.wake),
            Arc::clone(&self.slots),
        );
        let (task_window, task_key) = (window, key);
        let task = tauri::async_runtime::spawn(async move {
            let attached = match opening.await {
                Ok(session) => place.attach(session, sink),
                Err(error) => {
                    drop(place);
                    Err(error)
                }
            };
            let failed = {
                let Ok(mut slots) = shared.lock() else {
                    return;
                };
                let current = matches!(
                    slots.get(&view),
                    Some(Slot::Opening { token: open, .. }) if *open == token
                );
                match attached {
                    Ok(serial) if current => {
                        slots.insert(
                            view,
                            Slot::Open {
                                window: task_window,
                                key: task_key,
                                serial,
                            },
                        );
                        None
                    }
                    Ok(serial) => {
                        let _ = terminals.detach(&task_window, &task_key.agent, serial);
                        None
                    }
                    Err(error) if current => {
                        slots.remove(&view);
                        Some(error.code)
                    }
                    Err(_) => None,
                }
            };
            if let Some(code) = failed {
                let code = code.to_owned();
                let event = Fed::new(token, FeedEvent::Ended { view, code });
                if ended.send(event).await.is_ok() {
                    wake();
                }
            }
        });
        slots.insert(view, Slot::Opening { task, token });
        drop(slots);
        self.live.insert(view, token);
        Ok(())
    }

    /// Types one view's keys, `Scroll` for keys the owner's wheel made. Takes the view, 1 to
    /// `INPUT_BYTES_MAX` bytes and whether the wheel made them. Returns once queued.
    pub fn input(&mut self, view: u64, bytes: &[u8], wheel: bool) -> io::Result<()> {
        if bytes.is_empty() || bytes.len() > INPUT_BYTES_MAX {
            return Err(refused(
                io::ErrorKind::InvalidInput,
                "terminal_input_bounds",
            ));
        }
        self.send(view, vec![input_request(bytes.to_vec(), wheel)])
    }

    /// Pastes the owner's text into one view: every control character but tab and Enter
    /// dropped as the page's paste does, line ends made CR, then queued whole as `Paste`
    /// chunks the engine filters again and pastes once. Takes the view and at most
    /// `PASTE_BYTES_MAX` bytes of UTF-8. Returns once queued, at once for text that keeps
    /// nothing, or an error for text out of bounds, of which nothing is sent.
    pub fn paste(&mut self, view: u64, text: &[u8]) -> io::Result<()> {
        if text.len() > PASTE_BYTES_MAX {
            return Err(refused(
                io::ErrorKind::InvalidInput,
                "terminal_paste_bounds",
            ));
        }
        let text = std::str::from_utf8(text)
            .map_err(|_| refused(io::ErrorKind::InvalidData, "terminal_paste_invalid"))?;
        let keys = paste_keys(text);
        if keys.is_empty() {
            return Ok(());
        }
        self.send(view, paste_requests(&keys))
    }

    /// Sets one view's grid, which the engine applies to its agent's window. Takes the view
    /// and the grid. Returns once queued.
    pub fn resize(&mut self, view: u64, grid: (u16, u16)) -> io::Result<()> {
        let (cols, rows) = grid;
        self.send(view, vec![TerminalRequest::Resize { cols, rows }])
    }

    /// Closes one view: a bound place is given back, an opening session stopped, an open view
    /// detached from the engine, whose agent keeps running, and every later event of its
    /// opens dropped. Takes the view. Returns nothing.
    pub fn close(&mut self, view: u64) {
        self.live.remove(&view);
        self.end(view)
    }

    /// Ends one view's slot as `close` does but keeps its live token, so the `Ended` sent
    /// next still reaches its host. Takes the view. Returns nothing.
    fn end(&mut self, view: u64) {
        let Ok(mut slots) = self.slots() else {
            return;
        };
        match slots.remove(&view) {
            Some(Slot::Opening { task, .. }) => task.abort(),
            Some(Slot::Open {
                window,
                key,
                serial,
            }) => {
                let _ = self.terminals.detach(&window, &key.agent, serial);
            }
            Some(Slot::Bound { .. }) | None => {}
        }
        assert!(!slots.contains_key(&view));
    }

    /// Queues requests for one open view, whole or not at all; a queue without room for all
    /// of them ends the view, tells its host with `Ended(NATIVE_OVERFLOW)` and answers that
    /// code. Takes the view and its valid requests. Returns once queued, or the refusal.
    fn send(&mut self, view: u64, requests: Vec<TerminalRequest>) -> io::Result<()> {
        let (window, key, serial) = match self.slots()?.get(&view) {
            Some(Slot::Open {
                window,
                key,
                serial,
            }) => (window.clone(), key.clone(), *serial),
            _ => {
                return Err(refused(
                    io::ErrorKind::NotConnected,
                    "terminal_not_attached",
                ));
            }
        };
        match self
            .terminals
            .try_send_view(&window, &key, serial, requests)
        {
            Ok(()) => Ok(()),
            Err(error) if error.code == NATIVE_OVERFLOW => {
                self.end(view);
                assert!(
                    self.live.contains_key(&view),
                    "an open view has a live token"
                );
                let ended = FeedEvent::Ended {
                    view,
                    code: NATIVE_OVERFLOW.to_owned(),
                };
                if self
                    .inbox
                    .try_send(Fed::new(self.live[&view], ended))
                    .is_ok()
                {
                    (self.wake)();
                }
                Err(refused(io::ErrorKind::WouldBlock, NATIVE_OVERFLOW))
            }
            Err(error) => Err(refused(io::ErrorKind::Other, error.code)),
        }
    }
}

#[cfg(test)]
#[path = "feed_tests.rs"]
mod tests;
