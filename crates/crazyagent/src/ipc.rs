use std::io;
use std::path::Path;

#[cfg(unix)]
pub(crate) use crate::platform::unix_socket::{
    OwnedReadHalf, OwnedWriteHalf, ReadHalf, UnixListener as Listener, UnixStream as Stream,
};
#[cfg(windows)]
pub(crate) use windows::{
    OwnedReadHalf, OwnedWriteHalf, PipeListener as Listener, PipeStream as Stream, ReadHalf,
};

/// Binds the owner-class listener at `path` for at most `streams` streams at once: on Unix a
/// socket made inside the spawn gate, guarded by its parent, the protected root; on Windows
/// the named pipe of that root and the file name, with `streams + 1` instances (DESIGN-0256
/// D7-D8). Must run inside a Tokio runtime. Inputs: the endpoint path and the stream bound,
/// 1 or more. Output: the listener.
pub(crate) fn owner_listener(path: impl AsRef<Path>, streams: usize) -> io::Result<Listener> {
    assert!(streams > 0, "a listener serves at least one stream");
    #[cfg(unix)]
    {
        crate::spawn_gate::bind_unix(path)
    }
    #[cfg(windows)]
    {
        let (root, purpose) = endpoint(path.as_ref())?;
        windows::bind(root, purpose, instances(streams))
    }
}

/// Accepts one owner-class connection; on Unix the spawn gate is taken only for each accept
/// attempt. Input: the listener. Output: the stream, whose peer is not checked yet.
pub(crate) async fn owner_accept(listener: &Listener) -> io::Result<Stream> {
    #[cfg(unix)]
    {
        crate::spawn_gate::accept_unix(listener)
            .await
            .map(|(stream, _)| stream)
    }
    #[cfg(windows)]
    {
        listener.accept().await
    }
}

/// Connects to the owner-class endpoint at `path`, whose server is not checked yet; on
/// Windows the pipe name and the server's pin come from the root's pipes file. Input: the
/// endpoint path. Output: the stream.
pub(crate) async fn owner_connect(path: impl AsRef<Path>) -> io::Result<Stream> {
    #[cfg(unix)]
    {
        crate::spawn_gate::connect_unix(path).await
    }
    #[cfg(windows)]
    {
        let (root, purpose) = endpoint(path.as_ref())?;
        windows::connect(root, purpose).await
    }
}

/// Says whether a checked engine serves the owner-class endpoint at `path` now, writing no
/// byte to it (V244-WIN-ENGINE-WIRING decision 6): the server must pass the client's whole
/// check. Input: the endpoint path. Output: the answer; any failure reads false.
#[cfg(windows)]
pub(crate) fn owner_answering(path: &Path) -> bool {
    endpoint(path).is_ok_and(|(root, purpose)| windows::answering(root, purpose))
}

/// The endpoint the pipes file at the owner-class `path` publishes now, read through W1's
/// private read-only open: its random part and the pin of the engine serving it, the
/// identity a client compares before and after it connects (V244-WIN-ENGINE-WIRING W3-3).
/// Input: the endpoint path. Output: the endpoint, or why the file is refused.
#[cfg(windows)]
pub(crate) fn owner_endpoint(path: &Path) -> io::Result<Published> {
    let (root, purpose) = endpoint(path)?;
    windows::read(root, purpose)
}

/// The endpoint a Windows listener published at `path`, read from the pipes file it holds
/// rather than by path (V244-WIN-ENGINE-WIRING W3-3, rv-sec2 #719 C2). Inputs: the listener
/// and its endpoint path. Output: the published random part and pin.
#[cfg(windows)]
pub(crate) fn owner_listened(listener: &Listener, path: &Path) -> io::Result<Published> {
    let (_, purpose) = endpoint(path)?;
    listener.published(purpose)
}

/// Closes a Windows listener and deletes its pipes file, reporting a delete that failed
/// (V244-WIN-ENGINE-WIRING WIRE-2). Input: the listener. Output: Ok, or the delete's error.
#[cfg(windows)]
pub(crate) fn owner_close(listener: Listener) -> io::Result<()> {
    listener.close()
}

/// Test only: publishes a pipes file at `path`'s root that pins another start of this
/// process and serves its name from here, as a rogue server would (R1). Input: the endpoint
/// path. Output: the rogue's first instance.
#[cfg(all(test, windows))]
pub(crate) fn serve_unpinned(
    path: &Path,
) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    use std::io::Write;
    let (root, purpose) = endpoint(path)?;
    let own = crazyagent_win_sandbox::pipe::own_pin()?;
    let rogue = Published {
        random: [13; 16],
        pid: own.pid,
        created: own.created + 1,
    };
    let staged = root.join("pipes.rogue");
    let mut file = crazyagent_win_sandbox::private::secret_file(&staged)?;
    file.write_all(encode_pipes(purpose, &rogue).as_bytes())?;
    drop(file);
    std::fs::rename(&staged, root.join("pipes"))?;
    let name = windows::name_of(root, purpose, &rogue.random)?;
    crazyagent_win_sandbox::pipe::instance(&name, true, 2)
}

/// Test only: the pipe name the pipes file at `path`'s root names. Input: the endpoint
/// path. Output: the name.
#[cfg(all(test, windows))]
pub(crate) fn pipe_name_at(path: &Path) -> io::Result<String> {
    let (root, purpose) = endpoint(path)?;
    let published = windows::read(root, purpose)?;
    windows::name_of(root, purpose, &published.random)
}

/// The server side's check of an accepted stream: its client runs as this user (on Windows
/// also in no AppContainer, unrestricted and at medium or above). Input: the stream. Output:
/// Ok, `peer_owner_mismatch`, or the error reading the peer.
pub(crate) fn verify_client(stream: &Stream) -> io::Result<()> {
    #[cfg(unix)]
    {
        same_user(stream)
    }
    #[cfg(windows)]
    {
        windows::verify_client(stream)
    }
}

/// The client side's check, right after connect and before any byte is written: the server
/// runs as this user (on Windows it is also the engine start the pipes file pins). Input:
/// the stream. Output: Ok, `peer_owner_mismatch`, or the error reading the peer.
pub(crate) fn verify_server(stream: &Stream) -> io::Result<()> {
    #[cfg(unix)]
    {
        same_user(stream)
    }
    #[cfg(windows)]
    {
        windows::verify_server(stream)
    }
}

/// Splits an owner-class endpoint path into the protected root that holds it and its
/// purpose, the file name, which D7 places verbatim in the pipe name (V244-WIN-ENGINE-WIRING
/// decision 1). Input: the endpoint path. Output: the root and the purpose, or
/// `ipc_endpoint_not_plain`.
#[cfg(any(windows, test))]
fn endpoint(path: &Path) -> io::Result<(&Path, &str)> {
    let purpose = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| plain_purpose(name));
    let root = path.parent().filter(|root| !root.as_os_str().is_empty());
    match (root, purpose) {
        (Some(root), Some(purpose)) => Ok((root, purpose)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ipc_endpoint_not_plain",
        )),
    }
}

/// Windows' cap on the instances of one pipe name (win-sandbox `pipe::INSTANCES_MAX`).
#[cfg(any(windows, test))]
const PIPE_INSTANCES_MAX: u32 = 254;

/// The pipe instances a Windows listener of `streams` streams makes: one per stream plus the
/// one listening while a stream is handed off (DESIGN-0256 D8 caps). Input: the stream bound,
/// 1 to 253. Output: the instance cap.
#[cfg(any(windows, test))]
fn instances(streams: usize) -> u32 {
    assert!(streams > 0, "a listener serves at least one stream");
    let cap = u32::try_from(streams)
        .ok()
        .and_then(|streams| streams.checked_add(1))
        .filter(|cap| *cap <= PIPE_INSTANCES_MAX);
    cap.expect("a listener's streams fit Windows' instance cap")
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::FileTypeExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn an_owner_endpoint_serves_a_client_of_this_user_at_its_path() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("engine.sock");
        let listener = owner_listener(&path, 1).unwrap();
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
mod endpoint_tests {
    use super::{endpoint, instances};
    use std::path::Path;

    /// V244-WIN-ENGINE-WIRING decision 1: an owner-class endpoint path names its protected
    /// root, the folder holding it, and its purpose, the file name, so every caller keeps
    /// passing the path it passes on Unix.
    #[test]
    fn an_endpoint_path_names_its_root_and_its_file_name_as_the_purpose() {
        let root = Path::new("data").join("o00000001");
        for name in ["engine.sock", "owner.sock", "control.sock", "e.sock"] {
            let path = root.join(name);
            let (held, purpose) = endpoint(&path).unwrap();
            assert_eq!(held, root.as_path());
            assert_eq!(purpose, name);
        }
    }

    /// Decision 1: a file name outside D7's plain shape, or a path with no folder above it,
    /// names no endpoint, and the refusal is an error the caller reports, never a panic. A
    /// backslash is a name byte on Unix, refused, and a separator on Windows (farm run
    /// 44709311585).
    #[test]
    fn an_endpoint_path_outside_the_plain_shape_is_refused() {
        let long = "a".repeat(33);
        let root = Path::new("data");
        for name in ["Engine.sock", "engine sock", "e+s", long.as_str()] {
            let refused = endpoint(&root.join(name)).unwrap_err();
            assert_eq!(refused.to_string(), "ipc_endpoint_not_plain", "{name:?}");
        }
        #[cfg(unix)]
        assert_eq!(
            endpoint(&root.join("e\\s")).unwrap_err().to_string(),
            "ipc_endpoint_not_plain"
        );
        #[cfg(windows)]
        assert_eq!(
            endpoint(&root.join("e\\s")).unwrap(),
            (Path::new("data\\e"), "s")
        );
        for path in [Path::new("engine.sock"), Path::new("")] {
            let refused = endpoint(path).unwrap_err();
            assert_eq!(refused.to_string(), "ipc_endpoint_not_plain", "{path:?}");
        }
    }

    /// Decision 2 (DESIGN-0256 D8 caps): a listener of n streams makes n + 1 pipe instances,
    /// one listening while a stream is handed off: the engine 129, the owner 33.
    #[test]
    fn a_listener_of_n_streams_makes_n_plus_one_instances() {
        assert_eq!(instances(1), 2);
        assert_eq!(instances(32), 33);
        assert_eq!(instances(128), 129);
        assert_eq!(instances(253), 254);
    }

    /// Decision 2: no streams, or more than Windows' instance cap allows, is a programming
    /// error in the caller's bound.
    #[test]
    fn a_stream_count_outside_the_instance_cap_is_a_programming_error() {
        for streams in [0, 254, usize::MAX] {
            assert!(
                std::panic::catch_unwind(|| instances(streams)).is_err(),
                "{streams}"
            );
        }
    }
}

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

    /// W3-3 D2: two endpoints are the same only when the random part, the process ID and
    /// the creation time all match, so a client's compare sees a change in any one.
    #[test]
    fn an_endpoint_equals_another_only_on_random_pid_and_created() {
        assert_eq!(PUBLISHED, PUBLISHED);
        let mut random = PUBLISHED;
        random.random[15] ^= 1;
        let pid = Published {
            pid: PUBLISHED.pid + 1,
            ..PUBLISHED
        };
        let created = Published {
            created: PUBLISHED.created + 1,
            ..PUBLISHED
        };
        for other in [random, pid, created] {
            assert_ne!(other, PUBLISHED);
        }
    }
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

    /// V244-WIN-PIPES-LISTENER-REQS (rv-sec3 #555 N1, rv-code #562 F1): the pipes file carries
    /// a medium label with no-write-up, no-read-up and no-execute-up from its creation, in a
    /// root that holds no label of its own, so no lower integrity process can open it, hold it
    /// and so block the next start's rename.
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
            Ok(Some((0x2000, 0x7)))
        );
    }

    /// D7, #555 N1 and #562 F1 in the engine's real root, the database store folder, whose
    /// label passes to the files made in it: the pipes file still reads back the same and
    /// carries its own medium no-write-up, no-read-up and no-execute-up label.
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
            Ok(Some((0x2000, 0x7)))
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
        let listener = bind(&root, "engine.sock", 3).unwrap();
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
    /// The listener holds the pipes file for its life (rv-sec2 #702 5a), so this row serves
    /// the name itself, under a pipes file that pins another start of this process.
    #[tokio::test]
    async fn a_server_the_pipes_file_does_not_pin_is_refused() {
        let (_temp, root) = private_root();
        let own = pipe::own_pin().unwrap();
        let other = super::Published {
            random: [11; 16],
            pid: own.pid,
            created: own.created + 1,
        };
        let mut file = private::secret_file(&root.join("pipes.other")).unwrap();
        file.write_all(super::encode_pipes("engine.sock", &other).as_bytes())
            .unwrap();
        drop(file);
        std::fs::rename(root.join("pipes.other"), root.join("pipes")).unwrap();
        let name = name_of(&root, "engine.sock", &[11; 16]).unwrap();
        let served = pipe::instance(&name, true, 2).unwrap();
        let (client, _) = tokio::join!(connect(&root, "engine.sock"), served.connect());
        let refused = verify_server(&client.unwrap()).unwrap_err();
        assert_eq!(refused.to_string(), "peer_owner_mismatch");
    }

    /// V244-WIN-ENGINE-WIRING decision 1: the owner-class entry points every caller uses serve
    /// a client of this user at a path on Windows, the root being its folder and the purpose
    /// its file name, with both checks passing.
    #[tokio::test]
    async fn an_owner_endpoint_at_a_path_serves_a_client_of_this_user() {
        let (_temp, root) = private_root();
        let path = root.join("engine.sock");
        let listener = super::owner_listener(&path, 2).unwrap();
        assert_eq!(read(&root, "engine.sock").unwrap().pid, std::process::id());
        let (client, served) =
            tokio::join!(super::owner_connect(&path), super::owner_accept(&listener));
        let (mut client, mut served) = (client.unwrap(), served.unwrap());
        super::verify_server(&client).unwrap();
        super::verify_client(&served).unwrap();
        client.write_all(b"ping").await.unwrap();
        let mut bytes = [0; 4];
        served.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"ping");
    }

    /// Decision 3: the halves of a pipe stream, owned and borrowed, carry bytes both ways at
    /// once, so a terminal's output and its typed input never wait on each other.
    #[tokio::test]
    async fn the_halves_of_a_pipe_stream_run_both_ways_at_once() {
        let (_temp, root) = private_root();
        let path = root.join("owner.sock");
        let listener = super::owner_listener(&path, 2).unwrap();
        let (client, served) =
            tokio::join!(super::owner_connect(&path), super::owner_accept(&listener));
        let (mut client, served) = (client.unwrap(), served.unwrap());
        super::verify_server(&client).unwrap();
        let (mut from_client, mut to_client) = served.into_split();
        let echo = tokio::spawn(async move {
            tokio::io::copy(&mut from_client, &mut to_client)
                .await
                .unwrap()
        });
        let sent: Vec<u8> = (0..400_000).map(|index| (index % 251) as u8).collect();
        let (mut reader, mut writer) = client.split();
        let mut back = vec![0; sent.len()];
        let both = async { tokio::join!(writer.write_all(&sent), reader.read_exact(&mut back)) };
        let (written, read_back) = tokio::time::timeout(Duration::from_secs(20), both)
            .await
            .expect("both halves move at once");
        written.unwrap();
        read_back.unwrap();
        assert_eq!(back, sent);
        drop(client);
        assert_eq!(echo.await.unwrap(), sent.len() as u64);
    }

    /// Decision 2: a listener of one stream makes two instances, so with one stream served
    /// and a second client on the listening instance, a third client gets pipe busy once the
    /// client's wait runs out, instead of an instance past the cap.
    #[tokio::test]
    async fn a_listener_refuses_the_client_past_its_cap_with_pipe_busy() {
        let (_temp, root) = private_root();
        let path = root.join("engine.sock");
        let listener = super::owner_listener(&path, 1).unwrap();
        let (first, served) =
            tokio::join!(super::owner_connect(&path), super::owner_accept(&listener));
        let _first = (first.unwrap(), served.unwrap());
        let _second = super::owner_connect(&path).await.unwrap();
        let third = tokio::time::timeout(Duration::from_secs(15), super::owner_connect(&path))
            .await
            .expect("the client's own wait ends first");
        assert_eq!(third.unwrap_err().raw_os_error(), Some(231));
    }

    /// Decision 1: a root holds one endpoint, so a bind over a pipes file naming another
    /// purpose is refused with `pipes_purpose_taken`, and that file stays as it was.
    #[tokio::test]
    async fn a_bind_over_a_pipes_file_of_another_endpoint_is_refused() {
        let (_temp, root) = private_root();
        let kept = publish(&root, "owner.sock", [12; 16]).unwrap();
        let refused = super::owner_listener(root.join("engine.sock"), 2).unwrap_err();
        assert_eq!(refused.to_string(), "pipes_purpose_taken");
        assert_eq!(read(&root, "owner.sock").unwrap(), kept);
    }

    /// rv-sec2 #702 5a: while a listener lives it holds its root and its pipes file, so
    /// neither is renamed or replaced; once it drops, the pipes file is gone.
    #[tokio::test]
    async fn a_listener_holds_its_root_and_pipes_file_and_removes_the_file_at_drop() {
        let (temp, root) = private_root();
        let listener = super::owner_listener(root.join("engine.sock"), 2).unwrap();
        let moved = std::fs::rename(&root, temp.path().join("moved")).unwrap_err();
        assert!(matches!(moved.raw_os_error(), Some(5 | 32)), "{moved}");
        let mut file = private::secret_file(&root.join("pipes.swap")).unwrap();
        file.write_all(b"swap").unwrap();
        drop(file);
        let swapped = std::fs::rename(root.join("pipes.swap"), root.join("pipes")).unwrap_err();
        assert!(matches!(swapped.raw_os_error(), Some(5 | 32)), "{swapped}");
        std::fs::remove_file(root.join("pipes.swap")).unwrap();
        drop(listener);
        assert!(entries(&root).is_empty(), "{:?}", entries(&root));
    }

    /// Decision 6: `answering` is true only while a listener serves the endpoint and its
    /// server passes the client's whole check, and it writes no byte to the engine.
    #[tokio::test]
    async fn answering_is_true_only_while_a_checked_listener_serves() {
        let (_temp, root) = private_root();
        let path = root.join("engine.sock");
        assert!(!super::owner_answering(&path));
        let listener = super::owner_listener(&path, 2).unwrap();
        let probe = tokio::task::spawn_blocking({
            let path = path.clone();
            move || super::owner_answering(&path)
        });
        let mut served = super::owner_accept(&listener).await.unwrap();
        assert!(probe.await.unwrap());
        let mut bytes = Vec::new();
        served.read_to_end(&mut bytes).await.unwrap();
        assert!(bytes.is_empty(), "{bytes:?}");
        drop(listener);
        assert!(!super::owner_answering(&path));
        publish(&root, "engine.sock", [13; 16]).unwrap();
        assert!(
            !super::owner_answering(&path),
            "a pinned start with no pipe"
        );
    }
}
