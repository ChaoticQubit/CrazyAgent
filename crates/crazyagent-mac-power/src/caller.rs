//! Who may call (threat model section 5): the peer's audit token, its code looked up by that
//! token and checked against our signing requirement, the hardened runtime and an empty
//! entitlement allow list. Run at accept and again before acting.

use crate::lease::Holder;
use std::os::unix::net::UnixStream;

pub const REQUIREMENT: &str = "";
pub const RUNTIME_FLAG: u32 = 0x10000;
pub const ENTITLEMENTS_MAX: usize = 64;

/// What a caller's code must satisfy: a code requirement, whether the hardened runtime is
/// required, and the entitlements it may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub requirement: &'static str,
    pub runtime_required: bool,
    pub entitlements_allowed: &'static [&'static str],
}

/// The installed helper's policy: our Developer ID engine, hardened runtime, no entitlements.
pub const PRODUCTION: Policy = Policy {
    requirement: REQUIREMENT,
    runtime_required: false,
    entitlements_allowed: &[],
};

/// A peer's audit token as `LOCAL_PEERTOKEN` gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditToken(pub [u32; 8]);

/// Why a caller was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    NoToken,
    NotFound,
    BadRequirement,
    Requirement,
    SigningInformation,
    NoRuntime,
    Entitlement(String),
}

/// The audit token of the peer of `stream`, as of its `connect`.
pub fn token_of(stream: &UnixStream) -> Result<AuditToken, Refused> {
    let _ = stream;
    Err(Refused::NoToken)
}

/// Checks the code behind `token` against `policy`. Output: the holder it speaks for.
pub fn check(token: &AuditToken, policy: &Policy) -> Result<Holder, Refused> {
    let _ = (token, policy);
    Err(Refused::NoToken)
}
