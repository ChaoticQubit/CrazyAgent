use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::Refused;
use crate::generated::permissions::{BUNDLED_GRANTS, PERMISSIONS};
use crate::permission::{BundledGrant, Tier};
use crate::registry::{self, Engine, PLUGINS_MAX};
use crate::views::Origin;

/// The most entries the owner's grant listing holds: every app plugin plus every bundled
/// code plugin.
pub const GRANTS_MAX: usize = PLUGINS_MAX + BUNDLED_GRANTS.len();

/// The tier an app (data-only) plugin runs at; the manifest check refuses any other.
const APP_PLUGIN_TIER: Tier = Tier::T2;

/// Whether a plugin is on, as the owner's grant listing shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantState {
    On,
    Off,
}

/// One capability a plugin's permissions grant: its dotted name and the lowest tier that
/// may call it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantedCapability {
    pub name: String,
    pub tier: String,
}

/// One plugin in the owner's grant listing: its id, tier, where it is from, whether it is
/// on, whether it is an essential bundled code plugin whose switch stays locked, and the
/// capabilities it is granted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginGrants {
    pub plugin: String,
    pub tier: String,
    pub origin: Origin,
    pub state: GrantState,
    pub essential: bool,
    pub capabilities: Vec<GrantedCapability>,
}

/// The word a tier reads as in the listing. Input: the tier. Output: `kernel`, `t1`, `t2`
/// or `t3`.
fn tier_word(tier: Tier) -> &'static str {
    match tier {
        Tier::Kernel => "kernel",
        Tier::T1 => "t1",
        Tier::T2 => "t2",
        Tier::T3 => "t3",
    }
}

/// The capabilities a bundled code plugin's permissions grant, by capability name. Input:
/// the grant. Output: each capability whose needed permission the plugin holds.
fn granted(grant: &BundledGrant) -> Vec<GrantedCapability> {
    PERMISSIONS
        .iter()
        .filter(|permission| grant.permissions.contains(&permission.needs))
        .map(|permission| GrantedCapability {
            name: permission.name.to_owned(),
            tier: tier_word(permission.tier).to_owned(),
        })
        .collect()
}

/// Lists every plugin with the capabilities it is granted, sorted by plugin id. Bundled code
/// plugins read as on unless turned off, carry their essential mark and hold what their
/// plugin.toml grants; app plugins hold none, are never essential and read as on while
/// turned on. Inputs: database and engine facts. Output: at most
/// [`GRANTS_MAX`] entries, or the registry's refusal.
pub fn list(db: &Connection, engine: &Engine<'_>) -> Result<Vec<PluginGrants>, Refused> {
    let off = registry::off_code_plugins(db)?;
    let mut listing: Vec<PluginGrants> = BUNDLED_GRANTS
        .iter()
        .map(|grant| PluginGrants {
            plugin: grant.plugin.to_owned(),
            tier: tier_word(grant.tier).to_owned(),
            origin: Origin::Bundled,
            state: if off.contains(grant.plugin) {
                GrantState::Off
            } else {
                GrantState::On
            },
            essential: grant.essential,
            capabilities: granted(grant),
        })
        .collect();
    for plugin in registry::list(db, engine)?.plugins {
        listing.push(PluginGrants {
            plugin: plugin.name,
            tier: tier_word(APP_PLUGIN_TIER).to_owned(),
            origin: plugin.origin,
            state: if plugin.enabled {
                GrantState::On
            } else {
                GrantState::Off
            },
            essential: false,
            capabilities: Vec::new(),
        });
    }
    listing.sort_by(|left, right| left.plugin.cmp(&right.plugin));
    assert!(listing.len() <= GRANTS_MAX);
    Ok(listing)
}
