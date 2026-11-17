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

/// Has a lead's start on a host name its teammates' socket, and makes that socket's folder.
/// Takes the host and the lead's id. Returns the lead and the socket.
fn teammates_lead(host: &Servers, id: i64) -> (AgentId, PathBuf) {
    let lead = AgentId::team(id).unwrap();
    let record = crate::team::runtime::agent_teammates_socket_name(&host.root.join("team"), id);
    private_folder(record.parent().unwrap());
    let socket = crate::terminal::renew_teammates_socket(&host.root, &lead).unwrap();
    assert_eq!(
        teammates_socket(&host.root, &lead),
        Ok(socket.clone()),
        "the host reads the teammates' socket the lead's start named"
    );
    private_folder(socket.parent().unwrap());
    (lead, socket)
}

/// Lead 7 on a fresh host whose start named its teammates' socket, as [`teammates_lead`]
/// does. Output: the root folder, the host, the lead and the socket.
fn lead_with_teammates_socket() -> (tempfile::TempDir, Servers, AgentId, PathBuf) {
    let (root, host, _) = fresh_host();
    let (lead, socket) = teammates_lead(&host, 7);
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

/// Starts a stand-in keeper and waits for it to exit. Takes its program. Output: the child,
/// already reaped.
fn exited_keeper(program: &str) -> std::process::Child {
    let mut keeper = crate::spawn_gate::spawn(
        std::process::Command::new(program)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    keeper.wait().unwrap();
    keeper
}

/// A host on a fresh engine root whose server program is a stand-in that marks that it ran
/// and then runs no server for 3 s, so a start waits for its socket until its keeper exits,
/// and lead `id` set up as [`own_lead`] does, with both its profiles allowing everything.
/// Takes the lead's id. Output: the root folder, the host, the lead, its own socket and the
/// mark the program leaves.
fn marking_host(id: i64) -> (tempfile::TempDir, Servers, AgentId, PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let (root, base_host, _) = fresh_host();
    let base = base_host.root.clone();
    drop(base_host);
    let marks = base.join("marks");
    private_folder(&marks);
    let started = marks.join("started");
    let program = base.join("never-binds");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\n: > '{}'\nexec /bin/sleep 3\n",
            started.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let host = Servers::new(
        Tmux::open(Some(program.clone()), &base.join(crate::terminal::FOLDER)),
        &base,
        Some(program),
    );
    let (lead, socket) = own_lead(&host, id);
    let team = base.join("team");
    for profile in [
        crate::team::runtime::agent_profile(&team, id),
        crate::team::runtime::agent_teammates_profile(&team, id),
    ] {
        std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    }
    (root, host, lead, socket, started)
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

/// v242 MR-1 (rv-code2): a lead's server start, which waits up to `KEEPER_READY` for its
/// socket to accept, holds neither keeper map while it waits, so a call on another lead,
/// which reads those maps in `own()` and `mates()`, never waits on that start. A stand-in
/// server program that never binds its socket keeps each start waiting until its keeper
/// exits, and the row reads the map that start fills while the start still waits.
#[test]
fn a_lead_server_start_waiting_for_its_socket_holds_no_keeper_map() {
    let (_root, host, lead, socket, started) = marking_host(7);
    for mates in [false, true] {
        let target = if mates {
            socket.with_file_name("row-teammates.sock")
        } else {
            socket.clone()
        };
        let _ = std::fs::remove_file(&started);
        std::thread::scope(|scope| {
            let start = scope.spawn(|| host.keep_server(&lead, &target, mates, false));
            let deadline = Instant::now() + Duration::from_secs(10);
            while !started.exists() {
                assert!(
                    !start.is_finished(),
                    "the start ended before its server program ran"
                );
                assert!(
                    Instant::now() < deadline,
                    "the stand-in server program never ran"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            let kept = if mates {
                &host.mates_keepers
            } else {
                &host.keepers
            };
            let free = kept.try_lock().is_ok();
            assert!(
                !start.is_finished(),
                "the start ended before the row read its keeper map"
            );
            assert!(
                free,
                "a start waiting for its socket held the keeper map (teammates: {mates})"
            );
            assert_eq!(
                start.join().unwrap(),
                Err(failure("terminal_unavailable")),
                "a start whose server never bound its socket did not fail"
            );
        });
    }
    std::fs::remove_dir_all(socket.parent().unwrap()).unwrap();
}

/// v242 (rv-sec2 LOW): a lead server start keeps every other lead's keeper, also one that
/// exited cleanly, so that lead's own server is still read through its keeper: a refused own
/// socket whose keeper ended cleanly stays no own server rather than falling back to the
/// socket file and becoming the lead's own again. The start here fails, since its server
/// program runs no server.
#[test]
fn a_lead_server_start_keeps_another_leads_cleanly_exited_keeper() {
    let (_root, host, _) = fresh_host();
    let (starting, starting_socket) = own_lead(&host, 7);
    let (ended, ended_socket) = own_lead(&host, 8);
    let found = || host.own(&ended).map(|server| server.is_some());
    drop(crate::spawn_gate::bind_unix_std(&ended_socket).unwrap());
    assert!(
        socket_file(&ended_socket),
        "the ended server's socket file stays"
    );
    let mut keeper = crate::spawn_gate::spawn(
        std::process::Command::new("/usr/bin/true")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    assert!(keeper.wait().unwrap().success());
    host.keepers.lock().unwrap().insert(ended.clone(), keeper);
    assert_eq!(
        found(),
        Ok(false),
        "a refused own socket whose keeper ended cleanly was the lead's own server"
    );
    assert_eq!(
        host.keep_server(&starting, &starting_socket, false, false),
        Err(failure("terminal_unavailable")),
        "a start whose server program runs no server did not fail"
    );
    assert!(
        host.keepers.lock().unwrap().contains_key(&ended),
        "a start dropped another lead's cleanly exited keeper"
    );
    assert_eq!(
        found(),
        Ok(false),
        "another lead's start made a lead's ended own server its own again"
    );
    for socket in [&starting_socket, &ended_socket] {
        std::fs::remove_dir_all(socket.parent().unwrap()).unwrap();
    }
}

/// v242 MRR2-1 (rv-code2 r2): a kill of a native lead takes its teammates' keeper out of this
/// engine's map whatever the kill answers, as the lead's end takes its own keeper out
/// (`end_kept_server`), so a keeper that exited without confirming the end, which no later
/// start replaces once its lead ended, never stays for the engine's life. Here the kill fails
/// before it closes a server, since the lead's profiles are not installed, at the refused
/// socket an ended teammates' server left: a keeper that exited without confirming and one
/// the kill ends both leave the map, and a later kill finds the lead gone, as with no keeper
/// held. No case starts a server.
#[test]
fn a_killed_leads_teammates_keeper_leaves_the_map_whatever_the_kill_answers() {
    let (_root, host, lead, socket) = lead_with_teammates_socket();
    drop(crate::spawn_gate::bind_unix_std(&socket).unwrap());
    assert!(socket_file(&socket), "the ended server's socket file stays");
    let mut exited = exited_keeper("/usr/bin/false");
    assert!(!exited.wait().unwrap().success());
    for keeper in [exited, running_keeper()] {
        host.mates_keepers
            .lock()
            .unwrap()
            .insert(lead.clone(), keeper);
        assert_eq!(
            host.kill(&lead),
            Err(failure(OWN_SERVER_UNAVAILABLE)),
            "the kill of a lead whose profiles are not installed answered otherwise"
        );
        assert!(
            !host.mates_keepers.lock().unwrap().contains_key(&lead),
            "a killed lead's teammates' keeper stayed in the map"
        );
    }
    assert_eq!(
        host.kill(&lead),
        Err(failure("terminal_agent_unknown")),
        "a killed lead whose teammates' keeper left the map was not found gone"
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

/// v242 MRR2-1 (rv-code2 r2): a 257th lead's teammates' server still starts after 256 leads
/// ended whose teammates' keepers all exited without confirming the end, which no later start
/// replaces, since an ended lead's id does not start again. First those leads were killed, as
/// a lead's end kills it, at the refused sockets their ended servers left; then, with no
/// kill, their project was deleted with the records naming their teammates' sockets, so this
/// engine reads them as no server whatever keeper it holds and a full map drops those
/// keepers. Each start runs its server program, a stand-in that marks that it ran and runs no
/// server, so the start still fails.
#[test]
fn a_teammates_server_start_after_256_ended_leads_still_runs() {
    let (_root, host, lead, socket, started) = marking_host(300);
    let target = socket.with_file_name("row-teammates.sock");
    let count = i64::try_from(LEADS_KEPT_MAX).unwrap();
    let ended_leads: Vec<(AgentId, PathBuf)> =
        (1..=count).map(|id| teammates_lead(&host, id)).collect();
    let fill = || {
        let mut keepers = host.mates_keepers.lock().unwrap();
        assert!(keepers.is_empty(), "the map held a keeper before the row");
        for (ended, _) in &ended_leads {
            let mut keeper = exited_keeper("/usr/bin/false");
            assert!(!keeper.wait().unwrap().success());
            keepers.insert(ended.clone(), keeper);
        }
        assert_eq!(keepers.len(), LEADS_KEPT_MAX, "the map is not full");
    };
    let clear = || {
        for (_, mut keeper) in std::mem::take(&mut *host.mates_keepers.lock().unwrap()) {
            if keeper_runs(&mut keeper) {
                let _ = keeper.kill();
            }
            let _ = keeper.wait();
        }
    };
    for killed in [true, false] {
        if killed {
            for (ended, ended_socket) in &ended_leads {
                drop(crate::spawn_gate::bind_unix_std(ended_socket).unwrap());
                assert!(
                    socket_file(ended_socket),
                    "the ended server's socket file stays"
                );
                assert_eq!(
                    teammates_socket(&host.root, ended),
                    Ok(ended_socket.clone()),
                    "the host reads another teammates' socket than the ended lead's start named"
                );
            }
            fill();
            for (ended, _) in &ended_leads {
                let _ = host.kill(ended);
            }
        } else {
            for (ended, ended_socket) in &ended_leads {
                std::fs::remove_file(crate::team::runtime::agent_teammates_socket_name(
                    &host.root.join("team"),
                    ended.team_agent().unwrap(),
                ))
                .unwrap();
                std::fs::remove_dir_all(ended_socket.parent().unwrap()).unwrap();
                assert!(teammates_socket(&host.root, ended).is_err());
            }
            fill();
        }
        let _ = std::fs::remove_file(&started);
        assert_eq!(
            host.keep_server(&lead, &target, true, false),
            Err(failure("terminal_unavailable")),
            "a start whose server program runs no server did not fail"
        );
        assert!(
            started.exists(),
            "a teammates' server start after 256 ended leads did not run (killed: {killed})"
        );
        assert!(
            host.mates_keepers.lock().unwrap().len() < LEADS_KEPT_MAX,
            "the teammates' keepers of 256 ended leads stayed (killed: {killed})"
        );
        clear();
    }
    std::fs::remove_dir_all(socket.parent().unwrap()).unwrap();
}

/// v242 SEC2-R2 F1: a Stop of an adopted teammate whose teammates' server missed one look,
/// while its lead's own server listens, looks at that server again before it answers
/// `terminal_agent_unknown`, which a Stop takes as ended, so one failed connect never marks
/// a running teammate stopped. Both looks are armed to miss, so the kill still answers
/// unknown, as for a server that really ended. No case starts a server.
#[test]
fn a_stop_of_a_teammate_whose_teammates_server_missed_one_look_looks_again() {
    let (_root, host, lead, own_socket) = lead_with_own_socket();
    let (_, mates_socket) = teammates_lead(&host, 7);
    let _own = crate::spawn_gate::bind_unix_std(&own_socket).unwrap();
    let _mates = crate::spawn_gate::bind_unix_std(&mates_socket).unwrap();
    let ann = AgentId::team(8).unwrap();
    host.remember(&ann, &lead).unwrap();
    MATES_MISSES.set(2);

    let killed = host.kill(&ann);

    assert_eq!(
        MATES_MISSES.get(),
        0,
        "the Stop answered on one look at the teammates' server"
    );
    assert_eq!(killed, Err(failure("terminal_agent_unknown")));
    assert!(
        !host
            .root
            .join(crate::terminal::FOLDER)
            .join(super::super::SOCKET)
            .exists(),
        "the kill started the engine's server"
    );
    for folder in BTreeSet::from([own_socket.parent().unwrap(), mates_socket.parent().unwrap()]) {
        std::fs::remove_dir_all(folder).unwrap();
    }
}

/// v242 check-a-open sec r1 F1: once a remembered teammate's teammates' server missed a look,
/// its Stop looks again only at its own lead's teammates' server, never at another lead's own
/// server, where that lead may tag a decoy pane with the teammate's name so the Stop ends the
/// decoy and answers Ok. Lead 9 has no teammates' server and its own socket listens; the Stop
/// never opens it. No case starts a server.
#[test]
fn a_stop_of_a_remembered_teammate_after_a_missed_look_never_opens_another_leads_own_server() {
    let (_root, host, lead, own_socket) = lead_with_own_socket();
    let (_, mates_socket) = teammates_lead(&host, 7);
    let (other, other_socket) = own_lead(&host, 9);
    let _own = crate::spawn_gate::bind_unix_std(&own_socket).unwrap();
    let _mates = crate::spawn_gate::bind_unix_std(&mates_socket).unwrap();
    let _other = crate::spawn_gate::bind_unix_std(&other_socket).unwrap();
    let ann = AgentId::team(8).unwrap();
    host.remember(&ann, &lead).unwrap();
    MATES_MISSES.set(2);

    let killed = host.kill(&ann);

    assert!(
        !host.leads.lock().unwrap().contains_key(&other),
        "the Stop opened another lead's own server"
    );
    assert_eq!(killed, Err(failure("terminal_agent_unknown")));
    assert_eq!(
        MATES_MISSES.get(),
        0,
        "the Stop answered on one look at the teammates' server"
    );
    for folder in BTreeSet::from([
        own_socket.parent().unwrap(),
        mates_socket.parent().unwrap(),
        other_socket.parent().unwrap(),
    ]) {
        std::fs::remove_dir_all(folder).unwrap();
    }
}
