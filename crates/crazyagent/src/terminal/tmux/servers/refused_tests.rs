use super::*;

/// A host on a fresh engine root, as an engine that started no server holds, and lead 7.
/// Output: the root folder, which removes the engine's files once dropped, the host and the
/// lead.
fn fresh_host() -> (tempfile::TempDir, Servers, AgentId) {
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
    (root, host, AgentId::team(7).unwrap())
}

/// Makes a private folder and any folder above it. Takes the folder.
fn private_folder(folder: &Path) {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(folder)
        .unwrap();
}

/// A lead on a fresh host whose start named its teammates' socket, with that socket's folder
/// made. Output: the root folder, the host, the lead and the socket.
fn lead_with_teammates_socket() -> (tempfile::TempDir, Servers, AgentId, PathBuf) {
    let (root, host, lead) = fresh_host();
    let record = crate::team::runtime::agent_teammates_socket_name(&host.root.join("team"), 7);
    private_folder(record.parent().unwrap());
    let socket = crate::terminal::renew_teammates_socket(&host.root, &lead).unwrap();
    assert_eq!(
        teammates_socket(&host.root, &lead),
        Ok(socket.clone()),
        "the host reads the teammates' socket the lead's start named"
    );
    private_folder(socket.parent().unwrap());
    (root, host, lead, socket)
}

/// Installs a lead's profile and makes its agent folder on a host, so the host opens the
/// lead's own server and lists it while its own socket is a socket file, and makes that
/// socket's folder. Takes the host and the lead's id. Returns the lead and its own socket.
fn own_lead(host: &Servers, id: i64) -> (AgentId, PathBuf) {
    use std::os::unix::fs::OpenOptionsExt;
    let lead = AgentId::team(id).unwrap();
    let team = host.root.join("team");
    let profile = crate::team::runtime::agent_profile(&team, id);
    private_folder(profile.parent().unwrap());
    private_folder(&crate::team::runtime::agent_home(&team, id));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&profile)
        .unwrap();
    let socket = harness_socket(&host.root, &lead).unwrap();
    private_folder(socket.parent().unwrap());
    (lead, socket)
}

/// Lead 7 on a fresh host with its own server set up as [`own_lead`] does. Output: the root
/// folder, the host, the lead and its own socket.
fn lead_with_own_socket() -> (tempfile::TempDir, Servers, AgentId, PathBuf) {
    let (root, host, _) = fresh_host();
    let (lead, socket) = own_lead(&host, 7);
    (root, host, lead, socket)
}

/// Starts a stand-in keeper that runs until it is killed. Output: the child.
fn running_keeper() -> std::process::Child {
    crate::spawn_gate::spawn(
        std::process::Command::new("/bin/sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap()
}

/// Whether a path is a socket file now, read without following a link. Takes the path.
/// Returns the answer.
fn socket_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|found| found.file_type().is_socket())
}

/// v242 (GB-7 row 4 class): a native lead's teammates' socket whose connect is refused
/// counts as no teammates' server, while this engine holds a keeper for that server, only
/// once that keeper exited cleanly: on macOS a live server whose accept queue is full also
/// refuses, and a teammate can stop the server it runs under. A refused one whose keeper
/// still runs or exited without confirming the end stays the lead's teammates' server, as
/// a listening one does even after its keeper exited. With none held, as for a server an
/// earlier engine started, any failed connect still counts as none, since that engine's
/// keeper ends it at its deadline; a keeper of the lead's own server is not read. No case
/// starts a server.
#[test]
fn a_refused_teammates_socket_is_no_server_only_once_its_held_keeper_ended_cleanly() {
    let (_root, host, lead, socket) = lead_with_teammates_socket();
    let found = || host.mates(&lead).map(|server| server.is_some());

    let listener = crate::spawn_gate::bind_unix_std(&socket).unwrap();
    let stays = found();
    assert_ne!(
        stays,
        Ok(false),
        "a listening teammates' socket was taken as no server"
    );
    drop(listener);
    assert!(socket_file(&socket), "the ended server's socket file stays");
    assert_eq!(
        found(),
        Ok(false),
        "a refused teammates' socket this engine holds no keeper for was kept"
    );

    host.keepers
        .lock()
        .unwrap()
        .insert(lead.clone(), running_keeper());
    assert_eq!(
        found(),
        Ok(false),
        "the keeper of the lead's own server was read for its teammates' server"
    );
    let mut own = host.keepers.lock().unwrap().remove(&lead).unwrap();
    own.kill().unwrap();
    own.wait().unwrap();

    host.mates_keepers
        .lock()
        .unwrap()
        .insert(lead.clone(), running_keeper());
    assert_eq!(
        found(),
        stays,
        "a refused teammates' socket whose keeper still runs was taken as no server"
    );
    let mut keeper = host.mates_keepers.lock().unwrap().remove(&lead).unwrap();
    keeper.kill().unwrap();
    assert!(!keeper.wait().unwrap().success());
    host.mates_keepers
        .lock()
        .unwrap()
        .insert(lead.clone(), keeper);
    assert_eq!(
        found(),
        stays,
        "a refused teammates' socket whose keeper exited without confirming the end was taken as no server"
    );

    let mut keeper = crate::spawn_gate::spawn(
        std::process::Command::new("/usr/bin/true")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    assert!(keeper.wait().unwrap().success());
    host.mates_keepers
        .lock()
        .unwrap()
        .insert(lead.clone(), keeper);
    std::fs::remove_file(&socket).unwrap();
    let listener = crate::spawn_gate::bind_unix_std(&socket).unwrap();
    assert_eq!(
        found(),
        stays,
        "a listening teammates' socket was taken as no server once its keeper exited"
    );
    drop(listener);
    assert!(socket_file(&socket), "the ended server's socket file stays");
    assert_eq!(
        found(),
        Ok(false),
        "a refused teammates' socket whose keeper ended cleanly was kept"
    );
    std::fs::remove_dir_all(socket.parent().unwrap()).unwrap();
}

/// v242: a native lead an earlier engine started, whose teammates' server ended while this
/// engine holds no keeper for it, still ends here. A kill of the lead finds no server and
/// gives `terminal_agent_unknown`, which the next engine's thaw of a refused frozen lead
/// (`TeamRuntime::attach_terminal`) and its sweep both take as ended, rather than failing on
/// the ended server with a code they both return as the engine's failure. No case starts a
/// server.
#[test]
fn an_earlier_engines_lead_whose_teammates_server_ended_is_killed_as_unknown() {
    let (_root, host, lead, socket) = lead_with_teammates_socket();
    let listener = crate::spawn_gate::bind_unix_std(&socket).unwrap();
    drop(listener);
    assert!(socket_file(&socket), "the ended server's socket file stays");
    assert!(
        host.mates_keepers.lock().unwrap().is_empty(),
        "this engine holds no teammates' keeper"
    );
    assert_eq!(
        host.kill(&lead),
        Err(failure("terminal_agent_unknown")),
        "the earlier engine's lead with an ended teammates' server did not end as unknown"
    );
    assert!(
        !host
            .root
            .join(crate::terminal::FOLDER)
            .join(super::super::SOCKET)
            .exists(),
        "the kill started the engine's server"
    );
    std::fs::remove_dir_all(socket.parent().unwrap()).unwrap();
}

/// v242 (gb67 flag 23:05): a listing of the live agents that finds a lead's own socket
/// refused unlinks it, and on macOS a live server whose accept queue is full refuses too
/// while a lead can stop the server it runs under, so a gone socket proves no end either.
/// While this engine holds the keeper of that server and it has not exited cleanly, the
/// lead's own server is `terminal_own_server_unavailable`, so a call on the lead fails
/// rather than finding it gone, also after another lead's server was opened; once the keeper
/// ended cleanly, or with no keeper held, a gone socket is no own server. No case starts a
/// server.
#[test]
fn an_unlinked_own_socket_is_no_server_only_once_its_held_keeper_ended_cleanly() {
    let (_root, host, lead, socket) = lead_with_own_socket();
    let found = || host.own(&lead).map(|server| server.is_some());
    let unavailable = || failure(OWN_SERVER_UNAVAILABLE);

    let listener = crate::spawn_gate::bind_unix_std(&socket).unwrap();
    assert_eq!(
        host.leads_listening(),
        vec![lead.clone()],
        "the host does not list the lead"
    );
    assert_eq!(
        found(),
        Ok(true),
        "a listening own socket is not the lead's own server"
    );
    drop(listener);
    host.keepers
        .lock()
        .unwrap()
        .insert(lead.clone(), running_keeper());
    assert_eq!(
        found(),
        Ok(true),
        "a refused own socket whose keeper still runs was taken as no server"
    );
    assert_eq!(
        host.alive().map(|alive| alive.contains(&lead)),
        Ok(false),
        "a refused own server was listed as running its lead"
    );
    assert!(
        !socket_file(&socket),
        "the listing kept the refused own socket, so the row reaches no unlinked one"
    );
    assert_eq!(
        found(),
        Err(unavailable()),
        "an unlinked own socket whose keeper still runs was taken as no server"
    );
    assert_eq!(
        host.pane_pid(&lead),
        Err(unavailable()),
        "the lead of an unlinked own socket whose keeper still runs was found gone"
    );
    let (other, other_socket) = own_lead(&host, 8);
    let other_listener = crate::spawn_gate::bind_unix_std(&other_socket).unwrap();
    assert_eq!(
        host.own(&other).map(|server| server.is_some()),
        Ok(true),
        "another lead's listening own socket is not its own server"
    );
    assert_eq!(
        found(),
        Err(unavailable()),
        "opening another lead's server dropped the host of an unlinked own socket whose keeper still runs"
    );
    drop(other_listener);
    std::fs::remove_dir_all(other_socket.parent().unwrap()).unwrap();

    let mut keeper = host.keepers.lock().unwrap().remove(&lead).unwrap();
    keeper.kill().unwrap();
    assert!(!keeper.wait().unwrap().success());
    host.keepers.lock().unwrap().insert(lead.clone(), keeper);
    assert_eq!(
        found(),
        Err(unavailable()),
        "an unlinked own socket whose keeper exited without confirming the end was taken as no server"
    );

    let mut keeper = crate::spawn_gate::spawn(
        std::process::Command::new("/usr/bin/true")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    assert!(keeper.wait().unwrap().success());
    host.keepers.lock().unwrap().insert(lead.clone(), keeper);
    assert_eq!(
        host.pane_pid(&lead),
        Err(failure("terminal_agent_unknown")),
        "the lead of an unlinked own socket whose keeper ended cleanly was not found gone"
    );
    host.keepers.lock().unwrap().remove(&lead);
    assert_eq!(
        found(),
        Ok(false),
        "an unlinked own socket this engine holds no keeper for was kept"
    );
    assert!(
        !host
            .root
            .join(crate::terminal::FOLDER)
            .join(super::super::SOCKET)
            .exists(),
        "a call started the engine's server"
    );
    std::fs::remove_dir_all(socket.parent().unwrap()).unwrap();
}
