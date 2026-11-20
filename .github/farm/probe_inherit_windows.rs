#![cfg(windows)]

use crazyagent_win_sandbox::private;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl, InitializeSecurityDescriptor,
    PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SetSecurityDescriptorControl,
    SetSecurityDescriptorOwner,
};
use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;

const PACKAGES: [u8; 16] = [1, 2, 0, 0, 0, 0, 0, 15, 2, 0, 0, 0, 1, 0, 0, 0];

fn opened(path: &Path) -> std::fs::File {
    std::fs::OpenOptions::new()
        .access_mode(0x0002_0000 | 0x80)
        .custom_flags(0x0020_0000 | 0x0200_0000)
        .open(path)
        .unwrap()
}

fn control(entry: &std::fs::File) -> u16 {
    let mut descriptor = std::ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            entry.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    assert_eq!(status, 0);
    let mut bits = 0u16;
    let mut revision = 0u32;
    assert_ne!(
        unsafe { GetSecurityDescriptorControl(descriptor, &mut bits, &mut revision) },
        0
    );
    unsafe { LocalFree(descriptor) };
    bits
}

fn flags(list: &[u8]) -> Vec<(u8, bool)> {
    let count = usize::from(u16::from_le_bytes([list[4], list[5]]));
    let mut at = 8;
    let mut found = Vec::new();
    for _ in 0..count {
        let size = usize::from(u16::from_le_bytes([list[at + 2], list[at + 3]]));
        found.push((list[at + 1], list[at + 8..at + size] == PACKAGES));
        at += size;
    }
    found
}

fn show(label: &str, path: &Path) {
    let entry = opened(path);
    let facts = private::facts(&entry).unwrap();
    println!(
        "PROBE {label}: control {:#06x} entries (flags, is packages) {:?}",
        control(&entry),
        facts.list.as_deref().map(flags)
    );
}

fn make(path: &Path, owner: &[u8], auto: Option<bool>) {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut words = owner.to_vec();
    let mut descriptor = SECURITY_DESCRIPTOR::default();
    let raw: PSECURITY_DESCRIPTOR = (&raw mut descriptor).cast();
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: raw,
        bInheritHandle: 0,
    };
    let made = match auto {
        None => unsafe { CreateDirectoryW(wide.as_ptr(), std::ptr::null()) },
        Some(request) => unsafe {
            assert_ne!(InitializeSecurityDescriptor(raw, 1), 0);
            assert_ne!(SetSecurityDescriptorOwner(raw, words.as_mut_ptr().cast(), 0), 0);
            if request {
                assert_ne!(SetSecurityDescriptorControl(raw, 0x0100, 0x0100), 0);
            }
            CreateDirectoryW(wide.as_ptr(), &attributes)
        },
    };
    assert_ne!(made, 0, "{}", std::io::Error::last_os_error());
}

fn icacls(args: &[&str]) {
    let done = std::process::Command::new("icacls")
        .args(args)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(done.success());
}

#[test]
fn probe_which_descriptor_gives_inherited_entries() {
    let root = std::env::temp_dir().join(format!("crazy-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let held = private::dir(&root).unwrap();
    let user = private::user_sid().unwrap();
    show("parent (W1 dir)", &root);
    let shapes = [
        ("a owner only", Some(false)),
        ("b owner and auto-inherit request", Some(true)),
        ("c no descriptor", None),
    ];
    for (label, auto) in shapes {
        make(&root.join(&label[..1]), &user, auto);
        show(label, &root.join(&label[..1]));
    }
    let lease = root.to_str().unwrap().to_owned();
    icacls(&[&lease, "/grant", "*S-1-15-2-1:(OI)(CI)(M)"]);
    show("parent after the grant", &root);
    for (label, _) in shapes {
        show(&format!("{label}, after the grant"), &root.join(&label[..1]));
    }
    make(&root.join("d"), &user, Some(true));
    show("d auto-inherit request, made after the grant", &root.join("d"));
    icacls(&[&lease, "/remove:g", "*S-1-15-2-1"]);
    for (label, _) in shapes {
        show(&format!("{label}, after the removal"), &root.join(&label[..1]));
    }
    show("d, after the removal", &root.join("d"));
    drop(held);
    let _ = std::fs::remove_dir_all(&root);
}
