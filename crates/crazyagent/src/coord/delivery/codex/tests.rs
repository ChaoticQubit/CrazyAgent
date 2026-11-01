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
