#![cfg(windows)]

use crazyagent_win_sandbox::pipe;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU32, Ordering};
use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
use windows_sys::Win32::Security::{
    CreateRestrictedToken, DISABLE_MAX_PRIVILEGE, DuplicateTokenEx, GetLengthSid, PSID,
    RevertToSelf, SID_AND_ATTRIBUTES, SecurityImpersonation, SetTokenInformation, TOKEN_ALL_ACCESS,
    TOKEN_DUPLICATE, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_TYPE, TokenImpersonation,
    TokenIntegrityLevel, TokenPrimary,
};
use windows_sys::Win32::System::SystemServices::SE_GROUP_INTEGRITY;
use windows_sys::Win32::System::Threading::{
    CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess, OpenProcessToken,
    PROCESS_INFORMATION, STARTUPINFOW, SetThreadToken, WaitForSingleObject,
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

/// A restricted impersonation copy of this process's token: every privilege but
/// SeChangeNotify removed and RESTRICTED as its restricting SID. Output: the token.
fn restricted_token() -> OwnedHandle {
    let token = own_token(TokenImpersonation);
    let restricting = sid(RESTRICTED);
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
    let failure = io::Error::last_os_error();
    unsafe { LocalFree(restricting) };
    assert_ne!(done, 0, "{failure}");
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

/// W2 P2 (probe for row 2): a child process starts from a copy of this process's token
/// lowered to low integrity through CreateProcessAsUserW without any extra privilege, which
/// the rogue-server rows rely on, and runs to its exit code.
#[test]
fn a_low_integrity_child_starts_from_a_lowered_primary_token() {
    let token = low_token(TokenPrimary);
    let root = std::env::var("SystemRoot").unwrap();
    let mut command: Vec<u16> = format!(r"{root}\System32\cmd.exe /c exit 7")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
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
    let failure = io::Error::last_os_error();
    println!(
        "P2 low-il child through CreateProcessAsUserW: {}",
        if made != 0 {
            "started".to_string()
        } else {
            format!("refused {:?} {failure}", failure.raw_os_error())
        }
    );
    assert_ne!(made, 0);
    let process = unsafe { OwnedHandle::from_raw_handle(started.hProcess) };
    drop(unsafe { OwnedHandle::from_raw_handle(started.hThread) });
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
