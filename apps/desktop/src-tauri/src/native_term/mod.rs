mod a11y;
mod clip;
mod codec;
mod control;
mod ended;
pub(crate) mod host;
mod ime;
mod keymap;
mod pane;
#[cfg(feature = "native-webdriver")]
pub(crate) mod probe;
mod render;
mod screen;
pub(crate) mod style;
mod table;
mod view;
mod view_text;

use crazyagent::terminal::{FOLDER, tmux::SOCKET};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

/// The longest agent window name a view may ask for, in bytes.
const NAME_BYTES_MAX: usize = crazyagent::terminal::AGENT_ID_BYTES_MAX;

/// Whether a socket path names an engine's own tmux server by its shape: absolute, plain
/// parts, `tmux.sock` in a `terminal` folder, never a default tmux socket folder. Input:
/// the path. Output: the answer.
pub fn engine_socket_shape(path: &Path) -> bool {
    let plain = path
        .components()
        .all(|part| matches!(part, Component::RootDir | Component::Normal(_)));
    let named = path.file_name() == Some(OsStr::new(SOCKET));
    let folder = path.parent().and_then(Path::file_name) == Some(OsStr::new(FOLDER));
    let text = path.to_string_lossy();
    path.is_absolute() && plain && named && folder && !text.contains("/tmux-")
}

/// The engine's own tmux socket under the engine root the desktop talks to (the folder of
/// `desktop_socket()`), checked: its shape, an `engine.db` file in the root, and a socket
/// owned by this user, read without following a link, so a sandboxed lead's or teammates'
/// server is never reached by a plain client. Input: the engine root. Output: the socket,
/// or a plain code that drops the view to engine-fed output.
pub fn engine_socket(root: &Path) -> Result<PathBuf, &'static str> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let path = root.join(FOLDER).join(SOCKET);
    if !root.is_absolute() || !engine_socket_shape(&path) {
        return Err("native_terminal_socket_invalid");
    }
    let engine = std::fs::symlink_metadata(root.join("engine.db")).is_ok_and(|m| m.is_file());
    if !engine {
        return Err("native_terminal_socket_not_engine");
    }
    let metadata =
        std::fs::symlink_metadata(&path).map_err(|_| "native_terminal_socket_missing")?;
    let user = rustix::process::getuid().as_raw();
    if !metadata.file_type().is_socket() || metadata.uid() != user {
        return Err("native_terminal_socket_invalid");
    }
    assert!(path.starts_with(root));
    assert!(engine_socket_shape(&path));
    Ok(path)
}

/// Whether the agent has its own tmux server, as a sandboxed lead does: any file at that
/// server's private socket path, read by `lstat` only and never connected to, since a tmux
/// server can make its client run a command. Inputs: the engine root and a checked window
/// name. Output: the answer, or a plain code when it cannot be told: only a clean "no such
/// file" means no server.
pub fn own_server(root: &Path, name: &str) -> Result<bool, &'static str> {
    assert!(root.is_absolute());
    let agent = crate::bridge::terminal::agent(name).map_err(|_| "native_terminal_name_invalid")?;
    let socket = crazyagent::terminal::harness_socket(root, &agent)
        .map_err(|_| "native_terminal_own_server_unknown")?;
    assert!(socket.is_absolute());
    assert!(!socket.starts_with(root.join(FOLDER)));
    match std::fs::symlink_metadata(socket) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("native_terminal_own_server_unknown"),
    }
}

/// Checks the agent window name a view asks for: a team agent id of plain letters, digits,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn focus_call(text: &str) -> Option<String> {
        let needles = [
            ["acti", "vate"].concat(),
            ["make", "KeyWindow"].concat(),
            ["make", "KeyAndOrderFront"].concat(),
            ["order", "FrontRegardless"].concat(),
        ];
        needles
            .into_iter()
            .find(|needle| text.contains(needle.as_str()))
    }

    #[test]
    fn the_focus_scan_finds_a_planted_call() {
        let planted = format!("window.{}{}();", "makeKey", "Window");
        assert_eq!(
            focus_call(&planted).as_deref(),
            Some(planted[7..20].as_ref())
        );
        let app = format!("app.{}IgnoringOtherApps(true);", ["acti", "vate"].concat());
        assert!(focus_call(&app).is_some());
        assert_eq!(focus_call("window.setFrame(rect);"), None);
    }

    #[test]
    fn no_native_file_brings_the_app_forward_or_makes_a_window_key() {
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/native_term");
        let mut read = 0;
        for entry in std::fs::read_dir(&folder).expect("native_term folder") {
            let path = entry.expect("folder entry").path();
            if path.extension() != Some(OsStr::new("rs")) {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("source file");
            let shown = path.display();
            assert_eq!(focus_call(&text), None, "{shown}");
            read += 1;
        }
        assert!(read >= 14, "read {read} files");
    }

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

    #[test]
    fn engine_socket_needs_an_engine_db_and_a_socket() {
        let root = tempfile::tempdir().expect("temp dir");
        let root = root.path();
        let socket = root.join(FOLDER).join(SOCKET);
        std::fs::create_dir(root.join(FOLDER)).expect("terminal folder");
        assert_eq!(
            engine_socket(Path::new("relative")),
            Err("native_terminal_socket_invalid")
        );
        assert_eq!(
            engine_socket(root),
            Err("native_terminal_socket_not_engine")
        );
        std::fs::write(root.join("engine.db"), b"").expect("engine.db");
        assert_eq!(engine_socket(root), Err("native_terminal_socket_missing"));
        std::fs::write(&socket, b"").expect("plain file");
        assert_eq!(engine_socket(root), Err("native_terminal_socket_invalid"));
        std::fs::remove_file(&socket).expect("remove");
        std::os::unix::fs::symlink(root.join("engine.db"), &socket).expect("link");
        assert_eq!(engine_socket(root), Err("native_terminal_socket_invalid"));
        std::fs::remove_file(&socket).expect("remove link");
        let _listener = crazyagent::spawn_gate::bind_unix_std(&socket).expect("bind");
        assert_eq!(engine_socket(root), Ok(socket.clone()));
    }

    #[test]
    fn an_agent_with_its_own_server_socket_is_refused() {
        let root = tempfile::tempdir().expect("temp dir");
        let root = root.path();
        assert_eq!(own_server(root, "team-1"), Ok(false));
        let agent = crate::bridge::terminal::agent("team-1").expect("agent");
        let socket = crazyagent::terminal::harness_socket(root, &agent).expect("socket path");
        let folder = socket.parent().expect("socket folder").to_path_buf();
        let made = !folder.exists();
        std::fs::create_dir_all(&folder).expect("folder");
        std::fs::write(&socket, b"").expect("plain file");
        assert_eq!(own_server(root, "team-1"), Ok(true));
        std::fs::remove_file(&socket).expect("remove");
        assert_eq!(own_server(root, "team-1"), Ok(false));
        if made {
            std::fs::remove_dir(&folder).expect("remove folder");
        }
    }

    #[test]
    fn window_names_are_plain_agent_ids() {
        assert_eq!(window_name("team-1").as_deref(), Ok("team-1"));
        assert!(window_name("").is_err());
        assert!(window_name("team-1;kill-server").is_err());
        assert!(window_name("a b").is_err());
        assert!(window_name(&"x".repeat(NAME_BYTES_MAX + 1)).is_err());
    }
}
