use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
use windows_sys::Win32::Security::{
    CreateRestrictedToken, DISABLE_MAX_PRIVILEGE, DuplicateTokenEx, GetLengthSid, PSID,
    RevertToSelf, SID_AND_ATTRIBUTES, SecurityImpersonation, SetTokenInformation, TOKEN_ALL_ACCESS,
    TOKEN_DUPLICATE, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenImpersonation, TokenIntegrityLevel,
};
use windows_sys::Win32::System::SystemServices::SE_GROUP_INTEGRITY;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken, SetThreadToken};

/// The RESTRICTED SID, the usual restricting SID of a restricted token.
const RESTRICTED: &str = "S-1-5-12";
/// The medium mandatory level SID, an unelevated process's own.
const MEDIUM: &str = "S-1-16-8192";

/// Converts a SID string; the caller frees the SID with `LocalFree`. Input: the SID
/// string. Output: the SID.
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

/// This process's token, opened to be duplicated. Output: the token.
fn process_token() -> OwnedHandle {
    let mut process = null_mut();
    assert_ne!(
        unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_QUERY,
                &mut process,
            )
        },
        0,
        "{}",
        io::Error::last_os_error()
    );
    unsafe { OwnedHandle::from_raw_handle(process) }
}

/// An impersonation copy of `token`. Input: a token opened to be duplicated. Output: the
/// copy.
fn impersonation(token: &OwnedHandle) -> OwnedHandle {
    let mut copy = null_mut();
    assert_ne!(
        unsafe {
            DuplicateTokenEx(
                token.as_raw_handle(),
                TOKEN_ALL_ACCESS,
                null(),
                SecurityImpersonation,
                TokenImpersonation,
                &mut copy,
            )
        },
        0,
        "{}",
        io::Error::last_os_error()
    );
    unsafe { OwnedHandle::from_raw_handle(copy) }
}

/// A restricted impersonation copy of this process's token with RESTRICTED as its only
/// restricting SID, which no engine pipe list grants, so Windows refuses its open of an
/// engine pipe before the engine sees it (R4). Output: the token.
pub fn restricted() -> OwnedHandle {
    restricted_by(false)
}

/// A restricted impersonation copy of this process's token whose restricting SIDs are
/// RESTRICTED and the user's own, so the pipe list that grants the user passes both of its
/// checks and the open reaches the engine, whose peer check then refuses the restricted
/// token (R4). Output: the token.
pub fn restricted_with_user() -> OwnedHandle {
    restricted_by(true)
}

/// Makes a restricted impersonation copy of this process's token. Input: whether the user's
/// own SID is a restricting SID beside RESTRICTED. Output: the token.
fn restricted_by(with_user: bool) -> OwnedHandle {
    let restricting = sid(RESTRICTED);
    let user = crate::private::user_sid().expect("the process token names its user");
    let mut user_words = vec![0u32; user.len().div_ceil(size_of::<u32>())];
    unsafe {
        std::ptr::copy_nonoverlapping(user.as_ptr(), user_words.as_mut_ptr().cast(), user.len())
    };
    let mut entries = vec![SID_AND_ATTRIBUTES {
        Sid: restricting,
        Attributes: 0,
    }];
    if with_user {
        entries.push(SID_AND_ATTRIBUTES {
            Sid: user_words.as_mut_ptr().cast(),
            Attributes: 0,
        });
    }
    let process = process_token();
    let mut made = null_mut();
    let done = unsafe {
        CreateRestrictedToken(
            process.as_raw_handle(),
            DISABLE_MAX_PRIVILEGE,
            0,
            null(),
            0,
            null(),
            u32::try_from(entries.len()).expect("two entries at most"),
            entries.as_ptr(),
            &mut made,
        )
    };
    let failure = io::Error::last_os_error();
    unsafe { LocalFree(restricting) };
    assert_ne!(done, 0, "{failure}");
    impersonation(&unsafe { OwnedHandle::from_raw_handle(made) })
}

/// An impersonation copy of this process's token lowered to medium integrity: the user's
/// unelevated duplicate when the caller runs elevated (R6). Output: the token.
pub fn medium() -> OwnedHandle {
    let token = impersonation(&process_token());
    let level = sid(MEDIUM);
    let label = TOKEN_MANDATORY_LABEL {
        Label: SID_AND_ATTRIBUTES {
            Sid: level,
            Attributes: SE_GROUP_INTEGRITY as u32,
        },
    };
    let bytes = size_of::<TOKEN_MANDATORY_LABEL>() as u32 + unsafe { GetLengthSid(level) };
    let set = unsafe {
        SetTokenInformation(
            token.as_raw_handle(),
            TokenIntegrityLevel,
            std::ptr::from_ref(&label).cast(),
            bytes,
        )
    };
    let failure = io::Error::last_os_error();
    unsafe { LocalFree(level) };
    assert_ne!(set, 0, "{failure}");
    token
}

/// Runs `work` on this thread while it impersonates `token`, then reverts; a failed revert
/// aborts the process, so no later code runs as the lowered principal. Inputs: the token
/// and the work. Output: what the work returned.
pub fn as_principal<T>(token: &OwnedHandle, work: impl FnOnce() -> T) -> T {
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
