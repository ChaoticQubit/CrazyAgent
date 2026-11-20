use super::pane::Input;

/// Which mouse event a view got.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseKind {
    Press,
    Drag,
    Release,
}

/// What a native view tells its host, on the main thread, in the order the platform
/// delivered it. Points are in the view's own coordinates from its top left corner, in
/// points. `Marked` asks for a redraw: the input method's marked text changed (read it with
/// `marked_text`).
#[derive(Debug, PartialEq)]
pub enum ViewEvent {
    Key(Input),
    Wheel {
        point: (f64, f64),
        delta: f64,
        precise: bool,
    },
    Mouse {
        kind: MouseKind,
        point: (f64, f64),
        option: bool,
        clicks: u8,
    },
    Menu {
        point: (f64, f64),
    },
    Copy,
    /// The copy chord of a host without Cmd (Ctrl+C, Ctrl+Shift+C): the host copies the
    /// selection if there is one, else applies this input (^C).
    #[cfg(windows)]
    CopyKey(Input),
    /// The render thread faulted the view for good (Windows), told by its window once it
    /// hid: the host ends it with `native_terminal_gpu`, as macOS ends a view whose frame
    /// the GPU refused.
    #[cfg(windows)]
    Faulted,
    Paste,
    SelectAll,
    Focus(bool),
    Marked,
}

/// The host function a view hands its events to, with the view's id.
pub type Sink = fn(u64, ViewEvent);
