//! Leases per holder (threat model section 6, C1): each verified caller's hold runs out unless
//! renewed, at most `HOLDERS_MAX` at once, and one caller never touches another's.

use std::collections::BTreeMap;

pub const HOLDERS_MAX: usize = 4;

/// Who holds a lease, from the caller's audit token: effective uid, pid and pid version, so a
/// reused pid is another holder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Holder {
    pub euid: u32,
    pub pid: i32,
    pub pid_version: i32,
}

/// One lease: the monotonic millisecond it runs out at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    until_ms: u64,
}

impl Lease {
    /// A lease of `secs` seconds from `now_ms`.
    pub fn new(now_ms: u64, secs: u16) -> Self {
        let _ = (now_ms, secs);
        Self { until_ms: 0 }
    }

    /// Renews the lease for `secs` seconds from `now_ms`.
    pub fn extend(&mut self, now_ms: u64, secs: u16) {
        let _ = (now_ms, secs);
    }

    /// Whether the lease has run out at `now_ms`.
    pub fn expired(&self, now_ms: u64) -> bool {
        let _ = now_ms;
        false
    }

    /// Whole seconds left at `now_ms`, 0 once run out.
    pub fn left_s(&self, now_ms: u64) -> u16 {
        let _ = now_ms;
        u16::MAX
    }
}

/// The fifth holder's refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Full;

/// Every live lease, keyed by holder.
#[derive(Debug, Default)]
pub struct Leases {
    held: BTreeMap<Holder, Lease>,
}

impl Leases {
    /// Starts or renews `holder`'s lease. Output: `Full` for a new holder past `HOLDERS_MAX`.
    pub fn hold(&mut self, holder: Holder, now_ms: u64, secs: u16) -> Result<(), Full> {
        let _ = (holder, now_ms, secs, &self.held);
        Err(Full)
    }

    /// Ends `holder`'s own lease. Output: whether it had one.
    pub fn release(&mut self, holder: &Holder) -> bool {
        let _ = holder;
        true
    }

    /// Ends every lease that ran out at `now_ms`. Output: how many ended.
    pub fn expire(&mut self, now_ms: u64) -> usize {
        let _ = now_ms;
        0
    }

    /// Ends every lease.
    pub fn clear(&mut self) {}

    /// How many leases live.
    pub fn len(&self) -> usize {
        usize::MAX
    }

    /// Whether no lease lives.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The most seconds any lease has left at `now_ms`.
    pub fn left_s(&self, now_ms: u64) -> u16 {
        let _ = now_ms;
        0
    }

    /// When the next lease runs out, if any lives.
    pub fn next_expiry_ms(&self) -> Option<u64> {
        None
    }
}
