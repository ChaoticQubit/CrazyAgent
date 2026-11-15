use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::System::Power::{
    POWER_REQUEST_TYPE, PowerClearRequest, PowerCreateRequest, PowerRequestDisplayRequired,
    PowerRequestSystemRequired, PowerSetRequest,
};
use windows_sys::Win32::System::SystemServices::POWER_REQUEST_CONTEXT_VERSION;
use windows_sys::Win32::System::Threading::{
    POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0,
};

const REASON_UNITS_MAX: usize = 256;

/// What a power request keeps on: the system (no idle sleep) or the display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    System,
    Display,
}

impl RequestKind {
    fn raw(self) -> POWER_REQUEST_TYPE {
        match self {
            Self::System => PowerRequestSystemRequired,
            Self::Display => PowerRequestDisplayRequired,
        }
    }
}

/// One Windows power request. Windows honours the kinds set on it until they are cleared
/// or the handle closes, which happens when this value is dropped or the process ends.
#[derive(Debug)]
pub struct Request {
    handle: OwnedHandle,
}

impl Request {
    /// Creates a power request with a plain reason, holding nothing yet. Input: the reason
    /// Windows shows in its request list, 1 to 256 UTF-16 units with no NUL. Output: the
    /// request, or the Windows error. Relies on the reason buffer ending in a NUL and
    /// outliving the call, and on the returned handle being null or invalid on failure and
    /// otherwise a new handle this caller owns alone.
    pub fn new(reason: &str) -> io::Result<Self> {
        let mut text: Vec<u16> = reason.encode_utf16().collect();
        if text.is_empty() || text.len() > REASON_UNITS_MAX || text.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "power request reason out of bounds",
            ));
        }
        text.push(0);
        let context = REASON_CONTEXT {
            Version: POWER_REQUEST_CONTEXT_VERSION,
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 {
                SimpleReasonString: text.as_mut_ptr(),
            },
        };
        let handle = unsafe { PowerCreateRequest(&raw const context) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            handle: unsafe { OwnedHandle::from_raw_handle(handle) },
        })
    }

    /// Turns one kind on. Input: the kind. Output: Ok, or the Windows error. Relies on the
    /// handle being a live power request handle for the call.
    pub fn set(&self, kind: RequestKind) -> io::Result<()> {
        let set = unsafe { PowerSetRequest(self.handle.as_raw_handle(), kind.raw()) };
        if set == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Turns one kind off. Input: the kind. Output: Ok, or the Windows error. Relies on the
    /// handle being a live power request handle for the call.
    pub fn clear(&self, kind: RequestKind) -> io::Result<()> {
        let cleared = unsafe { PowerClearRequest(self.handle.as_raw_handle(), kind.raw()) };
        if cleared == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Status, status, status_of};

    #[test]
    fn the_power_status_fields_map_to_the_source_and_the_charge() {
        let read = |line: u8, flag: u8, percent: u8| status_of(line, flag, percent);
        assert_eq!(
            read(1, 8, 83),
            Status {
                ac: Some(true),
                battery_pct: Some(83)
            }
        );
        assert_eq!(
            read(0, 1, 100),
            Status {
                ac: Some(false),
                battery_pct: Some(100)
            }
        );
        assert_eq!(read(0, 4, 0).battery_pct, Some(0));
        assert_eq!(read(2, 0, 40).ac, Some(false), "backup power is battery");
        assert_eq!(read(255, 0, 40).ac, None);
        assert_eq!(read(7, 0, 40).ac, None, "a line value Windows does not name");
        assert_eq!(read(1, 128, 255).battery_pct, None, "no system battery");
        assert_eq!(
            read(1, 128, 50).battery_pct,
            None,
            "the no battery flag wins over a percent"
        );
        assert_eq!(read(0, 255, 50).battery_pct, None, "unknown battery flag");
        assert_eq!(read(0, 0, 255).battery_pct, None, "unknown percent");
        assert_eq!(read(0, 0, 101).battery_pct, None);
    }

    #[test]
    fn this_computer_reports_a_power_status() {
        let now = status().unwrap();
        assert!(now.battery_pct.is_none_or(|percent| percent <= 100));
    }
}
