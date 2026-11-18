use std::ffi::c_void;
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
    fn DeviceIoControl(
        device: *mut c_void,
        code: u32,
        input: *const c_void,
        input_size: u32,
        output: *mut c_void,
        output_size: u32,
        returned: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn GetLastError() -> u32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

const MOUNT_MANAGER: &str = r"\\.\MountPointManager";
const SHARE_ALL: u32 = 7;
const OPEN_EXISTING: u32 = 3;
const BACKUP_SEMANTICS: u32 = 0x0200_0000;
const VOLUME_NAME_NT: u32 = 2;
const NAME_UNITS: usize = 32768;
const OUTPUT_BYTES: usize = 256 * 1024;
const POINT_BYTES: usize = 24;
const POINTS_HEADER: usize = 8;
const ANY_ACCESS: u32 = 0;
const READ_WRITE_ACCESS: u32 = 3;
const GENERIC_READ_WRITE: u32 = 0xC000_0000;
const WRITE_BITS: u32 = 0x4000_0002;
const OPENS: [(u32, &str); 5] = [
    (0x0012_0089, "file-generic-read"),
    (0x0010_0080, "attributes-synchronize"),
    (0, "no-rights"),
    (0x0000_0002, "write-data"),
    (GENERIC_READ_WRITE, "generic-read-write"),
];

/// The device control code the mount manager's header builds with CTL_CODE: device type
/// 'm', buffered, with the access the handle must hold for the I/O manager to pass it on.
/// Inputs: the function number and the access bits. Output: the code.
const fn control(function: u32, access: u32) -> u32 {
    (0x6D << 16) | (access << 14) | (function << 2)
}

const CREATE_POINT: u32 = control(0, READ_WRITE_ACCESS);
const QUERY_POINTS: u32 = control(2, ANY_ACCESS);
const QUERY_DOS_VOLUME_PATHS: u32 = control(13, ANY_ACCESS);
const QUERY_AUTO_MOUNT: u32 = control(15, ANY_ACCESS);
const _: () = assert!(CREATE_POINT == 0x006D_C000 && QUERY_POINTS == 0x006D_0008);
const _: () = assert!(QUERY_DOS_VOLUME_PATHS == 0x006D_0034 && QUERY_AUTO_MOUNT == 0x006D_003C);

/// The UTF-16 units of `text` with one ending zero, the form the wide Windows calls take
/// for a name.
/// Input: the text. Output: the units.
fn wide(text: &str) -> Vec<u16> {
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    assert_eq!(units.last(), Some(&0));
    units
}

/// Opens `path` with `access`, every share mode and backup semantics, so a folder or a
/// device opens the same way. Inputs: the path and the access mask.
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

/// Sends one device control call with `input` and a large output buffer, and reads the
/// last error right after it. Inputs: the open device, the code and the input bytes.
/// Output: the bytes written back, or the last error.
fn call(device: *mut c_void, code: u32, input: &[u8]) -> Result<Vec<u8>, u32> {
    let mut output = vec![0u8; OUTPUT_BYTES];
    let mut returned = 0u32;
    let done = unsafe {
        DeviceIoControl(
            device,
            code,
            input.as_ptr().cast(),
            input.len() as u32,
            output.as_mut_ptr().cast(),
            output.len() as u32,
            &mut returned,
            null_mut(),
        )
    };
    let error = unsafe { GetLastError() };
    if done == 0 {
        return Err(error);
    }
    assert!(returned as usize <= output.len());
    output.truncate(returned as usize);
    Ok(output)
}

/// Reads a little-endian number of `width` bytes at `at`, or none past the end.
/// Inputs: the bytes, the offset and the width, 2 or 4.
/// Output: the number.
fn number(bytes: &[u8], at: usize, width: usize) -> Option<usize> {
    assert!(width == 2 || width == 4);
    let part = bytes.get(at..at.checked_add(width)?)?;
    Some(
        part.iter()
            .rev()
            .fold(0usize, |sum, byte| (sum << 8) | *byte as usize),
    )
}

/// Reads `length` bytes of UTF-16 text at `at`, or a marker when they fall outside.
/// Inputs: the bytes, the offset and the length in bytes.
/// Output: the text.
fn text_at(bytes: &[u8], at: usize, length: usize) -> String {
    let Some(part) = at.checked_add(length).and_then(|end| bytes.get(at..end)) else {
        return String::from("<outside>");
    };
    let units: Vec<u16> = part
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// The NT name of the volume the current folder sits on, such as
/// `\Device\HarddiskVolume3`, read from the folder's final name with the NT volume form.
/// Output: the name, or why it could not be read.
fn volume_device() -> Result<String, String> {
    let mut units = vec![0u16; NAME_UNITS];
    let wrote = unsafe { GetCurrentDirectoryW(units.len() as u32, units.as_mut_ptr()) } as usize;
    if wrote == 0 || wrote >= units.len() {
        return Err(format!("cwd err {}", unsafe { GetLastError() }));
    }
    let folder = String::from_utf16_lossy(&units[..wrote]);
    let handle = open(&folder, 0).map_err(|code| format!("open cwd err {code}"))?;
    let wrote = unsafe {
        GetFinalPathNameByHandleW(
            handle,
            units.as_mut_ptr(),
            units.len() as u32,
            VOLUME_NAME_NT,
        )
    } as usize;
    let error = unsafe { GetLastError() };
    unsafe { CloseHandle(handle) };
    if wrote == 0 || wrote >= units.len() {
        return Err(format!("final name err {error}"));
    }
    let name = String::from_utf16_lossy(&units[..wrote]);
    let parts: Vec<&str> = name.splitn(4, '\\').collect();
    if parts.len() < 3 || !parts[0].is_empty() {
        return Err(format!("unexpected name {name}"));
    }
    Ok(format!(r"\{}\{}", parts[1], parts[2]))
}

/// Prints every mount point the mount manager reports to this caller: its link name, the
/// volume device it names and the length of its unique ID. Input: the open mount manager.
/// Output: `MOUNT` lines on stdout.
fn query_points(device: *mut c_void) {
    let answer = match call(device, QUERY_POINTS, &[0u8; POINT_BYTES]) {
        Ok(answer) => answer,
        Err(code) => return println!("MOUNT query-points err {code}"),
    };
    let count = number(&answer, 4, 4).unwrap_or(0);
    let fits = answer.len().saturating_sub(POINTS_HEADER) / POINT_BYTES;
    println!("MOUNT query-points ok count={count} bytes={}", answer.len());
    for index in 0..count.min(fits) {
        let at = POINTS_HEADER + index * POINT_BYTES;
        let field = |offset: usize, width: usize| number(&answer, at + offset, width).unwrap_or(0);
        let link = text_at(&answer, field(0, 4), field(4, 2));
        let volume = text_at(&answer, field(16, 4), field(20, 2));
        println!(
            "MOUNT point link={link} device={volume} id-bytes={}",
            field(12, 2)
        );
    }
}

/// Prints the drive letters and folder mount points of `volume`, and whether new volumes
/// get a letter on their own. Inputs: the open mount manager and the volume's NT name.
/// Output: `MOUNT` lines on stdout.
fn query_volume(device: *mut c_void, volume: &str) {
    let units: Vec<u16> = volume.encode_utf16().collect();
    let mut input = ((units.len() * 2) as u16).to_le_bytes().to_vec();
    input.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
    match call(device, QUERY_DOS_VOLUME_PATHS, &input) {
        Err(code) => println!("MOUNT dos-paths {volume} err {code}"),
        Ok(answer) => {
            let length = number(&answer, 0, 4).unwrap_or(0);
            let paths = text_at(&answer, 4, length.min(answer.len().saturating_sub(4)));
            let shown: Vec<&str> = paths.split('\0').filter(|path| !path.is_empty()).collect();
            println!("MOUNT dos-paths {volume} ok [{}]", shown.join(", "));
        }
    }
    match call(device, QUERY_AUTO_MOUNT, &[]) {
        Err(code) => println!("MOUNT auto-mount err {code}"),
        Ok(answer) => println!("MOUNT auto-mount ok state={:?}", number(&answer, 0, 4)),
    }
}

/// Asks the mount manager to make `letter:` a new drive letter for `volume`, a mount change
/// a container must be refused. Inputs: the open mount manager, the letter and the
/// volume's NT name. Output: one `MOUNT` line on stdout.
fn create_point(device: *mut c_void, letter: &str, volume: &str) {
    let link: Vec<u16> = format!(r"\DosDevices\{letter}:").encode_utf16().collect();
    let target: Vec<u16> = volume.encode_utf16().collect();
    let link_bytes = link.len() * 2;
    let target_bytes = target.len() * 2;
    assert!(8 + link_bytes + target_bytes <= u16::MAX as usize);
    let mut input = Vec::new();
    for value in [8, link_bytes, 8 + link_bytes, target_bytes] {
        input.extend((value as u16).to_le_bytes());
    }
    input.extend(
        link.iter()
            .chain(target.iter())
            .flat_map(|unit| unit.to_le_bytes()),
    );
    match call(device, CREATE_POINT, &input) {
        Ok(_) => println!("MOUNT create-point {letter}: -> {volume} ok"),
        Err(code) => println!("MOUNT create-point {letter}: -> {volume} err {code}"),
    }
}

/// Opens the mount manager with each access in `OPENS` and prints the result, then queries
/// the first read handle that opened (`query`), or tries one mount change (`create <letter>`)
/// on the read-write handle when one opened and on that read handle otherwise. Run in the
/// folder whose volume it names. Output: `MOUNT` lines on stdout; the exit code is always 0.
fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let changing = arguments.first().map(String::as_str) == Some("create");
    let mut reader = None;
    let mut writer = None;
    for (access, label) in OPENS {
        match open(MOUNT_MANAGER, access) {
            Err(code) => println!("MOUNT open {label} err {code}"),
            Ok(handle) => {
                println!("MOUNT open {label} ok");
                let writes = access & WRITE_BITS != 0;
                if !writes && reader.is_none() {
                    reader = Some((handle, label));
                } else if writes && writer.is_none() && access == GENERIC_READ_WRITE {
                    writer = Some((handle, label));
                } else {
                    unsafe { CloseHandle(handle) };
                }
            }
        }
    }
    let (chosen, spare) = if changing && writer.is_some() {
        (writer, reader)
    } else {
        (reader, writer)
    };
    if let Some((handle, _)) = spare {
        unsafe { CloseHandle(handle) };
    }
    let volume = match volume_device() {
        Ok(volume) => volume,
        Err(why) => return println!("MOUNT volume {why}"),
    };
    let Some((handle, label)) = chosen else {
        return println!("MOUNT no usable handle for {volume}");
    };
    println!("MOUNT using {label} for {volume}");
    match arguments.first().map(String::as_str) {
        Some("query") => {
            query_points(handle);
            query_volume(handle, &volume);
        }
        Some("create") => create_point(
            handle,
            arguments.get(1).map_or("Q", String::as_str),
            &volume,
        ),
        _ => println!("MOUNT usage: query | create <letter>"),
    }
    unsafe { CloseHandle(handle) };
}
