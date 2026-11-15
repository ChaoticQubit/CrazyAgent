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
