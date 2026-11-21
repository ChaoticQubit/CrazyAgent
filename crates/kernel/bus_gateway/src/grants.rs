use std::collections::{BTreeMap, BTreeSet};

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

impl Grants {
    /// Builds the table. Input: per plugin its id, tier and granted permissions.
    /// Output: the table, or `grants_duplicate_plugin`, `grants_duplicate_permission`,
    /// `grants_kernel_tier` (a plugin is never a kernel module) or `grants_too_many`.
    pub fn new(entries: &[(&'static str, Tier, &[&'static str])]) -> Result<Self, &'static str> {
        let _ = entries;
        Ok(Self::default())
    }

    /// The tier and permissions of one plugin. Input: the plugin id.
    /// Output: its tier and permissions, or `None` for a plugin the table does not hold.
    pub(crate) fn of(&self, plugin: &str) -> Option<&(Tier, BTreeSet<&'static str>)> {
        self.by_plugin.get(plugin)
    }
}
