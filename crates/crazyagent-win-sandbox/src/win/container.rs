use super::{BoundaryError, Words, last_error, wide, windows};
use crate::profile::Capability;
use windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS;
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    CreateWellKnownSid, FreeSid, GetLengthSid, PSID, SECURITY_MAX_SID_SIZE, WELL_KNOWN_SID_TYPE,
    WinCapabilityInternetClientSid,
};

const PROFILE_EXISTS: i32 = (0x8007_0000u32 | ERROR_ALREADY_EXISTS) as i32;
const SID_BYTES_MIN: usize = 8;

/// Opens the AppContainer profile named `name`, creating it the first time, and returns a copy
/// of its SID. Input: a name `check_container` accepted. Output: the SID, or the failed step.
pub(super) fn open(name: &str) -> Result<Words, BoundaryError> {
    let name = wide(name);
    let display = wide("CrazyAgent agent");
    let description = wide("The boundary one CrazyAgent agent runs in");
    match create_profile(&name, &display, &description) {
        Ok(sid) => Ok(sid),
        Err(PROFILE_EXISTS) => derive_sid(&name).map_err(|code| BoundaryError::Windows {
            step: "open the agent's container",
            code: code as u32,
        }),
        Err(code) => Err(BoundaryError::Windows {
            step: "create the agent's container",
            code: code as u32,
        }),
    }
}

/// The SIDs of the capabilities the agent starts with. Input: the profile's capabilities.
/// Output: one SID each, or the failed step.
pub(super) fn capabilities(list: &[Capability]) -> Result<Vec<Words>, BoundaryError> {
    list.iter()
        .map(|capability| {
            well_known(match capability {
                Capability::InternetClient => WinCapabilityInternetClientSid,
            })
            .map_err(windows("build a capability SID"))
        })
        .collect()
}

/// Creates the AppContainer profile and returns a copy of its SID. Relies on the three names
/// being NUL-terminated UTF-16 that outlive the call, on passing no capabilities, and on the
/// system handing back a SID it allocated for this caller, which `take_sid` copies and frees.
fn create_profile(name: &[u16], display: &[u16], description: &[u16]) -> Result<Words, i32> {
    assert!(
        [name, display, description]
            .iter()
            .all(|text| text.last() == Some(&0))
    );
    let mut sid: PSID = std::ptr::null_mut();
    let result = unsafe {
        CreateAppContainerProfile(
            name.as_ptr(),
            display.as_ptr(),
            description.as_ptr(),
            std::ptr::null(),
            0,
            &mut sid,
        )
    };
    if result < 0 {
        return Err(result);
    }
    assert!(!sid.is_null());
    Ok(unsafe { take_sid(sid) })
}

/// Derives the SID of an existing AppContainer profile from its name. Relies on `name` being
/// NUL-terminated UTF-16 that outlives the call and on the system handing back a SID it
/// allocated for this caller, which `take_sid` copies and frees.
fn derive_sid(name: &[u16]) -> Result<Words, i32> {
    assert_eq!(name.last(), Some(&0));
    let mut sid: PSID = std::ptr::null_mut();
    let result = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
    if result < 0 {
        return Err(result);
    }
    assert!(!sid.is_null());
    Ok(unsafe { take_sid(sid) })
}

/// Copies a SID the system allocated, then frees the original with `FreeSid`.
/// Safety: `sid` must point at a valid SID the system allocated for this caller, which nothing
/// else frees and nothing uses after this call.
unsafe fn take_sid(sid: PSID) -> Words {
    let len = unsafe { GetLengthSid(sid) } as usize;
    assert!((SID_BYTES_MIN..=SECURITY_MAX_SID_SIZE as usize).contains(&len));
    let copy = unsafe { Words::copy_from(sid.cast_const().cast(), len) };
    unsafe { FreeSid(sid) };
    copy
}

/// Builds one well-known SID into an owned buffer. Relies on the buffer holding
/// `SECURITY_MAX_SID_SIZE` bytes, the most any SID takes, and on `size` reporting the bytes the
/// call wrote.
fn well_known(kind: WELL_KNOWN_SID_TYPE) -> Result<Words, u32> {
    let mut buffer = vec![0u32; (SECURITY_MAX_SID_SIZE as usize).div_ceil(4)];
    let mut size = SECURITY_MAX_SID_SIZE;
    let built = unsafe {
        CreateWellKnownSid(
            kind,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if built == 0 {
        return Err(last_error());
    }
    let len = size as usize;
    assert!((SID_BYTES_MIN..=SECURITY_MAX_SID_SIZE as usize).contains(&len));
    let bytes: Vec<u8> = buffer
        .iter()
        .flat_map(|word| word.to_ne_bytes())
        .take(len)
        .collect();
    Ok(Words::from_bytes(&bytes))
}
