use std::io;
use std::path::Path;

/// One event of the direct tmux path, as the unix client reports it. Off unix no client
/// attaches, so a host only ever takes `Closed`, from a drain that finds its queue lost.
pub enum Event {
    Restored { view: u64, bytes: Vec<u8> },
    Output { views: Vec<u64>, bytes: Vec<u8> },
    Refused { view: u64, code: &'static str },
    Closed,
}

/// The direct tmux path's client, never made off unix: every view there is engine-fed.
pub struct Control;

impl Control {
    /// Refuses the direct path off unix. Inputs: the tmux program, the engine socket and
    /// the event sink, as the unix client takes them. Output: always an `Unsupported` error.
    pub fn attach(
        _tmux: &Path,
        _socket: &Path,
        _sink: impl FnMut(Event) -> bool + Send + 'static,
    ) -> io::Result<Control> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native_terminal_direct_unsupported",
        ))
    }

    /// Refuses a binding, as no client exists. Inputs: the view and the agent window name.
    /// Output: `native_terminal_direct_unsupported`.
    pub fn bind(&mut self, _view: u64, _window: &str) -> Result<(), &'static str> {
        Err("native_terminal_direct_unsupported")
    }

    /// Closes a view's binding, of which there is none. Input: the view. Output: false.
    pub fn close(&mut self, _view: u64) -> bool {
        false
    }
}
