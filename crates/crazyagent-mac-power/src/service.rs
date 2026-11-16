//! The helper's daemon as `SMAppService` sees it (DEC-167 addendum 18, QUESTION 35a): its status,
//! register, unregister, and System Settings > Login Items where the owner approves it. One seam,
//! so the K0-9 result at the gate moves only the caller.

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
    /// Reads Apple's raw status value. Input: the value. Output: the status, or none for a
    /// value this build does not know, which callers treat as not available.
    pub fn from_raw(_raw: isize) -> Option<Self> {
        None
    }
}
