use std::ffi::c_void;
use std::ptr::{null, null_mut};

#[repr(C)]
struct ShellExecuteInfo {
    size: u32,
    mask: u32,
    window: *mut c_void,
    verb: *const u16,
    file: *const u16,
    parameters: *const u16,
    directory: *const u16,
    show: i32,
    instance: *mut c_void,
    id_list: *mut c_void,
    class: *const u16,
    class_key: *mut c_void,
    hot_key: u32,
    icon: *mut c_void,
    process: *mut c_void,
}

#[link(name = "shell32")]
extern "system" {
    fn ShellExecuteExW(info: *mut ShellExecuteInfo) -> i32;
}

#[link(name = "ole32")]
extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, model: u32) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetLastError() -> u32;
    fn WaitForSingleObject(handle: *mut c_void, millis: u32) -> u32;
    fn GetExitCodeProcess(handle: *mut c_void, code: *mut u32) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

const NO_CLOSE_PROCESS: u32 = 0x40;
const NO_ASYNC: u32 = 0x100;
const NO_UI: u32 = 0x400;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let mask = match mode.as_str() {
        "ui" => NO_CLOSE_PROCESS | NO_ASYNC,
        "noui" => NO_CLOSE_PROCESS | NO_ASYNC | NO_UI,
        _ => {
            println!("usage: runasprobe ui|noui");
            std::process::exit(64);
        }
    };
    let com = unsafe { CoInitializeEx(null_mut(), 0x2 | 0x4) };
    let verb = wide("runas");
    let file = wide(r"C:\Windows\System32\cmd.exe");
    let parameters = wide("/c exit 7");
    let mut info = ShellExecuteInfo {
        size: std::mem::size_of::<ShellExecuteInfo>() as u32,
        mask,
        window: null_mut(),
        verb: verb.as_ptr(),
        file: file.as_ptr(),
        parameters: parameters.as_ptr(),
        directory: null(),
        show: 0,
        instance: null_mut(),
        id_list: null_mut(),
        class: null(),
        class_key: null_mut(),
        hot_key: 0,
        icon: null_mut(),
        process: null_mut(),
    };
    let started = unsafe { ShellExecuteExW(&mut info) };
    let error = if started == 0 { unsafe { GetLastError() } } else { 0 };
    println!(
        "P4 mode={mode} size={} com=0x{com:08x} started={started} error={error} instance={} process={}",
        info.size,
        info.instance as usize,
        !info.process.is_null()
    );
    if !info.process.is_null() {
        let waited = unsafe { WaitForSingleObject(info.process, 60_000) };
        let mut code = 0u32;
        let read = unsafe { GetExitCodeProcess(info.process, &mut code) };
        println!("P4 mode={mode} waited={waited} read={read} child_exit={code}");
        unsafe { CloseHandle(info.process) };
    }
}
