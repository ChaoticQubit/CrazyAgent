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
        assert!(secs > 0);
        let lease = Self {
            until_ms: now_ms.saturating_add(u64::from(secs) * 1_000),
        };
        assert!(lease.until_ms >= now_ms);
        lease
    }

    /// Renews the lease for `secs` seconds from `now_ms`.
    pub fn extend(&mut self, now_ms: u64, secs: u16) {
        *self = Self::new(now_ms, secs);
        assert!(self.left_s(now_ms) <= secs);
    }

    /// Whether the lease has run out at `now_ms`.
    pub fn expired(&self, now_ms: u64) -> bool {
        now_ms >= self.until_ms
    }

    /// Whole seconds left at `now_ms`, rounded up, 0 once run out.
    pub fn left_s(&self, now_ms: u64) -> u16 {
        let left_ms = self.until_ms.saturating_sub(now_ms);
        let left_s = left_ms.div_ceil(1_000);
        u16::try_from(left_s).unwrap_or(u16::MAX)
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
        assert!(self.held.len() <= HOLDERS_MAX);
        if let Some(lease) = self.held.get_mut(&holder) {
            lease.extend(now_ms, secs);
            return Ok(());
        }
        if self.held.len() == HOLDERS_MAX {
            return Err(Full);
        }
        self.held.insert(holder, Lease::new(now_ms, secs));
        assert!(self.held.len() <= HOLDERS_MAX);
        Ok(())
    }

    /// Ends `holder`'s own lease. Output: whether it had one.
    pub fn release(&mut self, holder: &Holder) -> bool {
        let had = self.held.remove(holder).is_some();
        assert!(!self.held.contains_key(holder));
        had
    }

    /// Ends every lease that ran out at `now_ms`. Output: how many ended.
    pub fn expire(&mut self, now_ms: u64) -> usize {
        let before = self.held.len();
        self.held.retain(|_, lease| !lease.expired(now_ms));
        assert!(self.held.values().all(|lease| !lease.expired(now_ms)));
        before - self.held.len()
    }

    /// Ends every lease.
    pub fn clear(&mut self) {
        self.held.clear();
    }

    /// How many leases live.
    pub fn len(&self) -> usize {
        assert!(self.held.len() <= HOLDERS_MAX);
        self.held.len()
    }

    /// Whether no lease lives.
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// The most seconds any lease has left at `now_ms`.
    pub fn left_s(&self, now_ms: u64) -> u16 {
        let left = self.held.values().map(|lease| lease.left_s(now_ms));
        left.max().unwrap_or(0)
    }

    /// When the next lease runs out, if any lives.
    pub fn next_expiry_ms(&self) -> Option<u64> {
        self.held.values().map(|lease| lease.until_ms).min()
    }
}
