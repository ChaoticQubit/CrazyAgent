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
    assert_eq!(callers.serve(&mut panes), 0);
    assert_eq!(lists.get(), 0, "no caller, no listing");

    let digest = [3_u8; DIGEST_BYTES];
    let mut first = sent(&socket, &line(&id(1), &digest));
    assert_eq!(callers.serve(&mut panes), 1);
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
    assert_eq!(callers.serve(&mut panes), 1);
    assert_eq!(reply(&mut again), NAMED);
    assert_eq!(
        reply(&mut reused),
        UNNAMED,
        "a named call id is never named again"
    );
    assert_eq!(callers.take(&id(2), &digest), Some("%7".to_owned()));
    assert_eq!(callers.take(&id(2), &digest), None);

    let mut stranger = sent(&socket, &line(&id(3), &digest));
    let mut none = || vec![("%9".to_owned(), u32::MAX)];
    assert_eq!(callers.serve(&mut none), 0);
    assert_eq!(
        reply(&mut stranger),
        UNNAMED,
        "no listed pane holds this process"
    );
    assert_eq!(callers.take(&id(3), &digest), None);
    drop(callers);
    assert!(
        std::fs::symlink_metadata(&socket).is_err(),
        "the socket is removed"
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
    assert_eq!(callers.serve(&mut panes), 0);
    for stream in &mut streams {
        assert_eq!(reply(stream), b"", "dropped without a word");
    }
    assert_eq!(callers.pending.len(), 1, "the unfinished line waits");
    drop(unfinished);
    assert_eq!(callers.serve(&mut panes), 0);
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
                callers.serve(&mut || vec![("%7".to_owned(), listed)]);
                std::thread::sleep(Duration::from_millis(10));
            }
            asked.join().unwrap()
        });
        assert_eq!(served, named, "call {call}");
        assert_eq!(
            callers.take(&id(call), &digest),
            named.then(|| "%7".to_owned())
        );
    }
    drop(callers);
    std::fs::write(&socket, b"").unwrap();
    assert!(
        !name_call(&mailbox, &id(4), &digest),
        "a caller socket nobody serves names nothing"
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
        assert_eq!(callers.serve(&mut || vec![("%1".to_owned(), own)]), 16);
        for stream in &mut streams {
            assert_eq!(reply(stream), NAMED);
        }
    }
    let mut other = sent(&socket, &line(&id(65), &digest));
    assert_eq!(
        callers.serve(&mut || vec![("%2".to_owned(), own)]),
        1,
        "another pane's call is named"
    );
    assert_eq!(reply(&mut other), NAMED);
    assert_eq!(callers.take(&id(65), &digest), Some("%2".to_owned()));
    assert_eq!(
        callers.take(&id(64), &digest),
        Some("%1".to_owned()),
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
    assert_eq!(callers.serve(&mut || vec![("%1".to_owned(), own)]), 0);
    let mut other = sent(&socket, &line(&id(99), &digest));
    assert_eq!(
        callers.serve(&mut || vec![("%2".to_owned(), own)]),
        1,
        "another pane's call is named"
    );
    assert_eq!(reply(&mut other), NAMED);
    assert_eq!(callers.take(&id(99), &digest), Some("%2".to_owned()));
    drop(held);
}

/// The engine removes its socket when dropped only from the folder it bound in: a folder
/// swapped for a link to another folder keeps whatever that folder holds (DEC-111 S1R1-1).
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
