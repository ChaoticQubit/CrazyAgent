//! Every call into macOS the helper makes that Rust cannot make safely: the peer's audit token,
//! signing information, launchd's socket, the thermal pressure key, the lid, the monotonic
//! clock, signals, `poll` and syslog. The one file of this crate allowed `unsafe`.

use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use security_framework::os::macos::code_signing::SecCode;
use std::ffi::{CString, c_char, c_int, c_void};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};

const NOTIFY_STATUS_OK: u32 = 0;
const SIGNING_FLAGS: u32 = 2 | 4 | 8;
const MAIN_PORT_DEFAULT: u32 = 0;
const LAUNCH_FDS_MAX: usize = 8;
const LOG_CHARS_MAX: usize = 512;

static TERM: AtomicBool = AtomicBool::new(false);

#[allow(non_snake_case, non_upper_case_globals)]
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecCodeCopySigningInformation(
        code: *const c_void,
        flags: u32,
        information: *mut CFDictionaryRef,
    ) -> i32;
    static kSecCodeInfoFlags: CFStringRef;
    static kSecCodeInfoEntitlementsDict: CFStringRef;
}

#[allow(non_snake_case)]
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> CFDictionaryRef;
    fn IOServiceGetMatchingService(main_port: u32, matching: CFDictionaryRef) -> u32;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: CFStringRef,
        allocator: *const c_void,
        options: u32,
    ) -> CFTypeRef;
    fn IOObjectRelease(object: u32) -> i32;
}

unsafe extern "C" {
    fn notify_register_check(name: *const c_char, token: *mut c_int) -> u32;
    fn notify_get_state(token: c_int, state: *mut u64) -> u32;
    fn notify_cancel(token: c_int) -> u32;
    fn launch_activate_socket(
        name: *const c_char,
        fds: *mut *mut c_int,
        count: *mut usize,
    ) -> c_int;
}

/// The audit token of the peer of a connected local socket (`LOCAL_PEERTOKEN`), as of its
/// `connect`. Input: the socket. Output: the token's eight words, or the system error.
pub fn peer_token(socket: BorrowedFd<'_>) -> io::Result<[u32; 8]> {
    let mut token = [0_u32; 8];
    let size = size_of_val(&token);
    let mut length = libc::socklen_t::try_from(size).expect("32 fits a socklen_t");
    let status = unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERTOKEN,
            token.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    assert_eq!(length as usize, size);
    Ok(token)
}

/// A caller's signing flags (`kSecCodeInfoFlags`) and entitlement names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signing {
    pub flags: u32,
    pub entitlements: Vec<String>,
}

/// Reads `code`'s signing information: its flags and its entitlement names, none when it has
/// no entitlements. Inputs: the code and the most names accepted. Output: `None` when the
/// information cannot be read or holds more names than that.
pub fn signing_information(code: &SecCode, names_max: usize) -> Option<Signing> {
    let mut information: CFDictionaryRef = std::ptr::null();
    let code_ref = code.as_concrete_TypeRef().cast_const().cast::<c_void>();
    let status =
        unsafe { SecCodeCopySigningInformation(code_ref, SIGNING_FLAGS, &mut information) };
    if status != 0 || information.is_null() {
        return None;
    }
    let information: CFDictionary<CFString, CFType> =
        unsafe { CFDictionary::wrap_under_create_rule(information) };
    let flags_key = unsafe { CFString::wrap_under_get_rule(kSecCodeInfoFlags) };
    let flags = information
        .find(&flags_key)?
        .downcast::<CFNumber>()?
        .to_i64()?;
    let flags = u32::try_from(flags).ok()?;
    let names_key = unsafe { CFString::wrap_under_get_rule(kSecCodeInfoEntitlementsDict) };
    let entitlements = match information.find(&names_key) {
        Some(value) => entitlement_names(&value, names_max)?,
        None => Vec::new(),
    };
    assert!(entitlements.len() <= names_max);
    Some(Signing {
        flags,
        entitlements,
    })
}

/// The keys of an entitlements dictionary. Output: `None` when it is not a dictionary of
/// string keys or holds more than `names_max` of them.
fn entitlement_names(value: &CFType, names_max: usize) -> Option<Vec<String>> {
    let dictionary = value.downcast::<CFDictionary>()?;
    let (keys, _) = dictionary.get_keys_and_values();
    if keys.len() > names_max {
        return None;
    }
    let names = keys.into_iter().map(|key| {
        let key = unsafe { CFType::wrap_under_get_rule(key) };
        key.downcast::<CFString>().map(|name| name.to_string())
    });
    names.collect()
}

/// The system thermal pressure level from the `com.apple.system.thermalpressurelevel` notify
/// key. Output: the level, or `None` when it cannot be read.
pub fn thermal_pressure() -> Option<u64> {
    let mut token: c_int = 0;
    let name = c"com.apple.system.thermalpressurelevel";
    let registered = unsafe { notify_register_check(name.as_ptr(), &mut token) };
    if registered != NOTIFY_STATUS_OK {
        return None;
    }
    let mut state: u64 = 0;
    let read = unsafe { notify_get_state(token, &mut state) };
    let cancelled = unsafe { notify_cancel(token) };
    if cancelled != NOTIFY_STATUS_OK {
        log("notify_cancel failed");
    }
    (read == NOTIFY_STATUS_OK).then_some(state)
}

/// The lid from `IOPMrootDomain`: `AppleClamshellState` and `AppleClamshellCausesSleep`.
/// Output: both, or `None` when either cannot be read.
pub fn clamshell() -> Option<(bool, bool)> {
    let matching = unsafe { IOServiceMatching(c"IOPMrootDomain".as_ptr()) };
    if matching.is_null() {
        return None;
    }
    let service = unsafe { IOServiceGetMatchingService(MAIN_PORT_DEFAULT, matching) };
    if service == 0 {
        return None;
    }
    let closed = registry_bool(service, "AppleClamshellState");
    let causes_sleep = registry_bool(service, "AppleClamshellCausesSleep");
    let released = unsafe { IOObjectRelease(service) };
    if released != 0 {
        log("IOObjectRelease failed");
    }
    Some((closed?, causes_sleep?))
}

/// One boolean property of a registry entry. Output: the value, or `None`.
fn registry_bool(service: u32, name: &'static str) -> Option<bool> {
    assert_ne!(service, 0);
    let key = CFString::from_static_string(name);
    let null = std::ptr::null();
    let value =
        unsafe { IORegistryEntryCreateCFProperty(service, key.as_concrete_TypeRef(), null, 0) };
    if value.is_null() {
        return None;
    }
    let value = unsafe { CFType::wrap_under_create_rule(value) };
    value.downcast::<CFBoolean>().map(bool::from)
}

/// The one socket launchd made for `name` in the helper's plist. Output: the socket, or why
/// there was not exactly one.
pub fn launch_socket(name: &str) -> io::Result<OwnedFd> {
    let name = CString::new(name).map_err(io::Error::other)?;
    let mut fds: *mut c_int = std::ptr::null_mut();
    let mut count: usize = 0;
    let status = unsafe { launch_activate_socket(name.as_ptr(), &mut fds, &mut count) };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status));
    }
    assert!(!fds.is_null() || count == 0);
    let owned: Vec<OwnedFd> = (0..count)
        .map(|index| unsafe { OwnedFd::from_raw_fd(*fds.add(index)) })
        .collect();
    unsafe { libc::free(fds.cast()) };
    if owned.len() != 1 || count > LAUNCH_FDS_MAX {
        return Err(io::Error::other(format!("launchd gave {count} sockets")));
    }
    owned
        .into_iter()
        .next()
        .ok_or_else(|| io::Error::other("no socket"))
}

/// Milliseconds on `CLOCK_MONOTONIC`, which keeps counting while the Mac sleeps.
pub fn monotonic_ms() -> u64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let status = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    assert_eq!(status, 0);
    let secs = u64::try_from(now.tv_sec).expect("the monotonic clock is not negative");
    let nanos = u64::try_from(now.tv_nsec).expect("nanoseconds are not negative");
    assert!(nanos < 1_000_000_000);
    secs * 1_000 + nanos / 1_000_000
}

/// The effective uid of this process.
pub fn euid() -> u32 {
    unsafe { libc::geteuid() }
}

/// Marks that SIGTERM arrived. Async-signal-safe: one atomic store.
extern "C" fn on_term(_signal: c_int) {
    TERM.store(true, Ordering::SeqCst);
}

/// Catches SIGTERM without `SA_RESTART`, so a waiting `poll` returns and the loop sees it.
pub fn catch_term() {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = on_term as extern "C" fn(c_int) as libc::sighandler_t;
    action.sa_flags = 0;
    let emptied = unsafe { libc::sigemptyset(&mut action.sa_mask) };
    assert_eq!(emptied, 0);
    let installed = unsafe { libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()) };
    assert_eq!(installed, 0);
}

/// Whether SIGTERM arrived since `catch_term`.
pub fn term_requested() -> bool {
    TERM.load(Ordering::SeqCst)
}

/// Waits up to `timeout_ms` for `socket` to be readable. Output: whether it is; an
/// interrupted wait is `false`.
pub fn wait_readable(socket: BorrowedFd<'_>, timeout_ms: u64) -> io::Result<bool> {
    let timeout = c_int::try_from(timeout_ms).unwrap_or(c_int::MAX);
    let mut wanted = libc::pollfd {
        fd: socket.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut wanted, 1, timeout) };
    if ready < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(error);
    }
    assert!(ready <= 1);
    Ok(ready == 1)
}

/// Writes one notice to the system log, NUL bytes dropped and cut at `LOG_CHARS_MAX`.
pub fn log(message: &str) {
    let text: String = message
        .chars()
        .filter(|character| *character != '\0')
        .take(LOG_CHARS_MAX)
        .collect();
    let text = CString::new(text).expect("NUL bytes were dropped");
    unsafe { libc::syslog(libc::LOG_NOTICE, c"%s".as_ptr(), text.as_ptr()) };
}
