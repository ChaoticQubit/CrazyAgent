use super::*;
use std::os::unix::fs::PermissionsExt;

fn folder() -> tempfile::TempDir {
    let folder = tempfile::Builder::new()
        .prefix("ca-callers-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    folder
}

fn line(id: &str, digest: &[u8; DIGEST_BYTES]) -> Vec<u8> {
    let mut line = format!("{id} ").into_bytes();
    for byte in digest {
        line.extend_from_slice(format!("{byte:02x}").as_bytes());
    }
    line.push(b'\n');
    line
}

fn sent(socket: &Path, bytes: &[u8]) -> UnixStream {
    let mut stream = crate::spawn_gate::connect_unix_std(socket).unwrap();
    stream.set_read_timeout(Some(CALLER_WAIT)).unwrap();
    stream.write_all(bytes).unwrap();
    stream
}

fn reply(stream: &mut UnixStream) -> Vec<u8> {
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    reply
}

fn id(n: u8) -> String {
    format!("{n:032x}")
}

fn no_relay(_: &str, _: u32, _: &[u8], _: &mut dyn Write) {}

#[test]
fn a_call_is_named_once_by_the_listed_pane_of_its_process_or_a_parent() {
    let folder = folder();
    let socket = folder.path().join(CALLER_SOCKET);
    let mut callers = Callers::bind(&socket, None).unwrap();
    let parent = std::os::unix::process::parent_id();
    let lists = std::cell::Cell::new(0);
    let mut panes = || {
        lists.set(lists.get() + 1);
        vec![("%7".to_owned(), parent), ("%8".to_owned(), u32::MAX)]
    };
    assert_eq!(
        callers.serve(&mut panes, &mut |_, _| None, &mut no_relay),
        0
    );
    assert_eq!(lists.get(), 0, "no caller, no listing");

    let digest = [3_u8; DIGEST_BYTES];
    let mut first = sent(&socket, &line(&id(1), &digest));
    assert_eq!(
        callers.serve(&mut panes, &mut |_, _| None, &mut no_relay),
        1
    );
    assert_eq!(reply(&mut first), NAMED);
    assert_eq!(
        callers.take(&id(1), &[4_u8; DIGEST_BYTES]),
        None,
        "other bytes"
    );
    assert_eq!(
        callers.take(&id(1), &digest),
        None,
        "a call whose bytes changed loses its name"
    );

    let mut again = sent(&socket, &line(&id(2), &digest));
    let mut reused = sent(&socket, &line(&id(2), &[9_u8; DIGEST_BYTES]));
    assert_eq!(
        callers.serve(&mut panes, &mut |_, _| None, &mut no_relay),
        1
    );
    assert_eq!(reply(&mut again), NAMED);
    assert_eq!(
        reply(&mut reused),
        UNNAMED,
        "a named call id is never named again"
    );
    assert_eq!(
        callers.take(&id(2), &digest),
        Some(("%7".to_owned(), parent))
    );
    assert_eq!(callers.take(&id(2), &digest), None);

    let mut stranger = sent(&socket, &line(&id(3), &digest));
    let mut none = || vec![("%9".to_owned(), u32::MAX)];
    assert_eq!(callers.serve(&mut none, &mut |_, _| None, &mut no_relay), 0);
    assert_eq!(
        reply(&mut stranger),
        UNNAMED,
        "no listed pane holds this process"
    );
    assert_eq!(callers.take(&id(3), &digest), None);
    drop(callers);
    assert!(
        std::fs::symlink_metadata(&socket).is_ok_and(|meta| meta.file_type().is_socket()),
        "the dropped socket's node stays for later askers to be refused on"
    );
}

#[test]
fn a_malformed_or_unfinished_line_names_nothing() {
    let folder = folder();
    let socket = folder.path().join(CALLER_SOCKET);
    let mut callers = Callers::bind(&socket, None).unwrap();
    let own = std::process::id();
    let mut panes = || vec![("%7".to_owned(), own)];
    let digest = [5_u8; DIGEST_BYTES];
    let mut upper = line(&id(1), &digest);
    upper[ID_BYTES + 1] = b'A';
    let mut short = line(&id(2), &digest);
    short.remove(0);
    let mut streams = [
        sent(&socket, &upper),
        sent(&socket, &short),
        sent(
            &socket,
            &line(&format!("{:-<32}", "not-a-call-id"), &digest),
        ),
        sent(&socket, &[b'0'; LINE_BYTES]),
    ];
    let unfinished = sent(&socket, &line(&id(5), &digest)[..LINE_BYTES - 1]);
    assert_eq!(
        callers.serve(&mut panes, &mut |_, _| None, &mut no_relay),
        0
    );
    for stream in &mut streams {
        assert_eq!(reply(stream), b"", "dropped without a word");
    }
    assert_eq!(callers.pending.len(), 1, "the unfinished line waits");
    drop(unfinished);
    assert_eq!(
        callers.serve(&mut panes, &mut |_, _| None, &mut no_relay),
        0
    );
    assert!(callers.pending.is_empty(), "a closed connection is dropped");
    assert!(callers.stamps.is_empty());
}

#[test]
fn a_mailbox_finds_the_caller_socket_in_its_agent_s_short_temp_folder() {
    let agent = Path::new("/state/team/agents/agent-3");
    assert_eq!(
        socket_for_mailbox(&agent.join("tmp").join(crate::team::tool::MAILBOX_FOLDER)),
        Some(crate::sandbox::toolchains::tmp_path(&agent.join("home")).join(CALLER_SOCKET))
    );
    for other in [
        agent.join("tmp").join("calls"),
        agent.join("other").join(crate::team::tool::MAILBOX_FOLDER),
        PathBuf::from(crate::team::tool::MAILBOX_FOLDER),
    ] {
        assert_eq!(socket_for_mailbox(&other), None, "{}", other.display());
    }
}

struct Removed(PathBuf);

impl Drop for Removed {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_team_server_sends_its_call_only_once_the_engine_named_it() {
    let folder = folder();
    let mailbox = folder
        .path()
        .join("agent-1")
        .join("tmp")
        .join(crate::team::tool::MAILBOX_FOLDER);
    std::fs::create_dir_all(&mailbox).unwrap();
    let digest = [6_u8; DIGEST_BYTES];
    assert!(
        name_call(&mailbox, &id(1), &digest),
        "an agent with no caller socket leads no native team"
    );
    let socket = socket_for_mailbox(&mailbox).unwrap();
    let short = socket.parent().unwrap().to_path_buf();
    assert!(!short.exists(), "{}", short.display());
    std::fs::create_dir(&short).unwrap();
    let _removed = Removed(short);
    std::fs::set_permissions(
        socket.parent().unwrap(),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let mut callers = Callers::bind(&socket, None).unwrap();
    let own = std::process::id();
    for (call, listed, named) in [(2, own, true), (3, u32::MAX, false)] {
        let served = std::thread::scope(|scope| {
            let asked = scope.spawn(|| name_call(&mailbox, &id(call), &digest));
            let deadline = Instant::now() + CALLER_WAIT;
            while !asked.is_finished() && Instant::now() < deadline {
                callers.serve(
                    &mut || vec![("%7".to_owned(), listed)],
                    &mut |_, _| None,
                    &mut no_relay,
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            asked.join().unwrap()
        });
        assert_eq!(served, named, "call {call}");
        assert_eq!(
            callers.take(&id(call), &digest),
            named.then(|| ("%7".to_owned(), own))
        );
    }
    drop(callers);
    assert!(
        !name_call(&mailbox, &id(4), &digest),
        "a caller socket nobody serves names nothing"
    );
}

/// A relay request from a pane's process reaches the engine's handler with that pane and
/// its bytes, longer than a call line, and the handler's answer comes back whole, also one
/// past a socket's default send buffer; a request from a process in no pane is refused
/// before the handler, and one that does not fit a line is never sent (relay over the
/// caller socket, OQ-7 B).
#[test]
fn a_relay_request_reaches_the_engine_with_its_pane_and_its_whole_answer_comes_back() {
    let folder = folder();
    let mailbox = folder
        .path()
        .join("agent-1")
        .join("tmp")
        .join(crate::team::tool::MAILBOX_FOLDER);
    std::fs::create_dir_all(&mailbox).unwrap();
    let socket = socket_for_mailbox(&mailbox).unwrap();
    let short = socket.parent().unwrap().to_path_buf();
    assert!(!short.exists(), "{}", short.display());
    std::fs::create_dir(&short).unwrap();
    let _removed = Removed(short.clone());
    std::fs::set_permissions(&short, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut callers = Callers::bind(&socket, None).unwrap();
    let own = std::process::id();
    let request = format!("post own {}", "x".repeat(LINE_BYTES));
    let payload = vec![b'a'; REPLY_BYTES_MAX];
    for (listed, expected) in [(own, Some(payload.clone())), (u32::MAX, None)] {
        let mut seen = Vec::new();
        let answered = std::thread::scope(|scope| {
            let asked = scope.spawn(|| relay(&mailbox, &request, CALLER_WAIT));
            let deadline = Instant::now() + CALLER_WAIT;
            while !asked.is_finished() && Instant::now() < deadline {
                callers.serve(
                    &mut || vec![("%7".to_owned(), listed)],
                    &mut |_, _| None,
                    &mut |pane, pid, line, out| {
                        seen.push((pane.to_owned(), pid, line.to_vec()));
                        out.write_all(ANSWERED).unwrap();
                        out.write_all(&payload).unwrap();
                    },
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            asked.join().unwrap()
        });
        assert_eq!(answered, expected, "listed {listed}");
        let reached = expected.map(|_| ("%7".to_owned(), own, request.clone().into_bytes()));
        assert_eq!(
            seen,
            reached.into_iter().collect::<Vec<_>>(),
            "listed {listed}"
        );
    }
    assert_eq!(
        relay(&mailbox, &"x".repeat(RELAY_LINE_BYTES_MAX), CALLER_WAIT),
        None,
        "a request past one line is never sent"
    );
}

/// One pane that names many calls and never sends them keeps only its own newest stamps,
/// so another pane's call is still named (DEC-111 S1R1-3).
#[test]
fn one_pane_filling_the_stamps_never_stops_another_pane_s_call() {
    let folder = folder();
    let socket = folder.path().join(CALLER_SOCKET);
    let mut callers = Callers::bind(&socket, None).unwrap();
    let own = std::process::id();
    let digest = [1_u8; DIGEST_BYTES];
    for round in 0..4_u8 {
        let mut streams: Vec<UnixStream> = (0..16_u8)
            .map(|n| sent(&socket, &line(&id(round * 16 + n + 1), &digest)))
            .collect();
        assert_eq!(
            callers.serve(
                &mut || vec![("%1".to_owned(), own)],
                &mut |_, _| None,
                &mut no_relay
            ),
            16
        );
        for stream in &mut streams {
            assert_eq!(reply(stream), NAMED);
        }
    }
    let mut other = sent(&socket, &line(&id(65), &digest));
    assert_eq!(
        callers.serve(
            &mut || vec![("%2".to_owned(), own)],
            &mut |_, _| None,
            &mut no_relay
        ),
        1,
        "another pane's call is named"
    );
    assert_eq!(reply(&mut other), NAMED);
    assert_eq!(callers.take(&id(65), &digest), Some(("%2".to_owned(), own)));
    assert_eq!(
        callers.take(&id(64), &digest),
        Some(("%1".to_owned(), own)),
        "the full pane keeps its newest"
    );
    assert_eq!(
        callers.take(&id(1), &digest),
        None,
        "the full pane lost its oldest"
    );
}

/// One pane holding connections open with no full line keeps only a few waiting places,
/// so another pane's call is still named (DEC-111 S1R1-3).
#[test]
fn one_pane_holding_unfinished_lines_never_stops_another_pane_s_call() {
    let folder = folder();
    let socket = folder.path().join(CALLER_SOCKET);
    let mut callers = Callers::bind(&socket, None).unwrap();
    let own = std::process::id();
    let digest = [2_u8; DIGEST_BYTES];
    let held: Vec<UnixStream> = (0..PENDING_MAX)
        .map(|n| sent(&socket, &line(&id(n as u8 + 1), &digest)[..10]))
        .collect();
    assert_eq!(
        callers.serve(
            &mut || vec![("%1".to_owned(), own)],
            &mut |_, _| None,
            &mut no_relay
        ),
        0
    );
    let mut other = sent(&socket, &line(&id(99), &digest));
    assert_eq!(
        callers.serve(
            &mut || vec![("%2".to_owned(), own)],
            &mut |_, _| None,
            &mut no_relay
        ),
        1,
        "another pane's call is named"
    );
    assert_eq!(reply(&mut other), NAMED);
    assert_eq!(callers.take(&id(99), &digest), Some(("%2".to_owned(), own)));
    drop(held);
}

/// The engine removes nothing when its socket is dropped, also not through a folder swapped
/// for a link to another folder, which keeps whatever it holds (DEC-111 S1R1-1, RH-2).
#[test]
fn dropping_the_socket_never_removes_through_a_swapped_folder() {
    let root = folder();
    let bound = root.path().join("bound");
    std::fs::create_dir(&bound).unwrap();
    std::fs::set_permissions(&bound, std::fs::Permissions::from_mode(0o700)).unwrap();
    let callers = Callers::bind(&bound.join(CALLER_SOCKET), None).unwrap();
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let planted = elsewhere.join(CALLER_SOCKET);
    std::fs::write(&planted, b"owner data").unwrap();
    std::fs::rename(&bound, root.path().join("moved")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &bound).unwrap();

    drop(callers);
    assert_eq!(
        std::fs::read(&planted).ok().as_deref(),
        Some(&b"owner data"[..]),
        "the file behind the link survives"
    );
}

#[test]
fn a_pane_respawned_with_another_process_names_nothing_also_after_a_rebind() {
    let folder = folder();
    let socket = folder.path().join(CALLER_SOCKET);
    let mut callers = Callers::bind(&socket, None).unwrap();
    let own = std::process::id();
    let parent = std::os::unix::process::parent_id();
    let digest = [5_u8; DIGEST_BYTES];
    let mut first = sent(&socket, &line(&id(1), &digest));
    assert_eq!(
        callers.serve(
            &mut || vec![("%7".to_owned(), own)],
            &mut |_, _| None,
            &mut no_relay
        ),
        1
    );
    assert_eq!(reply(&mut first), NAMED);
    assert_eq!(callers.take(&id(1), &digest), Some(("%7".to_owned(), own)));

    let mut respawned = sent(&socket, &line(&id(2), &digest));
    assert_eq!(
        callers.serve(
            &mut || vec![("%7".to_owned(), parent)],
            &mut |_, _| None,
            &mut no_relay
        ),
        0
    );
    assert_eq!(
        reply(&mut respawned),
        UNNAMED,
        "the pane's id under another process names nothing"
    );
    assert_eq!(callers.take(&id(2), &digest), None);

    let pins = callers.pins();
    let pin = callers.folder();
    drop(callers);
    let mut rebound = Callers::bind(&socket, Some(pin)).unwrap().with_pins(pins);
    let mut again = sent(&socket, &line(&id(3), &digest));
    assert_eq!(
        rebound.serve(
            &mut || vec![("%7".to_owned(), parent)],
            &mut |_, _| None,
            &mut no_relay
        ),
        0
    );
    assert_eq!(
        reply(&mut again),
        UNNAMED,
        "a socket bound again keeps the pins"
    );
    drop(rebound);

    let mut fresh = Callers::bind(&socket, Some(pin)).unwrap();
    let mut unpinned = sent(&socket, &line(&id(4), &digest));
    assert_eq!(
        fresh.serve(
            &mut || vec![("%7".to_owned(), parent)],
            &mut |_, _| None,
            &mut no_relay
        ),
        1
    );
    assert_eq!(
        reply(&mut unpinned),
        NAMED,
        "only the pin refused the respawned pane"
    );
}

/// A `who` from a pane respawned under another process is answered no, as its calls are
/// never named: the engine answers with the same pinned pane it names calls by (TTDL F2,
/// rv-secreview merge point 4).
#[test]
fn a_respawned_pane_s_who_is_answered_no() {
    let folder = folder();
    let socket = folder.path().join(CALLER_SOCKET);
    let mut callers = Callers::bind(&socket, None).unwrap();
    let own = std::process::id();
    let parent = std::os::unix::process::parent_id();
    let mut first = sent(&socket, WHO);
    callers.serve(
        &mut || vec![("%7".to_owned(), own)],
        &mut |_, _| Some(Whom::Lead),
        &mut no_relay,
    );
    let answered = reply(&mut first);
    assert!(
        answered.starts_with(b"%7"),
        "{}",
        String::from_utf8_lossy(&answered)
    );

    let mut respawned = sent(&socket, WHO);
    callers.serve(
        &mut || vec![("%7".to_owned(), parent)],
        &mut |_, _| Some(Whom::Lead),
        &mut no_relay,
    );
    assert_eq!(
        reply(&mut respawned),
        UNNAMED,
        "the pane's id under another process is no one"
    );
}

/// The engine answers `who` with the pane and who runs in it, from its own rows: the lead,
/// or a teammate by its registered name, and no for a pane it names no one by or a name
/// that could not name a worktree (DEC-111 Q18, RH-2).
#[test]
fn who_is_answered_with_the_pane_and_the_member_the_engine_knows_there() {
    let own = std::process::id();
    for (whom, expected) in [
        (Some(Whom::Lead), b"%7 lead\n".to_vec()),
        (
            Some(Whom::Teammate("alpha".to_owned())),
            b"%7 teammate alpha\n".to_vec(),
        ),
        (None, UNNAMED.to_vec()),
        (Some(Whom::Teammate("../up".to_owned())), UNNAMED.to_vec()),
        (
            Some(Whom::Teammate("team-lead".to_owned())),
            UNNAMED.to_vec(),
        ),
    ] {
        let folder = folder();
        let socket = folder.path().join(CALLER_SOCKET);
        let mut callers = Callers::bind(&socket, None).unwrap();
        let mut asked = sent(&socket, WHO);
        callers.serve(
            &mut || vec![("%7".to_owned(), own)],
            &mut |pane, pid| {
                assert_eq!((pane, pid), ("%7", own));
                whom.clone()
            },
            &mut no_relay,
        );
        assert_eq!(
            String::from_utf8_lossy(&reply(&mut asked)),
            String::from_utf8_lossy(&expected),
            "{whom:?}"
        );
    }
}

/// Dropping the socket leaves its node in place, so while no engine answers there, as
/// across an engine restart, a hook that asks is refused rather than finding no socket,
/// which reads as an agent that leads no native team (DEC-111 Q18, RH-2).
#[test]
fn a_dropped_socket_leaves_its_node_for_hooks_to_be_refused_on() {
    let folder = folder();
    let socket = folder.path().join(CALLER_SOCKET);
    let callers = Callers::bind(&socket, None).unwrap();
    drop(callers);
    assert!(
        std::fs::symlink_metadata(&socket).is_ok_and(|meta| meta.file_type().is_socket()),
        "the node stays"
    );
    assert!(crate::spawn_gate::connect_unix_std(&socket).is_err());
}

/// A caller socket bound again, as on a native lead's relaunch or the rebind after an engine
/// restart, never leaves its path empty: a hook asking all along finds the old node or the
/// new socket, and is never told that its agent leads no native team, which would hand a
/// teammate the lead's place (rv-secreview RH2R2-1).
#[test]
fn a_hook_asking_while_the_socket_is_bound_again_never_reads_alone() {
    struct TmpRemoved(PathBuf);
    impl Drop for TmpRemoved {
        fn drop(&mut self) {
            let _ = crate::sandbox::toolchains::remove_tmp(&self.0);
        }
    }
    const REBINDS: usize = 2000;
    let root = folder();
    let agent = root.path().join("agent-1");
    let mailbox = agent.join("tmp").join(crate::team::tool::MAILBOX_FOLDER);
    std::fs::create_dir_all(&mailbox).unwrap();
    let home = agent.join("home");
    let scratch = crate::sandbox::toolchains::make_tmp(&home).unwrap();
    let _scratch = TmpRemoved(home);
    let socket = socket_for_mailbox(&mailbox).unwrap();
    assert_eq!(socket, scratch.join(CALLER_SOCKET));
    let callers = Callers::bind(&socket, None).unwrap();
    let pin = callers.folder();
    drop(callers);
    let done = std::sync::atomic::AtomicBool::new(false);
    let (asked, alone) = std::thread::scope(|scope| {
        let watcher = scope.spawn(|| {
            let (mut asked, mut alone) = (0_usize, 0_usize);
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                asked += 1;
                if own_place(&mailbox, Duration::from_millis(1)) == Place::Alone {
                    alone += 1;
                }
            }
            (asked, alone)
        });
        for _ in 0..REBINDS {
            drop(Callers::bind(&socket, Some(pin)).unwrap());
        }
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        watcher.join().unwrap()
    });
    assert!(asked > 0);
    assert_eq!(
        alone, 0,
        "{alone} of {asked} asks during {REBINDS} rebinds read the lead's place"
    );
}
