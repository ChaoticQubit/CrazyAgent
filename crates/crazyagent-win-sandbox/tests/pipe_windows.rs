#![cfg(windows)]

use crazyagent_win_sandbox::pipe::{self, Peer, Served};
use crazyagent_win_sandbox::private::user_sid;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_IO_PENDING, ERROR_NO_TOKEN, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
    HANDLE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
use windows_sys::Win32::Security::{
    CreateRestrictedToken, DISABLE_MAX_PRIVILEGE, DuplicateTokenEx, GetLengthSid, PSID,
    RevertToSelf, SID_AND_ATTRIBUTES, SecurityImpersonation, SetTokenInformation, TOKEN_ALL_ACCESS,
    TOKEN_DUPLICATE, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_TYPE, TokenImpersonation,
    TokenIntegrityLevel, TokenPrimary,
};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, SECURITY_IDENTIFICATION};
use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, ImpersonateNamedPipeClient};
use windows_sys::Win32::System::SystemServices::SE_GROUP_INTEGRITY;
use windows_sys::Win32::System::Threading::{
    CreateProcessAsUserW, GetCurrentProcess, GetCurrentThread, GetExitCodeProcess,
    OpenProcessToken, OpenThreadToken, PROCESS_INFORMATION, STARTUPINFOW, SetThreadToken,
    TerminateProcess, WaitForSingleObject,
};

/// The low mandatory level, the level of a sandboxed browser tab or a lowered helper.
const LOW: &str = "S-1-16-4096";
/// The RESTRICTED SID, the usual restricting SID of a restricted token.
const RESTRICTED: &str = "S-1-5-12";

/// A pipe name unique to this run and call, in the D7 shape. Input: the purpose. Output:
/// the full name.
fn unique(purpose: &str) -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let next = NEXT.fetch_add(1, Ordering::Relaxed);
    format!(
        r"\\.\pipe\crazyagent-p2-{purpose}-{}-{next}",
        std::process::id()
    )
}

/// Converts a SID string, freeing nothing: the caller frees the SID with `LocalFree`.
/// Input: the SID string. Output: the SID.
fn sid(text: &str) -> PSID {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let mut sid: PSID = null_mut();
    assert_ne!(
        unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut sid) },
        0,
        "{}",
        io::Error::last_os_error()
    );
    sid
}

/// This process's token duplicated as a token of `kind` at impersonation level. Input:
/// primary or impersonation. Output: the copy.
fn own_token(kind: TOKEN_TYPE) -> OwnedHandle {
    let mut process = null_mut();
    assert_ne!(
        unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_QUERY,
                &mut process,
            )
        },
        0
    );
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let mut copy = null_mut();
    let made = unsafe {
        DuplicateTokenEx(
            process.as_raw_handle(),
            TOKEN_ALL_ACCESS,
            null(),
            SecurityImpersonation,
            kind,
            &mut copy,
        )
    };
    assert_ne!(made, 0, "{}", io::Error::last_os_error());
    unsafe { OwnedHandle::from_raw_handle(copy) }
}

/// A copy of this process's token lowered to the low mandatory level. Input: primary or
/// impersonation. Output: the token.
fn low_token(kind: TOKEN_TYPE) -> OwnedHandle {
    let token = own_token(kind);
    let level = sid(LOW);
    let label = TOKEN_MANDATORY_LABEL {
        Label: SID_AND_ATTRIBUTES {
            Sid: level,
            Attributes: SE_GROUP_INTEGRITY as u32,
        },
    };
    let size = size_of::<TOKEN_MANDATORY_LABEL>() as u32 + unsafe { GetLengthSid(level) };
    let set = unsafe {
        SetTokenInformation(
            token.as_raw_handle(),
            TokenIntegrityLevel,
            (&raw const label).cast(),
            size,
        )
    };
    let failure = io::Error::last_os_error();
    unsafe { LocalFree(level) };
    assert_ne!(set, 0, "{failure}");
    token
}

/// A restricted impersonation copy of this process's token with RESTRICTED as its
/// restricting SID, which no engine pipe list grants. Output: the token.
fn restricted_token() -> OwnedHandle {
    let restricting = sid(RESTRICTED);
    let token = restricted_to(restricting);
    unsafe { LocalFree(restricting) };
    token
}

/// The user's own SID copied into 4-byte words, so Windows reads it aligned. Output: the
/// words.
fn user_words() -> Vec<u32> {
    let bytes = user_sid().unwrap();
    let mut words = vec![0_u32; bytes.len().div_ceil(4)];
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), words.as_mut_ptr().cast(), bytes.len())
    };
    words
}

/// A restricted impersonation copy of this process's token: every privilege but
/// SeChangeNotify removed and `restricting` as its only restricting SID. Input: the SID,
/// alive for the call. Output: the token.
fn restricted_to(restricting: PSID) -> OwnedHandle {
    let token = own_token(TokenImpersonation);
    let entry = SID_AND_ATTRIBUTES {
        Sid: restricting,
        Attributes: 0,
    };
    let mut made = null_mut();
    let done = unsafe {
        CreateRestrictedToken(
            token.as_raw_handle(),
            DISABLE_MAX_PRIVILEGE,
            0,
            null(),
            0,
            null(),
            1,
            &entry,
            &mut made,
        )
    };
    assert_ne!(done, 0, "{}", io::Error::last_os_error());
    unsafe { OwnedHandle::from_raw_handle(made) }
}

/// Runs `work` on this thread while it impersonates `token`, then reverts; a failed revert
/// aborts the process, so no later code runs as the lowered principal. Inputs: the
/// impersonation token and the work. Output: what the work returned.
fn as_principal<T>(token: &OwnedHandle, work: impl FnOnce() -> T) -> T {
    assert_ne!(
        unsafe { SetThreadToken(null(), token.as_raw_handle()) },
        0,
        "{}",
        io::Error::last_os_error()
    );
    let result = work();
    if unsafe { RevertToSelf() } == 0 {
        std::process::abort();
    }
    result
}

/// The Win32 code of a failed create, or "created". Input: the create result. Output: the
/// text the probe logs.
fn outcome(result: &io::Result<OwnedHandle>) -> String {
    match result {
        Ok(_) => "created".into(),
        Err(error) => format!("refused {:?} {error}", error.raw_os_error()),
    }
}

/// The Win32 code of a failed client open, or "opened". Input: the open result. Output:
/// the text the row logs.
fn opened(result: &io::Result<File>) -> String {
    match result {
        Ok(_) => "opened".into(),
        Err(error) => format!("refused {:?} {error}", error.raw_os_error()),
    }
}

/// What a peer check read, or its Win32 code. Input: the check result. Output: the text
/// the row logs.
fn described(result: &io::Result<Peer>) -> String {
    match result {
        Ok(peer) => format!(
            "read: same user {}, app container {}, restricted {}, integrity {:#x}",
            peer.user == user_sid().unwrap(),
            peer.app_container,
            peer.restricted,
            peer.integrity
        ),
        Err(error) => format!("refused {:?} {error}", error.raw_os_error()),
    }
}

/// A fresh engine pipe with one instance, and a client end opened at identification level
/// as the engine's clients open it, as `principal` when one is given. Input: the
/// impersonation token to open as, if any. Output: the server end, and the client end or
/// the open error.
fn connected(principal: Option<&OwnedHandle>) -> (OwnedHandle, io::Result<File>) {
    let name = unique("engine");
    let server = pipe::create(&name, true, 1).unwrap();
    let open = || {
        OpenOptions::new()
            .read(true)
            .write(true)
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(&name)
    };
    let client = match principal {
        Some(token) => as_principal(token, open),
        None => open(),
    };
    (server, client)
}

/// Waits for an overlapped call on `server` that may still be pending. Inputs: the server
/// end, the call's OVERLAPPED and whether the call itself returned success. Output: the
/// bytes it moved.
fn finished(server: &OwnedHandle, overlapped: &OVERLAPPED, done: bool) -> u32 {
    if !done {
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(ERROR_IO_PENDING as i32)
        );
    }
    let mut moved = 0_u32;
    let waited = unsafe { GetOverlappedResult(server.as_raw_handle(), overlapped, &mut moved, 1) };
    assert_ne!(waited, 0, "{}", io::Error::last_os_error());
    moved
}

/// Accepts the client already waiting on `server` and reads its first bytes, as the D9
/// server reads the first frame before its peer check. The handle is overlapped, so each
/// call waits on its own OVERLAPPED. Input: the server end. Output: the bytes read.
fn first_bytes(server: &OwnedHandle) -> Vec<u8> {
    let mut overlapped = OVERLAPPED::default();
    if unsafe { ConnectNamedPipe(server.as_raw_handle(), &mut overlapped) } == 0 {
        let error = io::Error::last_os_error().raw_os_error();
        if error != Some(ERROR_PIPE_CONNECTED as i32) {
            finished(server, &overlapped, false);
        }
    }
    let mut buffer = [0_u8; 16];
    let mut overlapped = OVERLAPPED::default();
    let read = unsafe {
        ReadFile(
            server.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            null_mut(),
            &mut overlapped,
        )
    };
    let moved = finished(server, &overlapped, read != 0) as usize;
    buffer[..moved].to_vec()
}

/// Impersonates the client of `server` and opens the thread token with OpenAsSelf false,
/// then reverts; a failed revert aborts. NOTE-0145 A2's P1 probe. Input: the server end.
/// Output: "opened", or the step that was refused and its code.
fn not_as_self(server: &OwnedHandle) -> String {
    if unsafe { ImpersonateNamedPipeClient(server.as_raw_handle()) } == 0 {
        return format!("impersonation refused {}", io::Error::last_os_error());
    }
    let mut token: HANDLE = null_mut();
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 0, &mut token) };
    let failure = io::Error::last_os_error();
    if unsafe { RevertToSelf() } == 0 {
        std::process::abort();
    }
    if opened == 0 {
        return format!("refused {:?} {failure}", failure.raw_os_error());
    }
    drop(unsafe { OwnedHandle::from_raw_handle(token) });
    "opened".into()
}

/// Whether this thread holds a token of its own. Output: Ok when it does, or the code of
/// the failed open, ERROR_NO_TOKEN when the thread acts as the process.
fn thread_token() -> Result<(), i32> {
    let mut token: HANDLE = null_mut();
    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } == 0 {
        return Err(io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or_default());
    }
    drop(unsafe { OwnedHandle::from_raw_handle(token) });
    Ok(())
}

/// The lowered principals P2 tries, by name, each as an impersonation token.
fn principals() -> Vec<(&'static str, OwnedHandle)> {
    vec![
        ("low-il", low_token(TokenImpersonation)),
        ("restricted", restricted_token()),
    ]
}

/// W2 D8 (P2 row 3): the first instance claims a pipe name, and a second create that asks
/// to be the first instance is refused with access denied, the code the engine reports as
/// `engine_pipe_taken`.
#[test]
fn a_first_instance_claims_the_name_and_a_second_first_create_is_refused() {
    let name = unique("engine");
    let held = pipe::create(&name, true, 2).unwrap();
    let second = pipe::create(&name, true, 2);
    println!(
        "P2 second first-instance create on a held name: {}",
        outcome(&second)
    );
    assert_eq!(
        second.unwrap_err().raw_os_error(),
        Some(ERROR_ACCESS_DENIED as i32)
    );
    drop(held);
}

/// W2 D8 (P2 row 4): once every instance a name allows exists, another create of that name
/// is refused, so no rogue instance fits beside the engine's.
#[test]
fn a_pipe_with_every_instance_in_use_refuses_another() {
    let name = unique("owner");
    let first = pipe::create(&name, true, 2).unwrap();
    let second = pipe::create(&name, false, 2).unwrap();
    let third = pipe::create(&name, false, 2);
    println!("P2 create beyond the instance cap: {}", outcome(&third));
    assert_eq!(
        third.unwrap_err().raw_os_error(),
        Some(ERROR_PIPE_BUSY as i32)
    );
    for (principal, token) in principals() {
        let rogue = as_principal(&token, || pipe::create(&name, false, 2));
        println!(
            "P2 {principal} create beyond the instance cap: {}",
            outcome(&rogue)
        );
        assert!(rogue.is_err());
    }
    drop((first, second));
}

/// W2 D8 (P2 row 1): a low integrity and a restricted principal of the same user cannot add
/// an instance to a name the engine holds with its owner-only list, while the engine itself
/// still can on the same name (A6 pairing).
#[test]
fn lower_principals_cannot_add_an_instance_to_a_held_name() {
    for purpose in ["engine", "owner", "runner"] {
        let name = unique(purpose);
        let held = pipe::create(&name, true, 4).unwrap();
        for (principal, token) in principals() {
            let rogue = as_principal(&token, || pipe::create(&name, false, 4));
            println!(
                "P2 {principal} adds an instance to a held {purpose} name: {}",
                outcome(&rogue)
            );
            assert!(rogue.is_err(), "{principal} added an instance to {name}");
        }
        let paired = pipe::create(&name, false, 4);
        println!(
            "P2 engine adds an instance to its {purpose} name: {}",
            outcome(&paired)
        );
        assert!(paired.is_ok());
        drop((held, paired));
    }
}

/// W2 D7 (P2 row 1, probe): whether a low integrity or restricted principal of the same user
/// can create a fresh name first is logged with its Win32 code; whatever it gets, the
/// engine creates that name once the principal's pipe is gone (A6 pairing). D7's unguessable
/// names and D10's client checks cover a principal that can.
#[test]
fn lower_principals_creating_a_fresh_name_are_logged() {
    for purpose in ["engine", "owner", "runner"] {
        for (principal, token) in principals() {
            let name = unique(purpose);
            let rogue = as_principal(&token, || pipe::create(&name, true, 1));
            println!(
                "P2 {principal} creates a fresh {purpose} name: {}",
                outcome(&rogue)
            );
            drop(rogue);
            let paired = pipe::create(&name, true, 1);
            println!(
                "P2 engine creates that {purpose} name after: {}",
                outcome(&paired)
            );
            assert!(paired.is_ok());
        }
    }
}

/// Starts `command` as a child from a copy of this process's token lowered to low
/// integrity, through CreateProcessAsUserW. Input: the command line. Output: the child's
/// process handle and ID, or the Windows error.
fn start_low(command: &str) -> io::Result<(OwnedHandle, u32)> {
    let token = low_token(TokenPrimary);
    let mut command: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut started = PROCESS_INFORMATION::default();
    let made = unsafe {
        CreateProcessAsUserW(
            token.as_raw_handle(),
            null(),
            command.as_mut_ptr(),
            null(),
            null(),
            0,
            0,
            null(),
            null(),
            &startup,
            &mut started,
        )
    };
    if made == 0 {
        return Err(io::Error::last_os_error());
    }
    drop(unsafe { OwnedHandle::from_raw_handle(started.hThread) });
    let process = unsafe { OwnedHandle::from_raw_handle(started.hProcess) };
    Ok((process, started.dwProcessId))
}

/// W2 P2 (probe for row 2): a child process starts from a copy of this process's token
/// lowered to low integrity through CreateProcessAsUserW without any extra privilege, which
/// the rogue-server rows rely on, and runs to its exit code.
#[test]
fn a_low_integrity_child_starts_from_a_lowered_primary_token() {
    let root = std::env::var("SystemRoot").unwrap();
    let child = start_low(&format!(r"{root}\System32\cmd.exe /c exit 7"));
    println!(
        "P2 low-il child through CreateProcessAsUserW: {}",
        match &child {
            Ok(_) => "started".to_string(),
            Err(failure) => format!("refused {:?} {failure}", failure.raw_os_error()),
        }
    );
    let (process, _) = child.unwrap();
    assert_eq!(
        unsafe { WaitForSingleObject(process.as_raw_handle(), 30_000) },
        0
    );
    let mut code = 0;
    assert_ne!(
        unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) },
        0
    );
    assert_eq!(code, 7);
}

/// W2 D9 (DESIGN-0140 D9, NOTE-0145 A2): after the first bytes are read, the server's peer
/// check on a client of this user reads the user's SID, no AppContainer, no restriction and
/// medium or above, so the judge passes, and the thread holds no token afterwards. A check
/// before any byte and a thread token opened not as self are logged with their codes.
#[test]
fn the_peer_check_reads_a_client_of_this_user_and_leaves_no_thread_token() {
    let user = user_sid().unwrap();
    let (server, client) = connected(None);
    let mut client = client.unwrap();
    let early = pipe::client(server.as_handle());
    println!(
        "D9 peer check before any byte is read: {}",
        described(&early)
    );
    assert_eq!(thread_token(), Err(ERROR_NO_TOKEN as i32));
    client.write_all(b"x").unwrap();
    assert_eq!(first_bytes(&server), b"x");
    println!(
        "D9 thread token opened not as self: {}",
        not_as_self(&server)
    );
    let peer = pipe::client(server.as_handle());
    println!("D9 peer check after the first byte: {}", described(&peer));
    let peer = peer.unwrap();
    assert_eq!(peer.user, user);
    assert!(!peer.app_container && !peer.restricted);
    assert!(peer.integrity >= 0x2000, "{:#x}", peer.integrity);
    assert_eq!(pipe::judge_peer(&peer, &user), Ok(()));
    assert_eq!(thread_token(), Err(ERROR_NO_TOKEN as i32));
}

/// W2 D9 (DESIGN-0140 row 3): a restricted token whose restricting SID is the user's own
/// passes the pipe's owner-only list, so only the peer check stands between it and the
/// engine; the check reads it as restricted and the judge refuses it.
#[test]
fn a_restricted_client_the_list_lets_in_is_refused_by_the_peer_check() {
    let user = user_sid().unwrap();
    let words = user_words();
    let token = restricted_to(words.as_ptr().cast_mut().cast());
    let (server, client) = connected(Some(&token));
    println!(
        "D9 client restricted to the user opens the engine pipe: {}",
        opened(&client)
    );
    let mut client = client.unwrap();
    client.write_all(b"x").unwrap();
    assert_eq!(first_bytes(&server), b"x");
    let peer = pipe::client(server.as_handle());
    println!(
        "D9 peer check on the restricted client: {}",
        described(&peer)
    );
    let peer = peer.unwrap();
    assert_eq!(peer.user, user);
    assert!(peer.restricted);
    assert_eq!(
        pipe::judge_peer(&peer, &user),
        Err("a restricted token is at the other end of the pipe")
    );
    assert_eq!(thread_token(), Err(ERROR_NO_TOKEN as i32));
}

/// W2 D9 (DESIGN-0140 row 2): a low integrity client and a client restricted to the
/// RESTRICTED SID are refused at open by the pipe's list and label, before any peer check,
/// while the engine's own client opens the same kind of pipe (A6 pairing).
#[test]
fn clients_the_pipe_list_refuses_never_reach_the_peer_check() {
    for (principal, token) in principals() {
        let (server, client) = connected(Some(&token));
        println!(
            "D9 {principal} client opens the engine pipe: {}",
            opened(&client)
        );
        assert!(client.is_err(), "{principal} opened the engine pipe");
        drop(server);
    }
    let (server, client) = connected(None);
    println!(
        "D9 engine client opens the engine pipe: {}",
        opened(&client)
    );
    assert!(client.is_ok());
    drop(server);
}

/// The name a rogue server child squats, made from its own process ID. Input: the child's
/// process ID. Output: the full pipe name.
fn rogue_name(pid: u32) -> String {
    format!(r"\\.\pipe\crazyagent-p2-rogue-{pid}")
}

/// This process, borrowed through its pseudo handle. Output: the handle.
fn this_process() -> BorrowedHandle<'static> {
    unsafe { BorrowedHandle::borrow_raw(GetCurrentProcess()) }
}

/// What a client read off its pipe, or the Win32 code. Input: the reading. Output: the text
/// the row logs.
fn read_off(result: &io::Result<Served>) -> String {
    match result {
        Ok(served) => format!(
            "pid {}, created {}, label {:?}, {}",
            served.pin.pid,
            served.pin.created,
            served.label.map(|level| format!("{level:#x}")),
            described(&Ok(served.peer.clone()))
        ),
        Err(error) => format!("refused {:?} {error}", error.raw_os_error()),
    }
}

/// Opens the client end of `name` as the engine's clients do, retrying while the name does
/// not exist yet, for 30 seconds at most. Input: the pipe name. Output: the client end.
fn open_when_made(name: &str) -> File {
    for _ in 0..300 {
        let opened = OpenOptions::new()
            .read(true)
            .write(true)
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(name);
        match opened {
            Ok(file) => return file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                std::thread::sleep(Duration::from_millis(100))
            }
            Err(error) => panic!("{name}: {error}"),
        }
    }
    panic!("{name} was not made within 30 seconds");
}

/// W2 D10 (DESIGN-0256 steps 1-3, NOTE-0145 A4 pass): before writing a byte, a client reads
/// off its connected pipe the process that serves it, here this engine: the pin matches the
/// engine's own ID and creation time, its token passes, and the pipe's label, if any, is not
/// below medium.
#[test]
fn the_client_pins_the_engine_that_serves_its_pipe() {
    let user = user_sid().unwrap();
    let engine = pipe::pin_of(this_process()).unwrap();
    assert_eq!(engine.pid, std::process::id());
    assert_ne!(engine.created, 0);
    let (server, client) = connected(None);
    let client = client.unwrap();
    let served = pipe::server(client.as_handle());
    println!(
        "D10 client reads the engine's own pipe: {}",
        read_off(&served)
    );
    let served = served.unwrap();
    assert_eq!(served.pin, engine);
    assert_eq!(pipe::judge_server(&served, engine, &user), Ok(()));
    drop((client, server));
}

/// Not a row on its own: the rogue server that
/// `a_client_refuses_a_pipe_a_low_integrity_child_serves` starts as a low integrity child.
/// It squats the name made from its own process ID and holds it for 30 seconds at most;
/// the parent ends it sooner.
#[test]
#[ignore = "the low integrity child of a D10 row"]
fn rogue_server_child() {
    let server = pipe::create(&rogue_name(std::process::id()), true, 1).unwrap();
    std::thread::sleep(Duration::from_secs(30));
    drop(server);
}

/// W2 D10 (DESIGN-0256 probe row 2): a low integrity child of the user squats a fresh name,
/// as P2 showed it can, and serves it. The client that opens it reads the child as the
/// server, so the pin to the engine refuses it before a byte is written; the child's token
/// is below medium as well. The child's pipe label is logged.
#[test]
fn a_client_refuses_a_pipe_a_low_integrity_child_serves() {
    let user = user_sid().unwrap();
    let engine = pipe::pin_of(this_process()).unwrap();
    let test = std::env::current_exe().unwrap();
    let (child, pid) = start_low(&format!(
        "\"{}\" rogue_server_child --exact --ignored --test-threads 1",
        test.display()
    ))
    .unwrap();
    let client = open_when_made(&rogue_name(pid));
    let served = pipe::server(client.as_handle());
    println!(
        "D10 client reads a pipe a low-il child serves: {}",
        read_off(&served)
    );
    let served = served.unwrap();
    assert_eq!(served.pin.pid, pid);
    assert_eq!(served.pin, pipe::pin_of(child.as_handle()).unwrap());
    assert_eq!(
        pipe::judge_server(&served, engine, &user),
        Err("another process serves the pipe")
    );
    assert_eq!(
        pipe::judge_peer(&served.peer, &user),
        Err("a lower integrity process is at the other end of the pipe")
    );
    drop(client);
    unsafe { TerminateProcess(child.as_raw_handle(), 1) };
    assert_eq!(
        unsafe { WaitForSingleObject(child.as_raw_handle(), 30_000) },
        0
    );
}
