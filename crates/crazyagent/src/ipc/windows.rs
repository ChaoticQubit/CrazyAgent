#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the owner-class wiring slice is the Windows listener's first caller"
    )
)]

use super::{PIPES_BYTES_MAX, Published, encode_pipes, parse_pipes, pipe_name};
use crazyagent_win_sandbox::pipe::{self, Peer};
use crazyagent_win_sandbox::private;
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsHandle;
use std::path::Path;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer};
use tokio::sync::{Mutex, mpsc};

/// The D7 pipes file's name in its protected root.
const PIPES_FILE: &str = "pipes";
/// The most entries one sweep of a protected root reads; a root holds a handful.
const ROOT_ENTRIES_MAX: usize = 1024;
/// How many times publish tries the rename over `pipes` while a client holds it open.
const RENAME_TRIES: u32 = 50;
/// The pause between two of those tries; all of them together wait about one second.
const RENAME_PAUSE: Duration = Duration::from_millis(20);
/// Windows' ERROR_ACCESS_DENIED.
const ACCESS_DENIED: i32 = 5;
/// Windows' ERROR_SHARING_VIOLATION: another handle's share mode refuses the access.
const SHARING_VIOLATION: i32 = 32;
/// Windows' ERROR_PIPE_BUSY: every instance of the name is in use, or the cap is reached.
const PIPE_BUSY: i32 = 231;
/// The most instances of one engine pipe name: the streams open at once, the instance
/// listening and the one made to replace it.
const INSTANCES: u32 = 64;
/// How many served streams wait for [`PipeListener::accept`] before the listener waits too.
const ACCEPTED_MAX: usize = 1;
/// How long a client tries a name whose every instance is in use before it gives up.
const BUSY_WAIT: Duration = Duration::from_secs(5);
/// The pause between two tries on a busy name, by a client or by the listener.
const BUSY_PAUSE: Duration = Duration::from_millis(20);
/// The impersonation level a client allows its server, SECURITY_IDENTIFICATION: the server
/// may read who the client is (D9), never act as it.
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;

/// The D8 Windows listener of one owner-class endpoint (DESIGN-0256 D8, advisor 08:2x, leader
/// 08:25): one task serves the pipe name with a fresh instance per client and holds a handle
/// on the name at every step. Dropping it stops that task; streams handed out live on.
#[derive(Debug)]
pub(crate) struct PipeListener {
    accepted: Mutex<mpsc::Receiver<PipeStream>>,
    task: tokio::task::JoinHandle<()>,
}

/// One end of a connected engine pipe: a served client with what D9 read of it when it
/// connected, or a client with the D10 pin its pipes file named for the server.
#[derive(Debug)]
pub(crate) struct PipeStream {
    end: End,
}

/// Which end of an engine pipe a [`PipeStream`] holds.
#[derive(Debug)]
enum End {
    Served {
        server: NamedPipeServer,
        peer: io::Result<Peer>,
    },
    Client {
        client: NamedPipeClient,
        expected: pipe::Pin,
    },
}

/// Binds the owner-class endpoint `purpose` in its protected root on Windows (DESIGN-0256
/// D7-D8): a fresh random part, then [`bind_with`] at [`INSTANCES`]. Must run inside a Tokio
/// runtime. Inputs: the root and the purpose. Output: the listener, or the error.
pub(super) fn bind(root: &Path, purpose: &str) -> io::Result<PipeListener> {
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(|_| io::Error::other("pipe_entropy_unavailable"))?;
    bind_with(root, purpose, random, INSTANCES)
}

/// Creates the first instance of the endpoint's pipe name, refused when the name exists, and
/// only then publishes the pipes file naming it (V244-WIN-PIPES-LISTENER-REQS, leader 08:08),
/// then starts the task serving the name. Must run inside a Tokio runtime. Inputs: the root,
/// the purpose, the random part and the instance cap, 2 or more. Output: the listener.
pub(super) fn bind_with(
    root: &Path,
    purpose: &str,
    random: [u8; 16],
    instances: u32,
) -> io::Result<PipeListener> {
    assert!(
        (2..=pipe::INSTANCES_MAX).contains(&instances),
        "a listener holds the connected instance while it makes the next"
    );
    let name = name_of(root, purpose, &random)?;
    let first = pipe::instance(&name, true, instances)?;
    let published = publish(root, purpose, random)?;
    assert_eq!(published.random, random);
    let (handed, accepted) = mpsc::channel(ACCEPTED_MAX);
    let task = tokio::spawn(serve(name, instances, first, handed));
    Ok(PipeListener {
        accepted: Mutex::new(accepted),
        task,
    })
}

/// Serves one pipe name until the listener drops (advisor 08:2x): waits for a client on the
/// listening instance, makes its replacement before the connected one is handed out or
/// dropped, reads its client (D9), then hands it to `accept`. Inputs: the name, the cap, the
/// listening instance and the sender. Output: none; it ends with the listener or a failed
/// create.
async fn serve(
    name: String,
    instances: u32,
    mut listening: NamedPipeServer,
    handed: mpsc::Sender<PipeStream>,
) {
    loop {
        let connected = listening.connect().await;
        let Some(next) = replacement(&name, instances, &handed).await else {
            return;
        };
        let server = std::mem::replace(&mut listening, next);
        if connected.is_err() {
            drop(server);
            continue;
        }
        let peer = client_of(&server).await;
        let stream = PipeStream {
            end: End::Served { server, peer },
        };
        if handed.send(stream).await.is_err() {
            return;
        }
    }
}

/// Makes the instance that replaces the one a client just connected to, so the engine always
/// holds a handle on its name. A busy name means every instance is still in use, so it waits
/// for a stream to end and tries again while the listener lives. Inputs: the name, the cap
/// and the sender. Output: the new instance, or None once the listener or the create failed.
async fn replacement(
    name: &str,
    instances: u32,
    handed: &mpsc::Sender<PipeStream>,
) -> Option<NamedPipeServer> {
    loop {
        match pipe::instance(name, false, instances) {
            Ok(next) => return Some(next),
            Err(error) if error.raw_os_error() == Some(PIPE_BUSY) && !handed.is_closed() => {
                tokio::time::sleep(BUSY_PAUSE).await;
            }
            Err(_) => return None,
        }
    }
}

/// Reads who is at the client end of a just connected instance (DESIGN-0140 D9) on a blocking
/// thread, through a duplicate of its handle, so a cancelled read never leaves the check on
/// a closed handle whose value was reused. Input: the server end. Output: the peer, or the
/// error.
async fn client_of(server: &NamedPipeServer) -> io::Result<Peer> {
    let instance = server.as_handle().try_clone_to_owned()?;
    match tokio::task::spawn_blocking(move || pipe::client(instance.as_handle())).await {
        Ok(peer) => peer,
        Err(_) => Err(io::Error::other("pipe_peer_check_stopped")),
    }
}

impl PipeListener {
    /// Accepts one owner-class connection (DESIGN-0256 D8): the next client the listener
    /// served, whose check [`verify_client`] judges. Output: the stream, or
    /// `pipe_listener_stopped` once the listener can serve no more clients.
    pub(crate) async fn accept(&self) -> io::Result<PipeStream> {
        self.accepted
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| io::Error::other("pipe_listener_stopped"))
    }
}

impl Drop for PipeListener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Connects to the owner-class endpoint `purpose` in its protected root (DESIGN-0256 D7,
/// D10): the pipe name and the server's pin come from the pipes file, read through the
/// private judge, and a busy name is tried again for up to [`BUSY_WAIT`]. Inputs: the root
/// and the purpose. Output: the client end, whose server [`verify_server`] judges.
pub(super) async fn connect(root: &Path, purpose: &str) -> io::Result<PipeStream> {
    let published = read(root, purpose)?;
    let name = name_of(root, purpose, &published.random)?;
    let expected = pipe::Pin {
        pid: published.pid,
        created: published.created,
    };
    let deadline = tokio::time::Instant::now() + BUSY_WAIT;
    loop {
        match ClientOptions::new()
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(&name)
        {
            Ok(client) => {
                return Ok(PipeStream {
                    end: End::Client { client, expected },
                });
            }
            Err(error)
                if error.raw_os_error() == Some(PIPE_BUSY)
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(BUSY_PAUSE).await;
            }
            Err(error) => return Err(error),
        }
    }
}

/// The server side's check of an accepted stream (DESIGN-0140 D9): what was read of its
/// client when it connected is this user, in no AppContainer, unrestricted and at medium or
/// above. Input: the stream. Output: Ok, `peer_owner_mismatch`, or the error reading the
/// client.
pub(super) fn verify_client(stream: &PipeStream) -> io::Result<()> {
    let End::Served { peer, .. } = &stream.end else {
        return Err(io::Error::other("a client end has no client to check"));
    };
    let peer = peer.as_ref().map_err(copied)?;
    let user = private::user_sid()?;
    pipe::judge_peer(peer, &user).map_err(|_| io::Error::other("peer_owner_mismatch"))
}

/// The client side's check, right after connect and before any byte is written (DESIGN-0256
/// D10): the server is the engine start the pipes file pinned, runs as this user and serves
/// a pipe no lower integrity process labelled. Input: the stream. Output: Ok,
/// `peer_owner_mismatch`, or the error reading the server.
pub(super) fn verify_server(stream: &PipeStream) -> io::Result<()> {
    let End::Client { client, expected } = &stream.end else {
        return Err(io::Error::other("a served end has no server to check"));
    };
    let served = pipe::server(client.as_handle())?;
    let user = private::user_sid()?;
    pipe::judge_server(&served, *expected, &user)
        .map_err(|_| io::Error::other("peer_owner_mismatch"))
}

/// A copy of an error a stream keeps, for a check judged more than once. Input: the error.
/// Output: an error with the same Windows code, or of the same kind and text.
fn copied(error: &io::Error) -> io::Error {
    match error.raw_os_error() {
        Some(code) => io::Error::from_raw_os_error(code),
        None => io::Error::new(error.kind(), error.to_string()),
    }
}

/// The pipe name of the endpoint `purpose` in a protected root (DESIGN-0256 D7): the root's
/// final path as Windows resolves it, hashed into [`pipe_name`] with the random part.
/// Inputs: the root, the purpose and the random part. Output: the name, or the error
/// resolving the root.
pub(super) fn name_of(root: &Path, purpose: &str, random: &[u8; 16]) -> io::Result<String> {
    let resolved: Vec<u16> = std::fs::canonicalize(root)?
        .as_os_str()
        .encode_wide()
        .collect();
    Ok(pipe_name(&resolved, purpose, random))
}

impl AsyncRead for PipeStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut self.get_mut().end {
            End::Served { server, .. } => std::pin::Pin::new(server).poll_read(context, buffer),
            End::Client { client, .. } => std::pin::Pin::new(client).poll_read(context, buffer),
        }
    }
}

impl AsyncWrite for PipeStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut self.get_mut().end {
            End::Served { server, .. } => std::pin::Pin::new(server).poll_write(context, bytes),
            End::Client { client, .. } => std::pin::Pin::new(client).poll_write(context, bytes),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut self.get_mut().end {
            End::Served { server, .. } => std::pin::Pin::new(server).poll_flush(context),
            End::Client { client, .. } => std::pin::Pin::new(client).poll_flush(context),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut self.get_mut().end {
            End::Served { server, .. } => std::pin::Pin::new(server).poll_shutdown(context),
            End::Client { client, .. } => std::pin::Pin::new(client).poll_shutdown(context),
        }
    }
}

/// Removes the staged pipes files earlier starts left in a held root, each judged private
/// before it goes, and leaves every other entry (rv-code2 #554 N4). Input: the root. Output:
/// Ok, or the error reading the root, judging a leftover or removing it.
fn sweep(root: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(root)?.take(ROOT_ENTRIES_MAX) {
        let name = entry?.file_name();
        let Some(name) = name.to_str().filter(|name| staged_name(name)) else {
            continue;
        };
        let leftover = root.join(name);
        drop(private::read_only(&leftover)?);
        std::fs::remove_file(&leftover)?;
    }
    Ok(())
}

/// Renames the staged pipes file over `pipes` (rv-code2 #554 N3). A client reading `pipes`
/// holds it without delete sharing for the moment its read takes, so a rename refused for
/// that is tried again, at most [`RENAME_TRIES`] times. Inputs: the staged file and the
/// target. Output: Ok, or the last rename error.
fn replace(staged: &Path, target: &Path) -> io::Result<()> {
    let mut tries = 1;
    loop {
        match std::fs::rename(staged, target) {
            Err(error) if held_open(&error) && tries < RENAME_TRIES => {
                tries += 1;
                std::thread::sleep(RENAME_PAUSE);
            }
            done => return done,
        }
    }
}

/// Whether a rename failed because another handle holds the target open: Windows' sharing
/// violation, or access denied, which a rename over a file open without delete sharing may
/// return. Input: the error. Output: whether to try again.
fn held_open(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(SHARING_VIOLATION | ACCESS_DENIED)
    )
}

/// Whether a root entry is a staged pipes file: `pipes.`, 16 lowercase hex, `.new`. Input:
/// the entry name. Output: whether publish made it.
fn staged_name(name: &str) -> bool {
    name.strip_prefix(PIPES_FILE)
        .and_then(|rest| rest.strip_prefix('.'))
        .and_then(|rest| rest.strip_suffix(".new"))
        .is_some_and(|hex| {
            hex.len() == 16
                && hex
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
}

/// Publishes a start of the endpoint `purpose` in its protected root (DESIGN-0256 D7, leader
/// 07:46): the caller's random part and this engine's pin, staged in a file created private
/// and closed to lower integrity opens before its first byte (#555 N1, #562 F1), renamed over
/// `pipes` in the held root, then read back through the private judge; a `pipes` file it
/// refuses stays. Inputs: the root, the purpose and the random part. Output: what was published.
pub(super) fn publish(root: &Path, purpose: &str, random: [u8; 16]) -> io::Result<Published> {
    let held = private::dir(root)?;
    let target = root.join(PIPES_FILE);
    match private::read_only(&target) {
        Ok(kept) => drop(kept),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    sweep(root)?;
    let mut stage = [0; 8];
    getrandom::fill(&mut stage).map_err(|_| io::Error::other("pipe_entropy_unavailable"))?;
    let pin = pipe::own_pin()?;
    let published = Published {
        random,
        pid: pin.pid,
        created: pin.created,
    };
    let stage: String = stage.iter().map(|byte| format!("{byte:02x}")).collect();
    let staged = root.join(format!("{PIPES_FILE}.{stage}.new"));
    let mut file = private::secret_file(&staged)?;
    let written = file
        .write_all(encode_pipes(purpose, &published).as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written.and_then(|()| replace(&staged, &target)) {
        let _ = std::fs::remove_file(&staged);
        return Err(error);
    }
    private::sync_folder(root)?;
    drop(held);
    if read(root, purpose)? != published {
        return Err(io::Error::other(
            "the pipes file changed while the engine published it",
        ));
    }
    Ok(published)
}

/// Reads the endpoint `purpose`'s D7 pipes file in its protected root through the private
/// judge, so only a file private to this user names a pipe or its server (DESIGN-0256 D7).
/// Inputs: the root and the purpose. Output: what the engine published, or why the file is
/// not trusted.
pub(super) fn read(root: &Path, purpose: &str) -> io::Result<Published> {
    let file = private::read_only(&root.join(PIPES_FILE))?;
    let mut bytes = Vec::with_capacity(PIPES_BYTES_MAX + 1);
    (&file)
        .take(PIPES_BYTES_MAX as u64 + 1)
        .read_to_end(&mut bytes)?;
    parse_pipes(&bytes, purpose)
        .map_err(|reason| io::Error::new(io::ErrorKind::InvalidData, reason))
}
