//! The helper's daemon as `SMAppService` sees it (DEC-167 addendum 18, QUESTION 35a): its status,
//! register, unregister, and System Settings > Login Items where the owner approves it. One seam,
//! so the K0-9 result at the gate moves only the caller.

use crate::ffi;

/// The daemon's plist inside the app bundle's `Contents/Library/LaunchDaemons`.
pub const PLIST_NAME: &str = "dev.crazyagent.power.plist";

/// `SMAppServiceStatus`: never registered, registered and allowed, waiting for the owner's
/// approval, or no such plist in the bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    NotRegistered,
    Enabled,
    RequiresApproval,
    NotFound,
}

impl Status {
    /// Reads Apple's raw status value (0 not registered, 1 enabled, 2 requires approval, 3 not
    /// found; PROBES-keepawake.md, Apple documentation). Input: the value. Output: the status,
    /// or none for a value this build does not know, which callers treat as not available.
    pub fn from_raw(raw: isize) -> Option<Self> {
        match raw {
            0 => Some(Self::NotRegistered),
            1 => Some(Self::Enabled),
            2 => Some(Self::RequiresApproval),
            3 => Some(Self::NotFound),
            _ => None,
        }
    }
}

/// What the engine asks of the daemon's registration. `Daemon` is the system's; rows use a
/// stand-in, so no row registers anything with macOS.
pub trait Service: Send + std::fmt::Debug {
    /// The status, or none when the system gave none this build knows.
    fn status(&self) -> Option<Status>;
    /// Registers the daemon, after which the owner approves it in Login Items.
    fn register(&self) -> Result<(), String>;
    /// Unregisters the daemon; launchd stops the helper with SIGTERM.
    fn unregister(&self) -> Result<(), String>;
    /// Opens System Settings > Login Items.
    fn open_login_items(&self) -> Result<(), String>;
}

/// This app bundle's daemon through `SMAppService` (macOS 13 and later).
#[derive(Debug, Default, Clone, Copy)]
pub struct Daemon;

impl Service for Daemon {
    fn status(&self) -> Option<Status> {
        ffi::app_service_status(PLIST_NAME).and_then(Status::from_raw)
    }

    fn register(&self) -> Result<(), String> {
        ffi::app_service_change(PLIST_NAME, ffi::Change::Register)
    }

    fn unregister(&self) -> Result<(), String> {
        ffi::app_service_change(PLIST_NAME, ffi::Change::Unregister)
    }

    fn open_login_items(&self) -> Result<(), String> {
        ffi::open_login_items()
    }
}
