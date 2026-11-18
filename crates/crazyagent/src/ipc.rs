use std::io;
use std::path::Path;

pub(crate) use crate::platform::unix_socket::{
    OwnedReadHalf, OwnedWriteHalf, ReadHalf, UnixListener as Listener, UnixStream as Stream,
};

/// Binds the owner-class listener at `path`: a socket made inside the spawn gate, guarded by
/// its parent, the protected root. Must run inside a Tokio runtime.
/// Input: the endpoint path. Output: the listener.
#[cfg(unix)]
pub(crate) fn owner_listener(path: impl AsRef<Path>) -> io::Result<Listener> {
    crate::spawn_gate::bind_unix(path)
}

/// Accepts one owner-class connection, taking the spawn gate only for each accept attempt.
/// Input: the listener. Output: the stream, whose peer is not checked yet.
pub(crate) async fn owner_accept(listener: &Listener) -> io::Result<Stream> {
    crate::spawn_gate::accept_unix(listener)
        .await
        .map(|(stream, _)| stream)
}

/// Connects to the owner-class endpoint at `path`, whose server is not checked yet.
/// Input: the endpoint path. Output: the stream, or `unix_socket_unavailable` where this
/// platform has no owner-class transport yet.
pub(crate) async fn owner_connect(path: impl AsRef<Path>) -> io::Result<Stream> {
    crate::spawn_gate::connect_unix(path).await
}

/// The server side's check of an accepted stream: its client runs as this user.
/// Input: the stream. Output: Ok, `peer_owner_mismatch`, or the error reading the peer.
pub(crate) fn verify_client(stream: &Stream) -> io::Result<()> {
    same_user(stream)
}

/// The client side's check, right after connect and before any byte is written: the server
/// runs as this user. Input: the stream. Output: Ok, `peer_owner_mismatch`, or the error
/// reading the peer.
pub(crate) fn verify_server(stream: &Stream) -> io::Result<()> {
    same_user(stream)
}

/// The Unix peer check both sides share: the kernel's credentials of the peer name this
/// process's user. Input: the stream. Output: Ok, `peer_owner_mismatch`, or the error
/// reading the credentials.
#[cfg(unix)]
fn same_user(stream: &Stream) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let uid = nix::unistd::getpeereid(stream)
        .map_err(io::Error::other)?
        .0
        .as_raw();
    #[cfg(not(target_os = "macos"))]
    let uid = stream.peer_cred()?.uid();
    if uid != rustix::process::getuid().as_raw() {
        return Err(io::Error::other("peer_owner_mismatch"));
    }
    Ok(())
}

/// The peer check of a stream that cannot exist where there is no owner-class transport.
/// Input: that stream. Output: unreachable by construction.
#[cfg(not(unix))]
fn same_user(stream: &Stream) -> io::Result<()> {
    match *stream {}
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::FileTypeExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn an_owner_endpoint_serves_a_client_of_this_user_at_its_path() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("engine.sock");
        let listener = owner_listener(&path).unwrap();
        let kind = std::fs::symlink_metadata(&path).unwrap().file_type();
        assert!(kind.is_socket());
        let (client, served) = tokio::join!(owner_connect(&path), owner_accept(&listener));
        let (mut client, mut served) = (client.unwrap(), served.unwrap());
        verify_server(&client).unwrap();
        verify_client(&served).unwrap();
        client.write_all(b"ping").await.unwrap();
        let mut bytes = [0; 4];
        served.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"ping");
    }
}

/// The Windows pipe name of an owner-class endpoint (DESIGN-0256 D7):
/// `\\.\pipe\crazyagent-<32 hex of sha256(root)>-<purpose>-<32 hex random>`, the root being
/// the protected folder's final name, hashed as little-endian UTF-16. Inputs: that name, the
/// endpoint's plain purpose (1-32 of a-z, 0-9, '.' and '-') and the per-start random part.
/// Output: the pipe name.
#[cfg(any(windows, test))]
pub(crate) fn pipe_name(root: &[u16], purpose: &str, random: &[u8; 16]) -> String {
    use sha2::{Digest, Sha256};
    assert!(!root.is_empty(), "a protected root has a final name");
    assert!(plain_purpose(purpose), "a pipe purpose is a plain name");
    let bytes: Vec<u8> = root.iter().flat_map(|unit| unit.to_le_bytes()).collect();
    let digest = Sha256::digest(&bytes);
    let hex = |part: &[u8]| -> String { part.iter().map(|byte| format!("{byte:02x}")).collect() };
    let name = format!(
        r"\\.\pipe\crazyagent-{}-{purpose}-{}",
        hex(&digest[..16]),
        hex(random)
    );
    assert!(name.len() <= 256);
    name
}

/// Whether a purpose is the plain name D7 places verbatim in a pipe name and its pipes file:
/// 1-32 of a-z, 0-9, '.' and '-'. Input: the purpose. Output: whether it is plain.
#[cfg(any(windows, test))]
fn plain_purpose(purpose: &str) -> bool {
    (1..=32).contains(&purpose.len())
        && purpose
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-'))
}

/// The first field of every D7 pipes file, naming its version.
#[cfg(any(windows, test))]
const PIPES_VERSION: &str = "crazyagent-pipes-1";
/// The most bytes a D7 pipes file may take; the longest line the engine writes is 117.
#[cfg(any(windows, test))]
const PIPES_BYTES_MAX: usize = 128;

/// What the D7 pipes file in a protected root holds for its one owner-class endpoint
/// (DESIGN-0256 D7 and D10 step 1, leader 07:46): the per-start random part of the pipe name
/// and the pin, process ID and creation time, of the engine that serves it.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Published {
    pub(crate) random: [u8; 16],
    pub(crate) pid: u32,
    pub(crate) created: u64,
}

/// The one line the engine writes as a D7 pipes file: the version, the purpose, the random
/// part as 32 lowercase hex and the pin in decimal. Inputs: the endpoint's plain purpose and
/// what is published. Output: the line, newline included.
#[cfg(any(windows, test))]
fn encode_pipes(purpose: &str, published: &Published) -> String {
    assert!(plain_purpose(purpose), "a pipe purpose is a plain name");
    let random: String = published
        .random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let text = format!(
        "{PIPES_VERSION} {purpose} {random} {} {}\n",
        published.pid, published.created
    );
    assert!(text.len() <= PIPES_BYTES_MAX);
    text
}

/// Reads a D7 pipes file a client opened through the private judge, trusting only the exact
/// line [`encode_pipes`] writes for this purpose with a nonzero pin. Inputs: the file's bytes,
/// at most one past the cap, and the purpose. Output: what the engine published, or why the
/// file is refused.
#[cfg(any(windows, test))]
fn parse_pipes(bytes: &[u8], purpose: &str) -> Result<Published, &'static str> {
    const SHAPE: &str = "the pipes file is not in the engine's shape";
    if bytes.len() > PIPES_BYTES_MAX {
        return Err(SHAPE);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| SHAPE)?;
    let fields: Vec<&str> = text.strip_suffix('\n').ok_or(SHAPE)?.split(' ').collect();
    let [version, named, hex, pid, created] = fields[..] else {
        return Err(SHAPE);
    };
    if version != PIPES_VERSION {
        return Err(SHAPE);
    }
    if named != purpose {
        return Err("the pipes file names another endpoint");
    }
    if hex.len() != 32 || !hex.is_ascii() {
        return Err(SHAPE);
    }
    let mut random = [0; 16];
    for (slot, pair) in random.iter_mut().zip(hex.as_bytes().chunks(2)) {
        let pair = std::str::from_utf8(pair).map_err(|_| SHAPE)?;
        *slot = u8::from_str_radix(pair, 16).map_err(|_| SHAPE)?;
    }
    let published = Published {
        random,
        pid: pid.parse().map_err(|_| SHAPE)?,
        created: created.parse().map_err(|_| SHAPE)?,
    };
    if published.pid == 0 || published.created == 0 || encode_pipes(purpose, &published) != text {
        return Err(SHAPE);
    }
    Ok(published)
}

#[cfg(windows)]
mod windows;

#[cfg(test)]
mod name_tests {
    use super::pipe_name;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    const ROOT: &str = r"\\?\C:\Users\owner\AppData\Local\CrazyAgent";
    const RANDOM: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

    /// DESIGN-0256 D7: the name is the fixed prefix, 32 hex of sha256 over the root's final
    /// name as little-endian UTF-16, the purpose and 32 hex of the per-start random part.
    #[test]
    fn a_pipe_name_carries_the_root_hash_the_purpose_and_the_random_part() {
        assert_eq!(
            pipe_name(&units(ROOT), "engine", &RANDOM),
            r"\\.\pipe\crazyagent-22aa81ee53910ef32d5dcbc1b09f87fd-engine-000102030405060708090a0b0c0d0e0f"
        );
    }

    /// D7: another root, purpose or random part gives another name, so neither a second engine
    /// nor a later start of the same one can be named from a known name.
    #[test]
    fn each_part_of_a_pipe_name_changes_it() {
        let base = pipe_name(&units(ROOT), "engine", &RANDOM);
        let other_root = units(r"\\?\C:\Users\owner\AppData\Local\CrazyAgent\o00000001");
        assert_ne!(pipe_name(&other_root, "engine", &RANDOM), base);
        assert_ne!(pipe_name(&units(ROOT), "owner", &RANDOM), base);
        let mut later = RANDOM;
        later[15] ^= 1;
        assert_ne!(pipe_name(&units(ROOT), "engine", &later), base);
    }

    /// D7: a purpose is the endpoint's own plain name, never a separator or text of another
    /// shape, as it is placed in the pipe name verbatim.
    #[test]
    fn a_purpose_outside_the_plain_shape_is_a_programming_error() {
        let long = "a".repeat(33);
        for purpose in ["", "Engine", r"a\b", "a/b", "é", long.as_str()] {
            let refused = std::panic::catch_unwind(|| pipe_name(&units(ROOT), purpose, &RANDOM));
            assert!(refused.is_err(), "{purpose:?}");
        }
        assert!(pipe_name(&units(ROOT), "engine.sock", &RANDOM).contains("-engine.sock-"));
    }

    /// rv-code #551 F3: a protected root always has a name, so naming a pipe from an empty
    /// one is a programming error, never a name every empty root would share.
    #[test]
    fn an_empty_root_is_a_programming_error() {
        let named = std::panic::catch_unwind(|| pipe_name(&[], "engine", &RANDOM));
        assert!(named.is_err());
    }
}

#[cfg(test)]
mod pipes_tests {
    use super::{Published, encode_pipes, parse_pipes};

    const PUBLISHED: Published = Published {
        random: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        pid: 4242,
        created: 133_000_000_000_000_000,
    };
    const SHAPE: &str = "the pipes file is not in the engine's shape";

    /// DESIGN-0256 D7 pipes file (leader 07:46): one line with the version, the endpoint's
    /// purpose, the per-start random part and the engine's pin, read back the same.
    #[test]
    fn a_pipes_file_reads_back_what_the_engine_wrote() {
        let text = encode_pipes("engine.sock", &PUBLISHED);
        assert_eq!(
            text,
            "crazyagent-pipes-1 engine.sock 000102030405060708090a0b0c0d0e0f 4242 133000000000000000\n"
        );
        assert_eq!(parse_pipes(text.as_bytes(), "engine.sock"), Ok(PUBLISHED));
    }

    /// D7: a client trusts only the exact line the engine writes, so another endpoint's file,
    /// another version, any other spelling of a field, extra bytes, a zero pin or an oversized
    /// file is refused.
    #[test]
    fn a_pipes_file_in_any_other_shape_is_refused() {
        let good = encode_pipes("engine.sock", &PUBLISHED);
        assert_eq!(
            parse_pipes(good.as_bytes(), "owner.sock"),
            Err("the pipes file names another endpoint")
        );
        let bad = [
            String::new(),
            good.trim_end().to_owned(),
            format!("{good}\n"),
            good.replace("-pipes-1 ", "-pipes-2 "),
            good.replace("0a0b", "0A0B"),
            good.replace("0e0f ", "0e0f0f "),
            good.replace(" 4242 ", " 04242 "),
            good.replace(" 4242 ", " +4242 "),
            good.replace(" 4242 ", "  4242 "),
            good.replace(" 4242 ", " 0 "),
            good.replace(" 133000000000000000", " 0"),
            "x".repeat(200),
        ];
        for text in &bad {
            assert_eq!(
                parse_pipes(text.as_bytes(), "engine.sock"),
                Err(SHAPE),
                "{text:?}"
            );
        }
    }

    /// D7: the purpose is written verbatim, so a purpose outside the plain shape is a
    /// programming error, never a line a client would split another way.
    #[test]
    fn a_pipes_file_for_a_purpose_outside_the_plain_shape_is_a_programming_error() {
        for purpose in ["", "engine sock", "Engine", "a\nb"] {
            let written = std::panic::catch_unwind(|| encode_pipes(purpose, &PUBLISHED));
            assert!(written.is_err(), "{purpose:?}");
        }
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::windows::{
        bind, bind_with, connect, name_of, publish, read, verify_client, verify_server,
    };
    use crazyagent_win_sandbox::{pipe, private};
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A private root as the engine holds one, inside a fresh temp folder. Output: the temp
    /// folder, kept alive by the caller, and the root.
    fn private_root() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        drop(private::dir(&root).unwrap());
        (temp, root)
    }

    /// The names in a folder, sorted. Input: the folder. Output: the names.
    fn entries(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// DESIGN-0256 D7 pipes file (leader 07:46): the engine publishes a private file its
    /// clients read back the same, pinning this process; a later start replaces it with its
    /// own random part through a staged file in the same root, leaving nothing staged behind.
    #[test]
    fn a_published_pipes_file_reads_back_private_and_a_later_start_replaces_it() {
        let (_temp, root) = private_root();
        let first = publish(&root, "engine.sock", [1; 16]).unwrap();
        assert_eq!(first.random, [1; 16]);
        assert_eq!(first.pid, std::process::id());
        assert_eq!(read(&root, "engine.sock").unwrap(), first);
        drop(private::read_only(&root.join("pipes")).unwrap());
        let second = publish(&root, "engine.sock", [2; 16]).unwrap();
        assert_eq!(second.random, [2; 16]);
        assert_eq!((second.pid, second.created), (first.pid, first.created));
        assert_eq!(read(&root, "engine.sock").unwrap(), second);
        assert_eq!(entries(&root), ["pipes"]);
    }

    /// V244-WIN-PIPES-LISTENER-REQS (rv-sec3 #555 N1, leader 08:08): the pipes file carries a
    /// medium label with no-write-up and no-read-up from its creation, in a root that holds no
    /// label of its own, so no lower integrity process can open it to read, hold it and so
    /// block the next start's rename.
    #[test]
    fn a_pipes_file_is_labelled_medium_no_read_up_in_an_unlabelled_root() {
        let (_temp, root) = private_root();
        let held = private::dir(&root).unwrap();
        let root_label = private::facts(&held).unwrap().label;
        assert_eq!(
            root_label.map_or(Ok(None), |sacl| crazyagent_win_sandbox::dacl::label(&sacl)),
            Ok(None)
        );
        drop(held);
        publish(&root, "engine.sock", [7; 16]).unwrap();
        let file = private::read_only(&root.join("pipes")).unwrap();
        let sacl = private::facts(&file)
            .unwrap()
            .label
            .expect("a labelled pipes file");
        assert_eq!(
            crazyagent_win_sandbox::dacl::label(&sacl),
            Ok(Some((0x2000, 0x3)))
        );
    }

    /// D7 and #555 N1 in the engine's real root, the database store folder, whose label passes
    /// to the files made in it: the pipes file still reads back the same and carries the
    /// medium no-write-up no-read-up label.
    #[test]
    fn a_pipes_file_in_the_database_root_reads_back_labelled() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        drop(private::store_dir(&root, private::Store::Database).unwrap());
        let published = publish(&root, "engine.sock", [7; 16]).unwrap();
        assert_eq!(read(&root, "engine.sock").unwrap(), published);
        let file = private::read_only(&root.join("pipes")).unwrap();
        let sacl = private::facts(&file)
            .unwrap()
            .label
            .expect("a labelled pipes file");
        assert_eq!(
            crazyagent_win_sandbox::dacl::label(&sacl),
            Ok(Some((0x2000, 0x3)))
        );
    }

    /// V244-WIN-PIPES-LISTENER-REQS (rv-code2 #554 N4, leader 08:08): a staged pipes file an
    /// earlier start left behind, private to this user, is swept at the next publish, while a
    /// file of any other name in the root is kept.
    #[test]
    fn a_staged_pipes_file_an_earlier_start_left_is_swept_at_publish() {
        let (_temp, root) = private_root();
        drop(private::secret_file(&root.join("pipes.0123456789abcdef.new")).unwrap());
        drop(private::secret_file(&root.join("pipes.keep")).unwrap());
        publish(&root, "engine.sock", [7; 16]).unwrap();
        assert_eq!(entries(&root), ["pipes", "pipes.keep"]);
    }

    /// V244-WIN-PIPES-LISTENER-REQS (rv-code2 #554 N3): a client reading the pipes file holds
    /// it without delete sharing, which blocks the rename over it; a later start waits a
    /// bounded time for that read to end instead of failing, and leaves nothing staged.
    #[test]
    fn a_publish_waits_for_a_client_reading_the_pipes_file() {
        let (_temp, root) = private_root();
        publish(&root, "engine.sock", [7; 16]).unwrap();
        let reader = private::read_only(&root.join("pipes")).unwrap();
        let released = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            drop(reader);
        });
        let second = publish(&root, "engine.sock", [7; 16]).unwrap();
        released.join().unwrap();
        assert_eq!(read(&root, "engine.sock").unwrap(), second);
        assert_eq!(entries(&root), ["pipes"]);
    }

    /// rv-code #562 F3: a client that holds the pipes file open through every rename try
    /// fails the publish with the rename's error, and leaves the file it holds as it was
    /// and nothing staged beside it.
    #[test]
    fn a_publish_gives_up_on_a_pipes_file_held_through_every_try() {
        let (_temp, root) = private_root();
        let first = publish(&root, "engine.sock", [1; 16]).unwrap();
        let held = private::read_only(&root.join("pipes")).unwrap();
        let refused = publish(&root, "engine.sock", [2; 16]).unwrap_err();
        assert!(matches!(refused.raw_os_error(), Some(5 | 32)), "{refused}");
        drop(held);
        assert_eq!(read(&root, "engine.sock").unwrap(), first);
        assert_eq!(entries(&root), ["pipes"]);
    }

    /// rv-code #562 F3 (rv-sec3 #562 N4): a staged leftover another account owns fails the
    /// publish before anything is written, and stays as it was.
    #[test]
    fn a_staged_leftover_another_account_owns_is_refused_and_kept() {
        let (_temp, root) = private_root();
        let leftover = root.join("pipes.0123456789abcdef.new");
        std::fs::write(&leftover, "theirs\n").unwrap();
        let set = crate::spawn_gate::status(
            std::process::Command::new("icacls")
                .arg(&leftover)
                .args(["/setowner", "*S-1-5-32-545"])
                .stdin(std::process::Stdio::null()),
        )
        .unwrap();
        assert!(set.success());
        let refused = publish(&root, "engine.sock", [7; 16]).unwrap_err();
        assert_eq!(
            refused.kind(),
            std::io::ErrorKind::PermissionDenied,
            "{refused}"
        );
        assert_eq!(std::fs::read(&leftover).unwrap(), b"theirs\n");
        assert_eq!(entries(&root), ["pipes.0123456789abcdef.new"]);
    }

    /// D7 (leader 07:46): a pipes file already in the root that another account owns is
    /// refused and left as it was, never replaced, and nothing is staged beside it.
    #[test]
    fn a_pipes_file_another_account_owns_is_refused_not_replaced() {
        let (_temp, root) = private_root();
        let file = root.join("pipes");
        std::fs::write(&file, "theirs\n").unwrap();
        let set = crate::spawn_gate::status(
            std::process::Command::new("icacls")
                .arg(&file)
                .args(["/setowner", "*S-1-5-32-545"])
                .stdin(std::process::Stdio::null()),
        )
        .unwrap();
        assert!(set.success());
        let refused = publish(&root, "engine.sock", [7; 16]).unwrap_err();
        assert_eq!(
            refused.kind(),
            std::io::ErrorKind::PermissionDenied,
            "{refused}"
        );
        assert_eq!(std::fs::read(&file).unwrap(), b"theirs\n");
        assert!(read(&root, "engine.sock").is_err());
        assert_eq!(entries(&root), ["pipes"]);
    }

    /// V244-WIN-PIPES-LISTENER-REQS (rv-sec3 #555, leader 08:08): bind holds the first
    /// instance of the pipe name before it publishes. A name someone else made first fails
    /// the bind with access denied and no pipes file is written; after a bind, a first
    /// instance create of the published name is refused the same way.
    #[tokio::test]
    async fn a_bind_holds_the_pipe_name_before_it_publishes() {
        let (_temp, root) = private_root();
        let squatted = name_of(&root, "engine.sock", &[3; 16]).unwrap();
        let squat = pipe::create(&squatted, true, 2).unwrap();
        let refused = bind_with(&root, "engine.sock", [3; 16], 2).unwrap_err();
        assert_eq!(refused.raw_os_error(), Some(5), "{refused}");
        assert!(entries(&root).is_empty());
        drop(squat);
        let _listener = bind_with(&root, "engine.sock", [4; 16], 2).unwrap();
        assert_eq!(read(&root, "engine.sock").unwrap().random, [4; 16]);
        let held = name_of(&root, "engine.sock", &[4; 16]).unwrap();
        let again = pipe::create(&held, true, 2).unwrap_err();
        assert_eq!(again.raw_os_error(), Some(5), "{again}");
    }

    /// DESIGN-0256 D8-D10: a client of this user reaches the pipe a default bind published,
    /// the client's check of the server passes before it writes, the server's check of the
    /// client passes, bytes go both ways, and each check refuses the end it does not belong to.
    #[tokio::test]
    async fn a_bound_pipe_serves_a_client_of_this_user_with_both_checks_passing() {
        let (_temp, root) = private_root();
        let listener = bind(&root, "engine.sock").unwrap();
        let (client, served) = tokio::join!(connect(&root, "engine.sock"), listener.accept());
        let (mut client, mut served) = (client.unwrap(), served.unwrap());
        verify_server(&client).unwrap();
        verify_client(&served).unwrap();
        assert!(verify_client(&client).is_err());
        assert!(verify_server(&served).is_err());
        client.write_all(b"ping").await.unwrap();
        let mut bytes = [0; 4];
        served.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"ping");
        served.write_all(b"pong").await.unwrap();
        client.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"pong");
    }

    /// D8 (advisor 08:2x): a reply the server writes just before it drops its end reaches a
    /// client that reads it later, a short reply and one larger than the pipe's buffer alike,
    /// since a drop closes the instance and never disconnects it.
    #[tokio::test]
    async fn a_reply_written_just_before_the_server_drops_reaches_a_late_reader() {
        let (_temp, root) = private_root();
        let listener = bind_with(&root, "engine.sock", [6; 16], 2).unwrap();
        for size in [3, 200_000] {
            let reply: Vec<u8> = (0..size).map(|index| (index % 251) as u8).collect();
            let (client, served) = tokio::join!(connect(&root, "engine.sock"), listener.accept());
            let (mut client, mut served) = (client.unwrap(), served.unwrap());
            let sent = reply.clone();
            let writer = tokio::spawn(async move {
                served.write_all(&sent).await.unwrap();
                drop(served);
            });
            tokio::time::sleep(Duration::from_millis(300)).await;
            let mut bytes = vec![0; size];
            client.read_exact(&mut bytes).await.unwrap();
            writer.await.unwrap();
            assert_eq!(bytes, reply);
        }
    }

    /// D8 (advisor 08:2x): clients one after another each reach a fresh instance, so bytes a
    /// client left unread, or its end of file, never reach the next client's stream.
    #[tokio::test]
    async fn bytes_a_client_left_unread_never_reach_the_next_client() {
        let (_temp, root) = private_root();
        let listener = bind_with(&root, "engine.sock", [7; 16], 2).unwrap();
        for round in 0..3_u8 {
            let (client, served) = tokio::join!(connect(&root, "engine.sock"), listener.accept());
            let (mut client, mut served) = (client.unwrap(), served.unwrap());
            client.write_all(&[round; 4]).await.unwrap();
            let mut bytes = [0; 2];
            served.read_exact(&mut bytes).await.unwrap();
            assert_eq!(bytes, [round; 2]);
            client.write_all(b"left").await.unwrap();
            drop((client, served));
        }
    }

    /// D8 (advisor 08:2x): the engine holds a handle on its pipe name at every step, so
    /// through fifty clients back to back on a name capped at two instances a first instance
    /// create of the name is refused each time, with a client connected and after it left:
    /// access denied for a held name, or pipe busy while the cap is still reached.
    #[tokio::test]
    async fn the_pipe_name_is_never_free_while_clients_come_and_go() {
        let (_temp, root) = private_root();
        let listener = bind_with(&root, "engine.sock", [8; 16], 2).unwrap();
        let name = name_of(&root, "engine.sock", &[8; 16]).unwrap();
        for _ in 0..50 {
            let (client, served) = tokio::join!(connect(&root, "engine.sock"), listener.accept());
            let connected = (client.unwrap(), served.unwrap());
            let squat = pipe::create(&name, true, 2).unwrap_err();
            assert!(matches!(squat.raw_os_error(), Some(5 | 231)), "{squat}");
            drop(connected);
            let squat = pipe::create(&name, true, 2).unwrap_err();
            assert!(matches!(squat.raw_os_error(), Some(5 | 231)), "{squat}");
        }
    }

    /// D8 (advisor 08:2x): with every instance of a name in use, the listener keeps the one
    /// connected last and hands out no stream until a stream ends, and a client of the busy
    /// name waits instead of failing; as each stream ends the next client is served.
    #[tokio::test]
    async fn clients_of_a_busy_pipe_wait_and_are_served_as_streams_end() {
        let (_temp, root) = private_root();
        let listener = bind_with(&root, "engine.sock", [9; 16], 2).unwrap();
        let (first, served) = tokio::join!(connect(&root, "engine.sock"), listener.accept());
        let first = (first.unwrap(), served.unwrap());
        let second = connect(&root, "engine.sock").await.unwrap();
        let third = tokio::spawn({
            let root = root.clone();
            async move { connect(&root, "engine.sock").await }
        });
        let early = tokio::time::timeout(Duration::from_millis(300), listener.accept()).await;
        assert!(
            early.is_err(),
            "no stream is handed out while the name is at its cap"
        );
        drop(first);
        let next = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        drop((second, next));
        let last = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let third = third.await.unwrap().unwrap();
        drop((third, last));
    }

    /// D8 (PLAN D8 scope): clients that open the pipe and leave at once, unread and
    /// unanswered, never end the listener's accept, and the client after them is served.
    #[tokio::test]
    async fn clients_that_open_and_leave_never_end_accept() {
        let (_temp, root) = private_root();
        let listener = bind_with(&root, "engine.sock", [10; 16], 2).unwrap();
        let server = async {
            loop {
                let mut served = listener.accept().await.unwrap();
                let mut bytes = [0; 4];
                if served.read_exact(&mut bytes).await.is_ok() {
                    return bytes;
                }
            }
        };
        let clients = async {
            for _ in 0..5 {
                drop(connect(&root, "engine.sock").await.unwrap());
            }
            let mut client = connect(&root, "engine.sock").await.unwrap();
            client.write_all(b"ping").await.unwrap();
            client
        };
        let (bytes, _client) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(server, clients)
        })
        .await
        .unwrap();
        assert_eq!(&bytes, b"ping");
    }

    /// DESIGN-0256 D10: a client whose pipes file pins another start of the engine refuses
    /// the server it reached before it writes a byte, with the error the Unix check gives.
    #[tokio::test]
    async fn a_server_the_pipes_file_does_not_pin_is_refused() {
        let (_temp, root) = private_root();
        let listener = bind_with(&root, "engine.sock", [11; 16], 2).unwrap();
        let published = read(&root, "engine.sock").unwrap();
        let other = super::Published {
            created: published.created + 1,
            ..published
        };
        let mut file = private::secret_file(&root.join("pipes.other")).unwrap();
        file.write_all(super::encode_pipes("engine.sock", &other).as_bytes())
            .unwrap();
        drop(file);
        std::fs::rename(root.join("pipes.other"), root.join("pipes")).unwrap();
        let (client, _served) = tokio::join!(connect(&root, "engine.sock"), listener.accept());
        let refused = verify_server(&client.unwrap()).unwrap_err();
        assert_eq!(refused.to_string(), "peer_owner_mismatch");
    }

    #[tokio::test]
    async fn an_owner_connect_refuses_until_the_pipe_transport_lands() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("engine.sock");
        let refused = super::owner_connect(&path).await.unwrap_err();
        assert_eq!(refused.to_string(), "unix_socket_unavailable");
    }
}
