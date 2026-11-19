#![cfg(target_os = "windows")]

use windows_sys::Win32::System::LibraryLoader::{
    FindResourceW, LoadResource, LockResource, SizeofResource,
};

/// Reads application manifest resource 1 of this test exe, which the desktop build script
/// links into every exe of the crate, and checks it is the Common-Controls v6 dependency the
/// app carries, with no execution level. Input: none. Output: passes or panics with the
/// manifest text.
#[test]
fn this_test_exe_loads_with_the_common_controls_v6_manifest_the_app_carries() {
    let manifest_id = std::ptr::without_provenance::<u16>(1);
    let manifest_type = std::ptr::without_provenance::<u16>(24);
    let exe = std::ptr::null_mut();
    let found = unsafe { FindResourceW(exe, manifest_id, manifest_type) };
    assert!(
        !found.is_null(),
        "the test exe has application manifest resource 1"
    );
    let size = unsafe { SizeofResource(exe, found) } as usize;
    assert!(size > 0 && size <= 64 * 1024, "manifest of {size} bytes");
    let data = unsafe { LockResource(LoadResource(exe, found)) };
    assert!(!data.is_null(), "the manifest resource loads");
    let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size) };
    let text = std::str::from_utf8(bytes).expect("the manifest is UTF-8 XML");
    for part in [
        "Microsoft.Windows.Common-Controls",
        "6.0.0.0",
        "6595b64144ccf1df",
    ] {
        assert!(text.contains(part), "the manifest names {part}: {text}");
    }
    assert!(
        !text.contains("trustInfo"),
        "the manifest adds no execution level: {text}"
    );
}
