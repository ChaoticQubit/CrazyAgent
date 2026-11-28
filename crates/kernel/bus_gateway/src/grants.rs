use std::collections::{BTreeMap, BTreeSet};

use kernel_plugin_host::permission::{self, BundledGrant, Permission};
use sdk::Tier;

/// The most plugins one grant table may hold.
pub const PLUGINS_MAX: usize = 1_024;
/// The most permissions one plugin may be granted.
pub const PERMISSIONS_MAX: usize = 64;
/// The most capability stems one plugin may serve.
pub const SERVED_STEMS_MAX: usize = 16;

/// The kernel's grant table: per plugin id, its tier and the permissions it holds, and per
/// served capability stem, the one plugin that serves it. Built by the host side from what
/// each plugin asked for; a plugin never writes it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Grants {
    by_plugin: BTreeMap<&'static str, (Tier, BTreeSet<&'static str>)>,
    served: BTreeMap<&'static str, &'static str>,
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
    /// the permission table. Each grant's capability stems become the stems it serves.
    /// Output: the table, `grants_unknown_permission`, `grants_tier_too_high`, or a refusal
    /// of the shared and served-stem checks.
    pub fn from_table(
        grants: &[BundledGrant],
        permissions: &[Permission],
    ) -> Result<Self, &'static str> {
        if grants.len() > PLUGINS_MAX {
            return Err("grants_too_many");
        }
        let mut entries = Vec::with_capacity(grants.len());
        let mut served = Vec::new();
        for grant in grants {
            served.extend(grant.capabilities.iter().map(|stem| (*stem, grant.plugin)));
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
        Self::build(&entries, &served, permissions)
    }

    /// Builds the table with no permission table check, for tests only; the engine builds
    /// through [`Grants::from_table`]. Input: per plugin its id, tier and granted
    /// permissions. Output: as the shared checks.
    #[cfg(any(test, feature = "test-support"))]
    pub fn new(entries: &[(&'static str, Tier, &[&'static str])]) -> Result<Self, &'static str> {
        Self::build(entries, &[], &[])
    }

    /// Builds the table with served stems and no permission table check, for tests only.
    /// Inputs: per plugin its id, tier and granted permissions; the (stem, plugin) pairs
    /// served; the capabilities each stem holds. Output: as the shared checks.
    #[cfg(any(test, feature = "test-support"))]
    pub fn new_serving(
        entries: &[(&'static str, Tier, &[&'static str])],
        served: &[(&'static str, &'static str)],
        permissions: &[Permission],
    ) -> Result<Self, &'static str> {
        Self::build(entries, served, permissions)
    }

    /// The checks every table passes. Inputs: per plugin its id, tier and granted
    /// permissions; the (stem, plugin) pairs served; the capabilities each stem holds.
    /// Output: the table, or `grants_duplicate_plugin`, `grants_duplicate_permission`,
    /// `grants_kernel_tier` (a plugin is never a kernel module), `grants_too_many`, or a
    /// refusal of [`Grants::serve_stems`].
    fn build(
        entries: &[(&'static str, Tier, &[&'static str])],
        served: &[(&'static str, &'static str)],
        permissions: &[Permission],
    ) -> Result<Self, &'static str> {
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
        let served = Self::serve_stems(&by_plugin, served, permissions)?;
        Ok(Self { by_plugin, served })
    }

    /// Checks the served stems and maps each to its provider. A stem is served by one known
    /// plugin, holds at least one capability, none of tier kernel, and none below the
    /// provider's tier; a plugin serves at most [`SERVED_STEMS_MAX`] stems. Inputs: the
    /// plugins, the (stem, plugin) pairs and the capabilities. Output: the map, or
    /// `grants_too_many`, `grants_provider_unknown`, `grants_served_twice`,
    /// `grants_serves_unknown`, `grants_serves_kernel` or `grants_provider_tier`.
    fn serve_stems(
        by_plugin: &BTreeMap<&'static str, (Tier, BTreeSet<&'static str>)>,
        served: &[(&'static str, &'static str)],
        permissions: &[Permission],
    ) -> Result<BTreeMap<&'static str, &'static str>, &'static str> {
        if served.len() > PLUGINS_MAX * SERVED_STEMS_MAX {
            return Err("grants_too_many");
        }
        let mut map = BTreeMap::new();
        let mut per_plugin: BTreeMap<&'static str, usize> = BTreeMap::new();
        for (stem, plugin) in served {
            let Some((tier, _)) = by_plugin.get(plugin) else {
                return Err("grants_provider_unknown");
            };
            if map.insert(*stem, *plugin).is_some() {
                return Err("grants_served_twice");
            }
            let mut held = permissions
                .iter()
                .filter(|p| p.name.split_once('.').map(|(s, _)| s) == Some(*stem))
                .peekable();
            if held.peek().is_none() {
                return Err("grants_serves_unknown");
            }
            for capability in held {
                let capability_tier = sdk_tier(capability.tier);
                if capability_tier == Tier::Kernel {
                    return Err("grants_serves_kernel");
                }
                if *tier > capability_tier {
                    return Err("grants_provider_tier");
                }
            }
            let count = per_plugin.entry(*plugin).or_insert(0);
            *count += 1;
            if *count > SERVED_STEMS_MAX {
                return Err("grants_too_many");
            }
        }
        assert_eq!(map.len(), served.len());
        Ok(map)
    }

    /// The tier and permissions of one plugin. Input: the plugin id.
    /// Output: its tier and permissions, or `None` for a plugin the table does not hold.
    pub(crate) fn of(&self, plugin: &str) -> Option<&(Tier, BTreeSet<&'static str>)> {
        self.by_plugin.get(plugin)
    }

    /// The plugin that serves a capability stem. Input: the stem. Output: its provider's
    /// id, or `None` for a stem no plugin serves.
    pub(crate) fn provider_of(&self, stem: &str) -> Option<&'static str> {
        self.served.get(stem).copied()
    }
}
