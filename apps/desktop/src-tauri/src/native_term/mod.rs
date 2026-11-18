#[cfg(target_os = "macos")]
mod a11y;
#[cfg(target_os = "macos")]
mod clip;
#[cfg(target_os = "macos")]
mod codec;
#[cfg(target_os = "macos")]
mod control;
#[cfg(target_os = "macos")]
mod host;
#[cfg(target_os = "macos")]
mod ime;
#[cfg(target_os = "macos")]
mod pane;
#[cfg(target_os = "macos")]
mod render;
#[cfg(target_os = "macos")]
mod screen;
#[cfg(target_os = "macos")]
mod view;

#[cfg(target_os = "macos")]
use host::{Clips, on_copy, on_mouse, on_native, on_paste, on_wheel};
#[cfg(target_os = "macos")]
pub use pane::Probe;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::OnceLock;

/// The research switch (DEC-203): `1` draws every engine-server agent terminal natively;
/// anything else leaves today's renderer untouched. Read once at desktop start.
pub const SWITCH_ENV: &str = "CRAZYAGENT_NATIVE_TERMINAL";

/// The engine's own tmux socket the native views attach to, set by whoever starts the
/// desktop with the switch on.
pub const SOCKET_ENV: &str = "CRAZYAGENT_NATIVE_TMUX_SOCKET";

/// The longest agent window name a page may ask for.
const NAME_BYTES_MAX: usize = 64;

static SWITCH: OnceLock<bool> = OnceLock::new();

/// Whether native terminals are on: macOS with `CRAZYAGENT_NATIVE_TERMINAL=1`, read once.
/// Output: the answer.
pub fn enabled() -> bool {
    *SWITCH.get_or_init(|| {
        cfg!(target_os = "macos") && std::env::var(SWITCH_ENV).as_deref() == Ok("1")
    })
}

/// Whether a socket path names an engine's own tmux server by its shape: absolute, plain
/// parts, `tmux.sock` in a `terminal` folder. Input: the path. Output: the answer.
pub fn engine_socket_shape(path: &std::path::Path) -> bool {
    let plain = path.components().all(|part| {
        matches!(
            part,
            std::path::Component::RootDir | std::path::Component::Normal(_)
        )
    });
    let named = path.file_name() == Some(OsStr::new("tmux.sock"));
    let folder = path.parent().and_then(|p| p.file_name()) == Some(OsStr::new("terminal"));
    let text = path.to_string_lossy();
    path.is_absolute() && plain && named && folder && !text.contains("/tmux-")
}

/// Checks the engine socket the switch names: its shape, an `engine.db` beside its
/// `terminal` folder, and a socket owned by this user, read without following a link, so a
/// native-mode lead's or teammates' server (inside a sandbox profile) is never reached by a
/// plain client. Input: the variable's value. Output: the socket path, or a plain code.
pub fn engine_socket(raw: &OsStr) -> Result<PathBuf, &'static str> {
    let path = PathBuf::from(raw);
    if !engine_socket_shape(&path) {
        return Err("native_terminal_socket_invalid");
    }
    let root = path
        .parent()
        .and_then(|p| p.parent())
        .ok_or("native_terminal_socket_invalid")?;
    let engine = std::fs::symlink_metadata(root.join("engine.db")).is_ok_and(|m| m.is_file());
    if !engine {
        return Err("native_terminal_socket_not_engine");
    }
    #[cfg(not(target_os = "macos"))]
    return Err("native_terminal_off");
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|_| "native_terminal_socket_missing")?;
        let user = rustix::process::getuid().as_raw();
        if !metadata.file_type().is_socket() || metadata.uid() != user {
            return Err("native_terminal_socket_invalid");
        }
        assert!(path.is_absolute());
        Ok(path)
    }
}

/// Whether the agent has its own tmux server, as a native-mode lead does inside its sandbox:
/// any file at that server's private socket path, read by `lstat` only and never connected
/// to, since a tmux server can make its client run a command. Such an agent keeps today's
/// renderer, whose engine reaches that server only from inside the lead's profile. Inputs:
/// the engine root and the checked window name. Output: the answer; a name that is not a team
/// agent has no own server.
pub fn own_server(engine_root: &std::path::Path, name: &str) -> bool {
    assert!(engine_root.is_absolute());
    let Ok(agent) = crate::bridge::terminal::agent(name) else {
        return false;
    };
    let Ok(socket) = crazyagent::terminal::harness_socket(engine_root, &agent) else {
        return false;
    };
    assert!(!socket.starts_with(engine_root.join("terminal")));
    std::fs::symlink_metadata(socket).is_ok()
}

/// Checks the agent window name a page asks for: a team agent id, plain letters, digits,
/// dashes and underscores. Input: the name. Output: the name, or a plain code.
pub fn window_name(name: &str) -> Result<String, &'static str> {
    let plain = !name.is_empty()
        && name.len() <= NAME_BYTES_MAX
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !plain || crate::bridge::terminal::agent(name).is_err() {
        return Err("native_terminal_agent_invalid");
    }
    assert!(
        !name
            .bytes()
            .any(|b| matches!(b, b';' | b':' | b'.' | b' ' | b'=' | b'"'))
    );
    assert!(name.len() <= NAME_BYTES_MAX);
    Ok(name.to_owned())
}

/// A rectangle in CSS pixels of the page, top-left origin, as `getBoundingClientRect` gives.
#[derive(serde::Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    /// Whether every side is a finite number and the size is not negative or huge.
    fn valid(&self) -> bool {
        let finite = [self.x, self.y, self.w, self.h]
            .iter()
            .all(|v| v.is_finite() && v.abs() <= 1.0e5);
        finite && self.w >= 0.0 && self.h >= 0.0
    }
}

/// One input the page's test hook forwards, as the page saw it: text, a named key, a wheel
/// move, a mouse press, drag or release at a point in the view (CSS pixels), a copy, or a
/// paste with its text.
#[derive(serde::Deserialize, Debug)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HookInput {
    Text { text: String },
    Key { key: String, ctrl: bool },
    Wheel { dy: f64, mode: u32, x: f64, y: f64 },
    Press { x: f64, y: f64, option: bool },
    Drag { x: f64, y: f64, option: bool },
    Release { x: f64, y: f64, option: bool },
    Copy,
    Paste { text: String },
}

/// One test-hook event: its sequence number, the page's dispatch time (wall ms), the text
/// the screen must show before the input counts as rendered, and the input.
#[derive(serde::Deserialize, Debug)]
pub struct HookEvent {
    pub seq: u64,
    pub t0: f64,
    pub expect: Option<String>,
    #[serde(flatten)]
    pub input: HookInput,
}

/// The longest text one typed test-hook event may carry.
const TEXT_BYTES_MAX: usize = 4096;

/// The longest key name one test-hook event may carry.
const KEY_BYTES_MAX: usize = 32;

/// The longest paste one test-hook event may carry, the most a copy holds.
const PASTE_BYTES_MAX: usize = 1 << 20;

/// The largest point or wheel delta one test-hook event may carry, in CSS pixels.
const POINT_ABS_MAX: f64 = 1.0e5;

/// The longest text a test-hook event may wait to see on screen.
const EXPECT_CHARS_MAX: usize = 256;

#[cfg(target_os = "macos")]
const _: () = assert!(EXPECT_CHARS_MAX == pane::EXPECT_CHARS_MAX);
#[cfg(target_os = "macos")]
const _: () = assert!(PASTE_BYTES_MAX == codec::COPY_BYTES_MAX);

impl HookEvent {
    /// Whether every value the page supplied is inside its bound, so nothing a page sends can
    /// reach an assertion below: a finite dispatch time, finite points and wheel deltas, a
    /// DOM wheel mode, and text, key, paste and awaited-text sizes. Output: the answer.
    pub fn valid(&self) -> bool {
        let point = |v: f64| v.is_finite() && v.abs() <= POINT_ABS_MAX;
        let expect = self
            .expect
            .as_ref()
            .is_none_or(|t| t.chars().count() <= EXPECT_CHARS_MAX);
        let input = match &self.input {
            HookInput::Text { text } => text.len() <= TEXT_BYTES_MAX,
            HookInput::Key { key, .. } => key.len() <= KEY_BYTES_MAX,
            HookInput::Wheel { dy, mode, x, y } => {
                point(*dy) && *mode <= 2 && point(*x) && point(*y)
            }
            HookInput::Press { x, y, .. }
            | HookInput::Drag { x, y, .. }
            | HookInput::Release { x, y, .. } => point(*x) && point(*y),
            HookInput::Copy => true,
            HookInput::Paste { text } => text.len() <= PASTE_BYTES_MAX,
        };
        self.t0.is_finite() && expect && input
    }
}

/// What a native view tells its page: its mirror is ready, one timed input finished, or
/// the view ended with a plain code and the page falls back to today's renderer.
#[derive(serde::Serialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Report {
    Ready,
    #[cfg(target_os = "macos")]
    Probe(pane::Probe),
    Ended {
        code: String,
    },
}

/// Says whether native terminals are on, so the page picks its renderer once at start.
#[tauri::command]
pub fn desktop_native_terminal_enabled() -> bool {
    enabled()
}

/// Opens a native view for an engine-server agent terminal over the page rectangle.
/// Inputs: the app, the calling window, the agent's window name, the rectangle and the
/// channel for its reports. Output: the view id, or a plain code.
#[tauri::command]
pub fn desktop_native_terminal_open(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    name: String,
    rect: Rect,
    output: tauri::ipc::Channel<Report>,
) -> Result<u64, String> {
    if !enabled() {
        return Err("native_terminal_off".into());
    }
    let name = window_name(&name)?;
    if !rect.valid() {
        return Err("native_terminal_rect_invalid".into());
    }
    #[cfg(target_os = "macos")]
    {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        window
            .with_webview(move |webview| {
                let _ = sender.send(webview.inner() as usize);
            })
            .map_err(|_| "native_terminal_webview_missing")?;
        let pointer = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| "native_terminal_webview_missing")?;
        if pointer == 0 {
            return Err("native_terminal_webview_missing".into());
        }
        host::open(
            &app,
            (window.label().to_owned(), pointer),
            name,
            rect,
            output,
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, window, name, output);
        Err("native_terminal_off".into())
    }
}

/// Moves a native view to a new page rectangle. Inputs: the app, the calling window, the
/// view id and the rectangle. Output: unit, or a plain code.
#[tauri::command]
pub fn desktop_native_terminal_place(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    id: u64,
    rect: Rect,
) -> Result<(), String> {
    if !enabled() || !rect.valid() {
        return Err("native_terminal_rect_invalid".into());
    }
    #[cfg(target_os = "macos")]
    return host::place(&app, window.label().to_owned(), id, rect);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, window, id);
        Err("native_terminal_off".into())
    }
}

/// Hides a native view while a page overlay covers it, and shows it again. Inputs: the app,
/// the calling window, the view id and whether it is covered. Output: unit, or a plain code.
#[tauri::command]
pub fn desktop_native_terminal_cover(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    id: u64,
    covered: bool,
) -> Result<(), String> {
    if !enabled() {
        return Err("native_terminal_off".into());
    }
    #[cfg(target_os = "macos")]
    return host::cover(&app, window.label().to_owned(), id, covered);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, window, id, covered);
        Err("native_terminal_off".into())
    }
}

/// Feeds one test-hook event from the page into a native view's own input handlers.
/// Inputs: the app, the calling window, the view id and the event. Output: unit, or a plain
/// code.
#[tauri::command]
pub fn desktop_native_terminal_input(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    id: u64,
    event: HookEvent,
) -> Result<(), String> {
    if !enabled() {
        return Err("native_terminal_off".into());
    }
    if !event.valid() {
        return Err("native_terminal_input_invalid".into());
    }
    #[cfg(target_os = "macos")]
    return host::input(&app, window.label().to_owned(), id, event);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, window, id, event);
        Err("native_terminal_off".into())
    }
}

/// Closes one native view of the calling window. Inputs: the app, the calling window and
/// the view id. Output: unit, or a plain code.
#[tauri::command]
pub fn desktop_native_terminal_close(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    id: u64,
) -> Result<(), String> {
    if !enabled() {
        return Err("native_terminal_off".into());
    }
    #[cfg(target_os = "macos")]
    return host::close(&app, window.label().to_owned(), Some(id));
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, window, id);
        Err("native_terminal_off".into())
    }
}

/// Closes every native view of a window whose page started loading again or that closed.
/// Inputs: the app and the window label.
pub fn window_gone(app: &tauri::AppHandle, label: &str) {
    if !enabled() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let _ = host::close(app, label.to_owned(), None);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, label);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn only_an_engine_socket_shape_passes() {
        assert!(engine_socket_shape(Path::new(
            "/tmp/r/engine/terminal/tmux.sock"
        )));
        assert!(!engine_socket_shape(Path::new("engine/terminal/tmux.sock")));
        assert!(!engine_socket_shape(Path::new(
            "/tmp/r/engine/lead/tmux.sock"
        )));
        assert!(!engine_socket_shape(Path::new(
            "/tmp/r/engine/terminal/other.sock"
        )));
        assert!(!engine_socket_shape(Path::new(
            "/tmp/r/../terminal/tmux.sock"
        )));
        assert!(!engine_socket_shape(Path::new(
            "/private/tmp/tmux-501/terminal/tmux.sock"
        )));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn engine_socket_needs_an_engine_db_and_a_socket() {
        let root = tempfile::tempdir().expect("temp dir");
        let terminal = root.path().join("terminal");
        std::fs::create_dir(&terminal).expect("terminal folder");
        let socket = terminal.join("tmux.sock");
        assert_eq!(
            engine_socket(socket.as_os_str()),
            Err("native_terminal_socket_not_engine")
        );
        std::fs::write(root.path().join("engine.db"), b"").expect("engine.db");
        assert_eq!(
            engine_socket(socket.as_os_str()),
            Err("native_terminal_socket_missing")
        );
        std::fs::write(&socket, b"").expect("plain file");
        assert_eq!(
            engine_socket(socket.as_os_str()),
            Err("native_terminal_socket_invalid")
        );
        std::fs::remove_file(&socket).expect("remove");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        assert_eq!(engine_socket(socket.as_os_str()), Ok(socket.clone()));
    }

    #[test]
    fn window_names_are_plain_agent_ids() {
        assert!(window_name("team-1").is_ok());
        assert!(window_name("").is_err());
        assert!(window_name("team-1;kill-server").is_err());
        assert!(window_name("a b").is_err());
        assert!(window_name(&"x".repeat(NAME_BYTES_MAX + 1)).is_err());
    }

    #[test]
    fn hook_events_out_of_bounds_are_refused() {
        let event = |input: HookInput| HookEvent {
            seq: 1,
            t0: 1.0,
            expect: None,
            input,
        };
        assert!(event(HookInput::Text { text: "a".into() }).valid());
        assert!(
            event(HookInput::Wheel {
                dy: -120.0,
                mode: 0,
                x: 4.0,
                y: 4.0
            })
            .valid()
        );
        assert!(
            !event(HookInput::Wheel {
                dy: f64::NAN,
                mode: 0,
                x: 4.0,
                y: 4.0
            })
            .valid()
        );
        assert!(
            !event(HookInput::Wheel {
                dy: 1.0,
                mode: 3,
                x: 4.0,
                y: 4.0
            })
            .valid()
        );
        assert!(
            !event(HookInput::Press {
                x: f64::INFINITY,
                y: 0.0,
                option: false
            })
            .valid()
        );
        assert!(
            !event(HookInput::Text {
                text: "a".repeat(TEXT_BYTES_MAX + 1)
            })
            .valid()
        );
        assert!(
            !event(HookInput::Paste {
                text: "a".repeat(PASTE_BYTES_MAX + 1)
            })
            .valid()
        );
        let long = HookEvent {
            expect: Some("x".repeat(EXPECT_CHARS_MAX + 1)),
            ..event(HookInput::Copy)
        };
        assert!(!long.valid());
        assert!(
            !HookEvent {
                t0: f64::NAN,
                ..event(HookInput::Copy)
            }
            .valid()
        );
    }

    #[test]
    fn rects_must_be_finite_and_not_negative() {
        assert!(
            Rect {
                x: 1.0,
                y: 2.0,
                w: 300.0,
                h: 200.0
            }
            .valid()
        );
        assert!(
            !Rect {
                x: f64::NAN,
                y: 0.0,
                w: 1.0,
                h: 1.0
            }
            .valid()
        );
        assert!(
            !Rect {
                x: 0.0,
                y: 0.0,
                w: -1.0,
                h: 1.0
            }
            .valid()
        );
    }
}
