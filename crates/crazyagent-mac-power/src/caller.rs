//! Who may call (threat model section 5): the peer's audit token, its code looked up by that
//! token and checked against our signing requirement, the hardened runtime and an empty
//! entitlement allow list. Run at accept and again before acting.

use crate::ffi;
use crate::lease::Holder;
use core_foundation::base::TCFType;
use core_foundation::data::CFData;
use security_framework::os::macos::code_signing::{
    Flags, GuestAttributes, SecCode, SecRequirement,
};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;

pub const REQUIREMENT: &str = "anchor apple generic and certificate leaf[subject.OU] = \
\"GK729B5HZQ\" and identifier \"crazyagent\" and certificate 1[field.1.2.840.113635.100.6.2.6] \
exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists";
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
    runtime_required: true,
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
    let token = ffi::peer_token(stream.as_fd()).map_err(|_| Refused::NoToken)?;
    Ok(AuditToken(token))
}

/// Checks the code behind `token` against `policy`. Output: the holder it speaks for.
pub fn check(token: &AuditToken, policy: &Policy) -> Result<Holder, Refused> {
    let requirement: SecRequirement = policy
        .requirement
        .parse()
        .map_err(|_| Refused::BadRequirement)?;
    let code = guest(token)?;
    code.check_validity(Flags::NONE, &requirement)
        .map_err(|_| Refused::Requirement)?;
    let signing =
        ffi::signing_information(&code, ENTITLEMENTS_MAX).ok_or(Refused::SigningInformation)?;
    if policy.runtime_required && signing.flags & RUNTIME_FLAG == 0 {
        return Err(Refused::NoRuntime);
    }
    let mut entitlements = signing.entitlements;
    assert!(entitlements.len() <= ENTITLEMENTS_MAX);
    entitlements.sort();
    let allowed = |name: &String| policy.entitlements_allowed.contains(&name.as_str());
    if let Some(name) = entitlements.into_iter().find(|name| !allowed(name)) {
        return Err(Refused::Entitlement(name));
    }
    let holder = holder_of(token);
    assert!(holder.pid > 0);
    Ok(holder)
}

/// The effective uid, pid and pid version an audit token names: words 1, 5 and 7, as the
/// libbsm accessors read them.
fn holder_of(token: &AuditToken) -> Holder {
    let [_, euid, _, _, _, pid, _, pid_version] = token.0;
    Holder {
        euid,
        pid: pid.cast_signed(),
        pid_version: pid_version.cast_signed(),
    }
}

/// The running code behind `token`, looked up by the kernel. Output: the code, or `NotFound`
/// for any lookup failure (a caller that exited or exec'd since its token was taken).
fn guest(token: &AuditToken) -> Result<SecCode, Refused> {
    let bytes: Vec<u8> = token.0.iter().flat_map(|word| word.to_ne_bytes()).collect();
    assert_eq!(bytes.len(), size_of::<AuditToken>());
    let data = CFData::from_buffer(&bytes);
    let mut attributes = GuestAttributes::new();
    attributes.set_audit_token(data.as_concrete_TypeRef());
    let code = SecCode::copy_guest_with_attribues(None, &attributes, Flags::NONE);
    code.map_err(|_| Refused::NotFound)
}
