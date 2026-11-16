use super::*;

/// GB-7 row 4 (2.4.1 gate): a lead's own socket that its ended server left behind, where a
/// connect is refused, counts as no own server once the keeper this engine started for that
/// server has exited, so a call on the lead routes on and finds it gone. A listening socket,
/// a refused one whose keeper still runs, as a live server with a full accept queue also
/// refuses on macOS, and a refused one this engine holds no keeper for all stay the lead's
/// own server, so a call on them fails rather than finding the lead gone. No case starts a
/// server.
#[test]
fn a_lead_socket_whose_keeper_exited_and_whose_connect_is_refused_routes_as_no_own_server() {
    use std::os::unix::fs::DirBuilderExt;
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let program = PathBuf::from("/usr/bin/false");
    let engine = base.join(crate::terminal::FOLDER);
    let host = Servers::new(
        Tmux::open(Some(program.clone()), &engine),
        &base,
        Some(program),
    );
    assert_eq!(
        host.root, base,
        "the host keeps the root the row names sockets by"
    );
    let lead = AgentId::team(7).unwrap();
    let socket = harness_socket(&base, &lead).unwrap();
    let folder = socket.parent().unwrap().to_path_buf();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&folder)
        .unwrap();
    let own = || failure(OWN_SERVER_UNAVAILABLE);
    let gone = || failure("terminal_agent_unknown");

    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    assert_eq!(
        host.pane_pid(&lead),
        Err(own()),
        "a listening own socket is still the lead's own server"
    );
    drop(listener);
    assert!(
        std::fs::symlink_metadata(&socket)
            .unwrap()
            .file_type()
            .is_socket(),
        "the ended server's socket file stays"
    );
    assert_eq!(
        host.pane_pid(&lead),
        Err(own()),
        "a refused socket this engine holds no keeper for was taken as gone"
    );

    let keeper = crate::spawn_gate::spawn(
        std::process::Command::new("/bin/sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    host.keepers.lock().unwrap().insert(lead.clone(), keeper);
    assert_eq!(
        host.pane_pid(&lead),
        Err(own()),
        "a refused socket whose keeper still runs was taken as gone"
    );

    let mut keeper = host.keepers.lock().unwrap().remove(&lead).unwrap();
    keeper.kill().unwrap();
    assert!(!keeper.wait().unwrap().success());
    host.keepers.lock().unwrap().insert(lead.clone(), keeper);
    assert_eq!(
        host.pane_pid(&lead),
        Err(gone()),
        "the dead own server was asked for the lead's pane"
    );
    assert_eq!(
        host.kill(&lead),
        Err(gone()),
        "the dead own server was asked to end the lead"
    );
    assert!(
        !engine.join(super::super::SOCKET).exists(),
        "a call started the engine's server"
    );
    std::fs::remove_dir_all(&folder).unwrap();
}
