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
/// points.
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
    Paste,
    SelectAll,
    Focus(bool),
}

/// The host function a view hands its events to, with the view's id.
pub type Sink = fn(u64, ViewEvent);
