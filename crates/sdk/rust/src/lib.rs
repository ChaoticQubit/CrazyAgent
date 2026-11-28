#![forbid(unsafe_code)]

extern crate alloc;

pub mod generated;

/// The SDK modules capgen writes from `capgen/fixture_served`: a kernel stem and two
/// plugin-served stems, for the host crate's served rows; never part of a release build.
#[cfg(feature = "test-support")]
pub mod fixture_served;
pub mod wire;
pub use generated::CAPABILITIES;
pub use generated::host::Host;
pub use generated::net;
pub use generated::process;
pub use generated::secrets;
pub use generated::settings;

/// Why the bus did not complete a call: no grant, a tier above the caller's, the caller
/// turned off by the owner, the provider's rejection, or its failure. Rejected and Failed
/// carry the provider's code unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    NotGranted {
        capability: &'static str,
        needs: &'static str,
    },
    TierTooHigh {
        capability: &'static str,
    },
    PluginOff {
        capability: &'static str,
    },
    Rejected {
        code: String,
    },
    Failed {
        code: String,
    },
}

/// The plugin tiers, lowest first; a caller may use its own tier or lower.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Kernel,
    T1,
    T2,
    T3,
}

/// One capability's static facts, as capgen writes them: name, tier and permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability {
    pub name: &'static str,
    pub tier: Tier,
    pub needs: &'static str,
}

/// The most bytes one `text` value of a capability call may hold.
pub const TEXT_BYTES_MAX: usize = 8_192;
/// The most bytes one `bytes` value of a capability call may hold.
pub const BYTES_MAX: usize = 4_194_304;
/// The most items one `list` value of a capability call may hold.
pub const LIST_ITEMS_MAX: usize = 64;

#[cfg(test)]
mod events_tests;
#[cfg(test)]
pub mod fixture_generated;
#[cfg(test)]
pub mod fixture_two;
#[cfg(test)]
pub mod fixture_wire;
#[cfg(test)]
mod tests;
