use super::{BoundaryError, Words, last_error, wide, windows};
use crate::profile::Capability;
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_BAD_ENVIRONMENT, ERROR_FILE_NOT_FOUND, ERROR_NOT_FOUND,
    ERROR_PATH_NOT_FOUND,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    CreateWellKnownSid, FreeSid, GetLengthSid, PSID, SECURITY_MAX_SID_SIZE, WELL_KNOWN_SID_TYPE,
    WinCapabilityInternetClientSid,
};

const PROFILE_EXISTS: i32 = (0x8007_0000u32 | ERROR_ALREADY_EXISTS) as i32;
const PROFILE_MISSING: [i32; 3] = [
    (0x8007_0000u32 | ERROR_FILE_NOT_FOUND) as i32,
    (0x8007_0000u32 | ERROR_PATH_NOT_FOUND) as i32,
    (0x8007_0000u32 | ERROR_NOT_FOUND) as i32,
];
const PROFILE_RACED: i32 = (0x8007_0000u32 | ERROR_BAD_ENVIRONMENT) as i32;
const CREATE_TRIES: u32 = 3;
const CREATE_PAUSE: Duration = Duration::from_millis(50);
const SID_BYTES_MIN: usize = 8;

/// Opens the AppContainer profile named `name`, creating it the first time, and returns a copy
/// of its SID; a creation that lost Windows' Packages-folder race is tried again ([`retried`]).
/// Input: a name `check_container` accepted. Output: the SID, or the failed step.
pub(super) fn open(name: &str) -> Result<Words, BoundaryError> {
    let name = wide(name);
    let display = wide("CrazyAgent agent");
    let description = wide("The boundary one CrazyAgent agent runs in");
    match retried(
        || create_profile(&name, &display, &description),
        CREATE_PAUSE,
    ) {
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

/// The SID of the AppContainer named `name`, derived from the name as Windows does, whether or
/// not its profile exists. Input: a name `check_container` accepted. Output: the SID, or the
/// failed step.
pub(super) fn sid(name: &str) -> Result<Words, BoundaryError> {
    derive_sid(&wide(name)).map_err(|code| BoundaryError::Windows {
        step: "derive the agent's container SID",
        code: code as u32,
    })
}

/// Deletes the AppContainer profile named `name` with the folder and settings Windows keeps
/// for it, so nothing the container wrote there outlives it; a profile already gone is fine.
/// Relies on the name being NUL-terminated UTF-16 that outlives the call. Input: a name
/// `check_container` accepted. Output: Ok, or the failed step.
pub(super) fn delete(name: &str) -> Result<(), BoundaryError> {
    let name = wide(name);
    assert_eq!(name.last(), Some(&0));
    let result = unsafe { DeleteAppContainerProfile(name.as_ptr()) };
    if result >= 0 || PROFILE_MISSING.contains(&result) {
        return Ok(());
    }
    Err(BoundaryError::Windows {
        step: "delete the agent's container",
        code: result as u32,
    })
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

/// Calls `create`, the AppContainer creation, again while it returns 0x8007000A: the race
/// Windows loses when two first creations for one user make that user's Packages folder at
/// once, after which the folder exists for the next call. At most `CREATE_TRIES` calls,
/// `pause` apart; any other result is taken at once. Inputs: the call and the pause. Output:
/// the first other result, or the race after the last try, which the caller refuses.
fn retried<T>(mut create: impl FnMut() -> Result<T, i32>, pause: Duration) -> Result<T, i32> {
    assert!(pause <= CREATE_PAUSE);
    assert!(CREATE_TRIES >= 2);
    let mut tries = 1;
    let mut result = create();
    while tries < CREATE_TRIES && matches!(result, Err(PROFILE_RACED)) {
        std::thread::sleep(pause);
        tries += 1;
        result = create();
    }
    assert!(tries <= CREATE_TRIES);
    result
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

#[cfg(test)]
mod tests {
    use super::{CREATE_TRIES, PROFILE_EXISTS, PROFILE_MISSING, PROFILE_RACED, retried};
    use std::time::{Duration, Instant};

    const ACCESS_DENIED: i32 = 0x8007_0005u32 as i32;

    /// Runs [`retried`] over scripted results, one per call, the last one repeated. Inputs: the
    /// results in call order and the pause. Output: what `retried` returned and the calls made.
    fn scripted(results: &[Result<u32, i32>], pause: Duration) -> (Result<u32, i32>, usize) {
        assert!(!results.is_empty());
        let mut calls = 0;
        let result = retried(
            || {
                calls += 1;
                results[(calls - 1).min(results.len() - 1)]
            },
            pause,
        );
        assert!(calls >= 1);
        (result, calls)
    }

    #[test]
    fn a_lost_packages_race_is_tried_again_and_the_next_result_stands() {
        let (result, calls) = scripted(&[Err(PROFILE_RACED), Ok(7)], Duration::ZERO);
        assert_eq!((result, calls), (Ok(7), 2));
        let raced_then_exists = [Err(PROFILE_RACED), Err(PROFILE_EXISTS)];
        let (result, calls) = scripted(&raced_then_exists, Duration::ZERO);
        assert_eq!((result, calls), (Err(PROFILE_EXISTS), 2));
    }

    #[test]
    fn a_lasting_race_refuses_after_three_tries_with_the_same_code_and_the_pause_kept() {
        let pause = Duration::from_millis(20);
        let started = Instant::now();
        let (result, calls) = scripted(&[Err(PROFILE_RACED)], pause);
        assert_eq!(CREATE_TRIES, 3);
        assert_eq!((result, calls), (Err(PROFILE_RACED), 3));
        assert!(started.elapsed() >= pause * (CREATE_TRIES - 1));
    }

    #[test]
    fn any_other_result_is_taken_at_once() {
        let firsts = [
            Ok(1),
            Err(PROFILE_EXISTS),
            Err(ACCESS_DENIED),
            Err(PROFILE_MISSING[0]),
        ];
        for first in firsts {
            let (result, calls) = scripted(&[first, Ok(2)], Duration::ZERO);
            assert_eq!((result, calls), (first, 1), "{first:?}");
        }
    }
}
