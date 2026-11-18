use std::ffi::c_void;
use std::path::Path;
use std::ptr::null_mut;

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentDirectoryW(length: u32, buffer: *mut u16) -> u32;
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *mut c_void,
        disposition: u32,
        flags: u32,
        template: *mut c_void,
    ) -> *mut c_void;
    fn GetFinalPathNameByHandleW(
        file: *mut c_void,
        buffer: *mut u16,
        length: u32,
        flags: u32,
    ) -> u32;
    fn GetLongPathNameW(short: *const u16, buffer: *mut u16, length: u32) -> u32;
    fn GetLastError() -> u32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

const UNITS: usize = 32768;
const SHARE_ALL: u32 = 7;
const OPEN_EXISTING: u32 = 3;
const BACKUP_SEMANTICS: u32 = 0x0200_0000;
const READ_ATTRIBUTES: u32 = 0x80;
const MOUNT_MANAGER: &str = r"\\.\MountPointManager";
const NAME_FLAGS: [(u32, &str); 6] = [
    (0, "final-normalized-dos"),
    (1, "final-normalized-guid"),
    (2, "final-normalized-nt"),
    (4, "final-normalized-none"),
    (8, "final-opened-dos"),
    (10, "final-opened-nt"),
];

/// The UTF-16 units of `text` with one ending zero, the form the wide Windows calls take
/// for a name.
/// Input: the text. Output: the units.
fn wide(text: &str) -> Vec<u16> {
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    assert_eq!(units.last(), Some(&0));
    units
}

/// Runs one Windows call that fills a name buffer and reads its last error right after it,
/// before anything else can change it. Input: the call, given the buffer and its length.
/// Output: `ok <name>`, or `err <code>` when it wrote nothing or more than fits.
fn named(call: impl FnOnce(*mut u16, u32) -> u32) -> String {
    let mut buffer = vec![0u16; UNITS];
    let wrote = call(buffer.as_mut_ptr(), buffer.len() as u32) as usize;
    let error = unsafe { GetLastError() };
    if wrote == 0 || wrote >= buffer.len() {
        return format!("err {error}");
    }
    format!("ok {}", String::from_utf16_lossy(&buffer[..wrote]))
}

/// Opens `path` with `access` the way git's getcwd opens its folder: every share mode and
/// backup semantics, so a folder opens too. Inputs: the path and the access mask.
/// Output: the handle, or the last error.
fn open(path: &str, access: u32) -> Result<*mut c_void, u32> {
    let name = wide(path);
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            access,
            SHARE_ALL,
            null_mut(),
            OPEN_EXISTING,
            BACKUP_SEMANTICS,
            null_mut(),
        )
    };
    if handle as isize == -1 {
        return Err(unsafe { GetLastError() });
    }
    Ok(handle)
}

/// Prints one line per call git's getcwd makes in the current folder: the folder, its
/// zero-access open, its final name under each volume and name flag, the long name of it and
/// of every folder above it, and the mount manager opened with no access and with attributes.
/// Output: `CALL` lines on stdout; the exit code is always 0.
fn main() {
    let cwd = named(|buffer, length| unsafe { GetCurrentDirectoryW(length, buffer) });
    println!("CALL cwd {cwd}");
    let Some(folder) = cwd.strip_prefix("ok ").map(str::to_owned) else {
        return;
    };
    match open(&folder, 0) {
        Err(code) => println!("CALL open-zero err {code}"),
        Ok(handle) => {
            println!("CALL open-zero ok");
            for (flags, label) in NAME_FLAGS {
                let line = named(|buffer, length| unsafe {
                    GetFinalPathNameByHandleW(handle, buffer, length, flags)
                });
                println!("CALL {label} {line}");
            }
            unsafe { CloseHandle(handle) };
        }
    }
    for part in Path::new(&folder).ancestors() {
        let name = wide(&part.to_string_lossy());
        let line =
            named(|buffer, length| unsafe { GetLongPathNameW(name.as_ptr(), buffer, length) });
        println!("CALL long {} {line}", part.display());
    }
    for (access, label) in [
        (0, "mount-manager-zero"),
        (READ_ATTRIBUTES, "mount-manager-attributes"),
    ] {
        match open(MOUNT_MANAGER, access) {
            Err(code) => println!("CALL {label} err {code}"),
            Ok(handle) => {
                println!("CALL {label} ok");
                unsafe { CloseHandle(handle) };
            }
        }
    }
}
