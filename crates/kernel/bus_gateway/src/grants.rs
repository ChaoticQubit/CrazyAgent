use std::collections::{BTreeMap, BTreeSet};

use kernel_plugin_host::permission::{self, BundledGrant, Permission};
use sdk::Tier;

/// The most plugins one grant table may hold.
pub const PLUGINS_MAX: usize = 1_024;
/// The most permissions one plugin may be granted.
pub const PERMISSIONS_MAX: usize = 64;

/// The kernel's grant table: per plugin id, its tier and the permissions it holds.
/// Built by the host side from what each plugin asked for; a plugin never writes it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Grants {
    by_plugin: BTreeMap<&'static str, (Tier, BTreeSet<&'static str>)>,
}

/// The SDK tier of a plugin_host tier, variant for variant in the same order. Input: the
/// tier. Output: its SDK tier.
pub(crate) fn sdk_tier(tier: permission::Tier) -> Tier {
    match tier {
        permission::Tier::Kernel => Tier::Kernel,
        permission::Tier::T1 => Tier::T1,
        permission::Tier::T2 => Tier::T2,
        permission::Tier::T3 => Tier::T3,
    }
}

impl Grants {
    /// Builds the table from generated grants, checked against the generated permission
    /// table first: every granted permission is one some capability needs, and no
    /// capability needing it sits above the plugin's tier. Inputs: the bundled grants and
    /// the permission table. Output: the table, `grants_unknown_permission`,
    /// `grants_tier_too_high`, or a refusal of [`Grants::new`]'s checks.
    pub fn from_table(
        grants: &[BundledGrant],
        permissions: &[Permission],
    ) -> Result<Self, &'static str> {
        if grants.len() > PLUGINS_MAX {
            return Err("grants_too_many");
        }
        let mut entries = Vec::with_capacity(grants.len());
        for grant in grants {
            for held in grant.permissions {
                let mut needing = permissions.iter().filter(|p| p.needs == *held).peekable();
                if needing.peek().is_none() {
                    return Err("grants_unknown_permission");
                }
                if needing.any(|p| p.tier > grant.tier) {
                    return Err("grants_tier_too_high");
                }
            }
            entries.push((grant.plugin, sdk_tier(grant.tier), grant.permissions));
        }
        assert_eq!(entries.len(), grants.len());
        Self::build(&entries)
    }

    /// Builds the table with no permission table check, for tests only; the engine builds
    /// through [`Grants::from_table`]. Input: per plugin its id, tier and granted
    /// permissions. Output: as the shared checks.
    #[cfg(any(test, feature = "test-support"))]
    pub fn new(entries: &[(&'static str, Tier, &[&'static str])]) -> Result<Self, &'static str> {
        Self::build(entries)
    }

    /// The checks every table passes. Input: per plugin its id, tier and granted
    /// permissions. Output: the table, or `grants_duplicate_plugin`,
    /// `grants_duplicate_permission`, `grants_kernel_tier` (a plugin is never a kernel
    /// module) or `grants_too_many`.
    fn build(entries: &[(&'static str, Tier, &[&'static str])]) -> Result<Self, &'static str> {
        if entries.len() > PLUGINS_MAX {
            return Err("grants_too_many");
        }
        let mut by_plugin = BTreeMap::new();
        for (plugin, tier, permissions) in entries {
            if *tier == Tier::Kernel {
                return Err("grants_kernel_tier");
            }
            if permissions.len() > PERMISSIONS_MAX {
                return Err("grants_too_many");
            }
            let mut held = BTreeSet::new();
            for permission in *permissions {
                if !held.insert(*permission) {
                    return Err("grants_duplicate_permission");
                }
            }
            if by_plugin.insert(*plugin, (*tier, held)).is_some() {
                return Err("grants_duplicate_plugin");
            }
        }
        assert_eq!(by_plugin.len(), entries.len());
        Ok(Self { by_plugin })
    }

    /// The tier and permissions of one plugin. Input: the plugin id.
    /// Output: its tier and permissions, or `None` for a plugin the table does not hold.
    pub(crate) fn of(&self, plugin: &str) -> Option<&(Tier, BTreeSet<&'static str>)> {
        self.by_plugin.get(plugin)
    }
}
