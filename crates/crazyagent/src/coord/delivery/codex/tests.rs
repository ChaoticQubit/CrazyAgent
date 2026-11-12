use super::*;

/// K2b, F38 (BL-CODEX167): the bridge reads the Codex release from the app-server's
/// `userAgent`. A release it was checked against runs as checked; a later minor of the same
/// major runs with a warning instead of failing closed (Codex 0.167.1 today); an earlier
/// release, another major, a malformed agent or no version still fails closed, so the
/// engine types instead.
#[test]
fn a_newer_codex_release_runs_the_bridge_with_a_warning() {
    assert_eq!(VERSIONS, [(0, 164)]);
    for agent in [
        "codex_cli_rs/0.164.0 (Mac OS 15.6.0; arm64) iTerm.app/3.5.0",
        "crazyagent/0.164.3",
        "codex-tui/0.164.0-alpha.2 (Linux)",
    ] {
        assert_eq!(release(agent), Support::Checked, "{agent}");
    }
    for (agent, newer) in [
        ("codex_cli_rs/0.165.0 (Mac OS 15.6.0; arm64)", (0, 165, 0)),
        (
            "codex_cli_rs/0.167.1 (Mac OS 26.0.0; arm64) iTerm.app/3.5.0",
            (0, 167, 1),
        ),
        ("codex-tui/0.168.0-alpha.2 (Linux)", (0, 168, 0)),
    ] {
        assert_eq!(release(agent), Support::Newer(newer), "{agent}");
    }
    for agent in [
        "codex_cli_rs/0.163.9",
        "codex_cli_rs/1.164.0",
        "codex_cli_rs/1.0.0",
        "codex_cli_rs 0.164.0",
        "codex_cli_rs/0.164",
        "0.164.0",
        "",
    ] {
        assert_eq!(release(agent), Support::Refused, "{agent}");
    }
}

/// F38: a release listed as known-bad stays refused even where the range rule would let it
/// run, checked or newer, and no checked release at all refuses everything. The shipped
/// list names none yet; the public rule is the one the bridge uses.
#[test]
fn a_known_bad_codex_release_is_refused_whatever_the_range_says() {
    assert!(REFUSED.is_empty());
    let checked = [(0, 164)];
    let refused = [(0, 164, 2), (0, 167, 1)];
    assert_eq!(judged((0, 164, 1), &checked, &refused), Support::Checked);
    assert_eq!(judged((0, 164, 2), &checked, &refused), Support::Refused);
    assert_eq!(
        judged((0, 167, 0), &checked, &refused),
        Support::Newer((0, 167, 0))
    );
    assert_eq!(judged((0, 167, 1), &checked, &refused), Support::Refused);
    assert_eq!(judged((0, 163, 9), &checked, &refused), Support::Refused);
    assert_eq!(judged((1, 0, 0), &checked, &refused), Support::Refused);
    assert_eq!(judged((0, 167, 1), &[], &[]), Support::Refused);
    assert_eq!(support((0, 167, 1)), Support::Newer((0, 167, 1)));
    assert_eq!(support((0, 164, 0)), Support::Checked);
    assert_eq!(support((0, 163, 0)), Support::Refused);
}

/// F38: the warning a newer release leaves in the agent's Messages timeline names only the
/// parsed release numbers and the newest checked one, in plain words; no text from the
/// agent's own process reaches it.
#[test]
fn the_newer_release_warning_names_the_numbers_only() {
    assert_eq!(
        newer_notice((0, 167, 1)),
        "Codex 0.167.1 is newer than the Codex CrazyAgent was checked with (0.164). Messages still go straight to this agent; if it stops getting them, update CrazyAgent."
    );
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

/// PA1213R2-F2: the engine counts a Codex agent's pinned hooks as trusted only when the
/// app-server's `hooks/list` lists every pinned key as trusted; a modified, untrusted or
/// unlisted key, or an answer of another shape, reads as not trusted.
#[test]
fn pinned_hooks_count_as_trusted_only_when_codex_lists_each_one_trusted() {
    let guard = "/h/config.toml:pre_tool_use:0:0".to_owned();
    let owner = "/h/config.toml:session_start:0:0".to_owned();
    let listing = |guard_status: &str, owner_status: &str| {
        serde_json::json!({"data": [
            {"cwd": "/w", "warnings": [], "errors": [], "hooks": [
                {"key": guard, "eventName": "preToolUse", "trustStatus": guard_status},
                {"key": "/w/.codex/hooks.json:pre_tool_use:0:0", "trustStatus": "untrusted"},
            ]},
            {"cwd": "/v", "warnings": [], "errors": [], "hooks": [
                {"key": owner, "eventName": "sessionStart", "trustStatus": owner_status},
            ]},
        ]})
    };
    let both = [guard.clone(), owner.clone()];
    assert!(all_trusted(&listing("trusted", "trusted"), &both));
    assert!(all_trusted(&listing("Trusted", "trusted"), &both));
    for (guard_status, owner_status) in [
        ("modified", "trusted"),
        ("trusted", "untrusted"),
        ("managed", "trusted"),
    ] {
        assert!(
            !all_trusted(&listing(guard_status, owner_status), &both),
            "{guard_status} {owner_status}"
        );
    }
    let unlisted = [guard.clone(), "/h/config.toml:stop:0:0".to_owned()];
    assert!(!all_trusted(&listing("trusted", "trusted"), &unlisted));
    for odd in [
        serde_json::json!({}),
        serde_json::json!({"data": {"hooks": []}}),
        serde_json::json!({"data": [{"hooks": [{"key": guard, "trustStatus": 1}]}]}),
    ] {
        assert!(!all_trusted(&odd, std::slice::from_ref(&guard)), "{odd}");
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
        let mut stream = accepted(&listener);
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
    let answer = super::wire::deliver(&socket, "dummy", true, own_group());
    assert_eq!(answer, Err("codex_bridge_reply_too_large"));
    server.join().unwrap();
}

/// Serves one bridge socket that answers the upgrade and sends a ping of `length` bytes.
/// Takes the length. Returns what `deliver` answered and the payload of the pong the
/// server read back, none when no pong came.
#[cfg(target_os = "macos")]
#[allow(clippy::type_complexity)]
fn ping_of(
    length: usize,
) -> (
    Result<(Option<String>, Option<(u64, u64, u64)>), &'static str>,
    Option<Vec<u8>>,
) {
    use std::io::{Read, Write};
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = base.join("p");
    let listener = crate::spawn_gate::bind_unix_std(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let mut stream = accepted(&listener);
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
    let answer = super::wire::deliver(&socket, "dummy", true, own_group());
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

/// G4 F2: a bridge that answers the upgrade with a ping and closes at once is named closed
/// every time, whether the read meets its end first or a timeout is set on the socket after
/// its peer went, which macOS refuses with EINVAL.
#[cfg(target_os = "macos")]
#[test]
fn a_bridge_that_closes_right_after_its_answer_is_always_named_closed() {
    use std::io::{Read, Write};
    use std::os::unix::fs::PermissionsExt;
    for _ in 0..20 {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = base.join("p");
        let listener = crate::spawn_gate::bind_unix_std(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let mut stream = accepted(&listener);
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let mut answer = b"HTTP/1.1 101 Switching Protocols\r\n\r\n".to_vec();
            answer.extend([0x89, 0x01, b'p']);
            stream.write_all(&answer).unwrap();
        });
        let answer = super::wire::deliver(&socket, "dummy", true, own_group());
        server.join().unwrap();
        assert_eq!(answer, Err("codex_bridge_closed"));
    }
}

/// A pane's script that starts a stand-in app-server in its background, as the bridge
/// script starts the real one. The stand-in listens on the socket named by `$1`, takes two
/// connections in turn and writes the length of each one's first read to `$1.got`.
#[cfg(target_os = "macos")]
const BACKGROUND_LISTENER: &str = r#"/usr/bin/perl -MIO::Socket::UNIX -e '
alarm 30;
my $listener = IO::Socket::UNIX->new(Local => $ARGV[0], Listen => 2) or die;
open(my $got, ">", "$ARGV[0].got") or die;
$got->autoflush(1);
for (1, 2) {
    my $peer = $listener->accept or die;
    my $size = sysread($peer, my $first, 4096);
    print $got (($size // 0), "\n");
    close $peer;
}
' "$1" </dev/null >/dev/null 2>&1 &
wait
"#;

/// The process group this test process runs in, the one a listener it binds is seen in.
#[cfg(target_os = "macos")]
fn own_group() -> u32 {
    u32::try_from(rustix::process::getpgrp().as_raw_nonzero().get()).unwrap()
}

/// SECAUDIT codex-bridge-socket-check-then-connect-follows-swapped-link: an agent whose
/// pane cannot be read is not reached through the bridge at all.
#[cfg(target_os = "macos")]
#[test]
fn an_agent_with_no_pane_to_compare_is_not_reached_through_its_bridge() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        deliver(home.path(), "dummy", true, None),
        Bridged::Unavailable("codex_bridge_peer_unknown")
    );
}

/// SECAUDIT codex-bridge-socket-check-then-connect-follows-swapped-link: a real pane's
/// background listener passes for that pane, though its own pid is not the pane's, and is
/// refused with no byte sent when compared with another agent's pane, the redirect a
/// swapped link makes.
#[cfg(target_os = "macos")]
#[test]
fn a_bridge_is_used_only_when_its_listener_runs_in_the_agents_pane() {
    use crate::terminal::Host;
    let mut fixture = super::super::tests::Fixture::new();
    let host = fixture.host();
    let owner = fixture.agent("sub", "codex", "idle", None);
    let other = fixture.agent("sub", "codex", "idle", None);
    let socket = fixture.base.join("p");
    let got = fixture.base.join("p.got");
    fixture.pane(&host, owner, BACKGROUND_LISTENER, &socket);
    fixture.pane(
        &host,
        other,
        "exec /bin/sleep 60\n",
        &fixture.base.join("other"),
    );
    super::super::tests::until("the stand-in listening", || got.exists());
    let pane_of = |agent| {
        host.pane_pid(&crate::terminal::AgentId::team(agent).unwrap())
            .unwrap()
            .unwrap()
    };
    assert_eq!(
        super::wire::deliver(&socket, "dummy", true, pane_of(other)),
        Err("codex_bridge_peer_unknown")
    );
    assert_eq!(
        super::wire::deliver(&socket, "dummy", true, pane_of(owner)),
        Err("codex_bridge_closed")
    );
    super::super::tests::until("both connections read", || {
        std::fs::read_to_string(&got).is_ok_and(|text| text.lines().count() == 2)
    });
    let sizes: Vec<usize> = std::fs::read_to_string(&got)
        .unwrap()
        .lines()
        .map(|line| line.parse().unwrap())
        .collect();
    assert_eq!(sizes[0], 0, "the refused connection got no byte");
    assert!(sizes[1] > 0, "the accepted connection got the upgrade");
}

/// Takes the one connection a peer makes, polling a non-blocking listener so the spawn gate
/// is never held across a wait: a spawn queued behind a held gate would stop the peer's own
/// connect. Takes the listener. Returns the stream, blocking again.
#[cfg(target_os = "macos")]
fn accepted(listener: &std::os::unix::net::UnixListener) -> std::os::unix::net::UnixStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        match crate::spawn_gate::accept_unix_std(listener) {
            Ok(stream) => {
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(std::time::Instant::now() < deadline, "no connection came");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(error) => panic!("{error}"),
        }
    }
}
