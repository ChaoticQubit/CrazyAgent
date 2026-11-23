/// The plugin tiers, lowest first; `Kernel` is a kernel module's capability, never a
/// plugin's tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    Kernel,
    T1,
    T2,
    T3,
}

impl Tier {
    /// The tier a `plugin.toml` tier word names. Input: the word. Output: `T1`, `T2` or
    /// `T3` for `t1`, `t2` or `t3`, else `None`; no word names `Kernel`.
    pub fn from_word(word: &str) -> Option<Self> {
        match word {
            "t1" => Some(Self::T1),
            "t2" => Some(Self::T2),
            "t3" => Some(Self::T3),
            _ => None,
        }
    }
}

/// One capability's facts from its `.cap` file: its dotted name, the lowest tier that may
/// call it, the permission a caller needs and the event kinds a call emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permission {
    pub name: &'static str,
    pub tier: Tier,
    pub needs: &'static str,
    pub emits: &'static [&'static str],
}

/// One bundled code plugin's grant from its `plugin.toml`: its id, its tier, the
/// permissions it holds and whether it is essential, so its switch stays locked on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundledGrant {
    pub plugin: &'static str,
    pub tier: Tier,
    pub permissions: &'static [&'static str],
    pub essential: bool,
}
