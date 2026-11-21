#![forbid(unsafe_code)]

pub mod generated;
pub use generated::host::Host;
pub use generated::settings;

/// Why the bus did not complete a call: no grant, a tier above the caller's, the provider's
/// rejection, or its failure. Rejected and Failed carry the provider's code unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    NotGranted {
        capability: &'static str,
        needs: &'static str,
    },
    TierTooHigh {
        capability: &'static str,
    },
    Rejected {
        code: String,
    },
    Failed {
        code: String,
    },
}

/// The plugin tiers of DEC-206 Q5, lowest first; a caller may use its own tier or lower.
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

#[cfg(test)]
mod tests;
