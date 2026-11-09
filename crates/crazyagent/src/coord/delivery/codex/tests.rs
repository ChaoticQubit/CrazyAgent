use super::*;

/// K2b: the bridge runs only for a Codex release it was checked against, read from the
/// app-server's `userAgent`; another release, a malformed agent or no version fails
/// closed so the engine types instead.
#[test]
fn only_a_checked_codex_release_may_use_the_bridge() {
    assert_eq!(VERSIONS, [(0, 164)]);
    for agent in [
        "codex_cli_rs/0.164.0 (Mac OS 15.6.0; arm64) iTerm.app/3.5.0",
        "crazyagent/0.164.3",
        "codex-tui/0.164.0-alpha.2 (Linux)",
    ] {
        assert!(supported(agent), "{agent}");
    }
    for agent in [
        "codex_cli_rs/0.165.0 (Mac OS 15.6.0; arm64)",
        "codex_cli_rs/0.163.9",
        "codex_cli_rs/1.164.0",
        "codex_cli_rs 0.164.0",
        "codex_cli_rs/0.164",
        "0.164.0",
        "",
    ] {
        assert!(!supported(agent), "{agent}");
    }
}

/// K2b: an agent's app-server socket sits in the shared daemon folder under the SHA-256 of
/// its canonical control socket path, short enough to bind, the same for a home reached
/// through a link, and none for a home that does not exist.
#[cfg(target_os = "macos")]
#[test]
fn an_agent_socket_is_named_by_its_canonical_control_path() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let socket = socket(&home).unwrap();
    let control = home.canonicalize().unwrap().join(CONTROL_SOCKET);
    let name = format!("{:x}", Sha256::digest(control.to_str().unwrap().as_bytes()));
    assert_eq!(
        socket,
        PathBuf::from(format!(
            "/private/tmp/codex-daemon-{}",
            rustix::process::getuid().as_raw()
        ))
        .join(name)
    );
    assert!(socket.to_str().unwrap().len() <= 103);
    let linked = root.path().join("linked");
    std::os::unix::fs::symlink(&home, &linked).unwrap();
    assert_eq!(super::socket(&linked), Some(socket));
    assert_eq!(super::socket(&root.path().join("missing")), None);
}

/// b19 P2 r2: the bridge keeps the turn id the app-server named only when it is plain,
/// printable ASCII of at most `TURN_ID_BYTES_MAX` bytes, as 0.164's UUIDs are; a missing,
/// empty, longer or unprintable id is kept as none, so the delivery names no turn.
#[test]
fn a_turn_id_is_kept_only_when_it_is_short_plain_text() {
    let uuid = "01a18507-26f8-7de2-8fbd-bcc304c9daf2";
    assert_eq!(turn_id(Some(uuid)), Some(uuid.to_owned()));
    let longest = "t".repeat(TURN_ID_BYTES_MAX);
    assert_eq!(turn_id(Some(&longest)), Some(longest.clone()));
    let longer = "t".repeat(TURN_ID_BYTES_MAX + 1);
    for id in [
        None,
        Some(""),
        Some(longer.as_str()),
        Some("turn 1"),
        Some("turn\n"),
        Some("tür"),
    ] {
        assert_eq!(turn_id(id), None, "{id:?}");
    }
}

/// SECAUDIT codex-bridge-oversized-server-ping-panics-team-runtime: a ping larger than a
/// control frame may be is refused as a broken bridge, with no panic in the engine.
#[cfg(target_os = "macos")]
#[test]
fn an_oversized_ping_from_the_socket_is_refused_without_a_panic() {
    use std::io::{Read, Write};
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = base.join("p");
    let listener = crate::spawn_gate::bind_unix_std(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let mut stream = crate::spawn_gate::accept_unix_std(&listener).unwrap();
        let mut request = [0u8; 4096];
        let _ = stream.read(&mut request);
        stream
            .write_all(b"HTTP/1.1 101 Switching Protocols\r\n\r\n")
            .unwrap();
        let mut frame = vec![0x89, 0x7f];
        frame.extend_from_slice(&1_048_577u64.to_be_bytes());
        frame.resize(frame.len() + 1_048_577, 0);
        let _ = stream.write_all(&frame);
        let mut rest = Vec::new();
        let _ = stream.read_to_end(&mut rest);
    });
    let answer = super::wire::deliver(&socket, "dummy", true);
    assert_eq!(answer, Err("codex_bridge_reply_too_large"));
    server.join().unwrap();
}

/// Serves one bridge socket that answers the upgrade and sends a ping of `length` bytes.
/// Takes the length. Returns what `deliver` answered and the payload of the pong the
/// server read back, none when no pong came.
#[cfg(target_os = "macos")]
fn ping_of(length: usize) -> (Result<Option<String>, &'static str>, Option<Vec<u8>>) {
    use std::io::{Read, Write};
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = base.join("p");
    let listener = crate::spawn_gate::bind_unix_std(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let mut stream = crate::spawn_gate::accept_unix_std(&listener).unwrap();
        let mut request = [0u8; 4096];
        let _ = stream.read(&mut request);
        stream
            .write_all(b"HTTP/1.1 101 Switching Protocols\r\n\r\n")
            .unwrap();
        let mut ping = vec![0x89];
        if length < 126 {
            ping.push(u8::try_from(length).unwrap());
        } else {
            ping.push(0x7e);
            ping.extend(u16::try_from(length).unwrap().to_be_bytes());
        }
        ping.resize(ping.len() + length, b'p');
        let _ = stream.write_all(&ping);
        loop {
            let mut head = [0u8; 2];
            stream.read_exact(&mut head).ok()?;
            let size = match head[1] & 0x7f {
                126 => {
                    let mut wide = [0u8; 2];
                    stream.read_exact(&mut wide).ok()?;
                    usize::from(u16::from_be_bytes(wide))
                }
                short => usize::from(short),
            };
            let mut mask = [0u8; 4];
            stream.read_exact(&mut mask).ok()?;
            let mut payload = vec![0u8; size];
            stream.read_exact(&mut payload).ok()?;
            if head[0] & 0x0f == 0xa {
                for (at, byte) in payload.iter_mut().enumerate() {
                    *byte ^= mask[at % 4];
                }
                return Some(payload);
            }
        }
    });
    let answer = super::wire::deliver(&socket, "dummy", true);
    (answer, server.join().unwrap())
}

/// SECAUDIT codex-bridge-oversized-server-ping-panics-team-runtime: a ping at the control
/// frame limit is answered with the same payload, and one byte more is refused by name.
#[cfg(target_os = "macos")]
#[test]
fn a_ping_at_the_control_limit_is_answered_and_one_byte_more_is_refused() {
    assert_eq!(
        ping_of(125),
        (Err("codex_bridge_closed"), Some(vec![b'p'; 125]))
    );
    assert_eq!(ping_of(126), (Err("codex_bridge_reply_too_large"), None));
}
