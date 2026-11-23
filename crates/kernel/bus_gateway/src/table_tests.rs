use kernel_plugin_host::generated::permissions::{BUNDLED_GRANTS, PERMISSIONS};
use kernel_plugin_host::permission::{self, BundledGrant, Permission};

use super::*;
use crate::grants::sdk_tier;

/// A one-capability permission table: `x.use` needs `x.use` at the given tier and emits
/// nothing. Input: the tier. Output: the table.
fn table(tier: permission::Tier) -> [Permission; 1] {
    [Permission {
        name: "x.use",
        tier,
        needs: "x.use",
        emits: &[],
    }]
}

/// One bundled grant of the given permissions to plugin `p`. Inputs: the plugin's tier and
/// its permissions. Output: the grant.
fn grant(tier: permission::Tier, permissions: &'static [&'static str]) -> [BundledGrant; 1] {
    [BundledGrant {
        plugin: "p",
        tier,
        permissions,
    }]
}

#[test]
fn every_plugin_host_tier_maps_to_the_sdk_tier_in_order() {
    let tiers = [
        permission::Tier::Kernel,
        permission::Tier::T1,
        permission::Tier::T2,
        permission::Tier::T3,
    ];
    let mapped: Vec<sdk::Tier> = tiers.iter().map(|tier| sdk_tier(*tier)).collect();
    assert_eq!(
        mapped,
        [
            sdk::Tier::Kernel,
            sdk::Tier::T1,
            sdk::Tier::T2,
            sdk::Tier::T3
        ]
    );
    for pair in tiers.windows(2) {
        assert!(pair[0] < pair[1]);
        assert!(sdk_tier(pair[0]) < sdk_tier(pair[1]));
    }
}

#[test]
fn a_grant_outside_the_table_is_refused() {
    let table = table(permission::Tier::Kernel);
    assert_eq!(
        Grants::from_table(&grant(permission::Tier::T2, &["y.use"]), &table),
        Err("grants_unknown_permission")
    );
    assert!(Grants::from_table(&grant(permission::Tier::T2, &["x.use"]), &table).is_ok());
}

#[test]
fn a_capability_tier_comes_from_the_table() {
    let wanted = grant(permission::Tier::T2, &["x.use"]);
    assert!(Grants::from_table(&wanted, &table(permission::Tier::T2)).is_ok());
    assert_eq!(
        Grants::from_table(&wanted, &table(permission::Tier::T3)),
        Err("grants_tier_too_high")
    );
    let shared = [
        Permission {
            name: "x.low",
            tier: permission::Tier::T1,
            needs: "x.use",
            emits: &[],
        },
        Permission {
            name: "x.high",
            tier: permission::Tier::T3,
            needs: "x.use",
            emits: &[],
        },
    ];
    assert_eq!(
        Grants::from_table(&wanted, &shared),
        Err("grants_tier_too_high")
    );
}

#[test]
fn from_table_refuses_an_unknown_permission_and_a_tier_mismatch() {
    let unknown = Grants::from_table(
        &grant(permission::Tier::T1, &["x.other"]),
        &table(permission::Tier::Kernel),
    );
    assert_eq!(unknown, Err("grants_unknown_permission"));
    let mismatch = Grants::from_table(
        &grant(permission::Tier::T1, &["x.use"]),
        &table(permission::Tier::T2),
    );
    assert_eq!(mismatch, Err("grants_tier_too_high"));
    let kernel = Grants::from_table(
        &grant(permission::Tier::Kernel, &[]),
        &table(permission::Tier::Kernel),
    );
    assert_eq!(kernel, Err("grants_kernel_tier"));
    let built = Grants::from_table(
        &grant(permission::Tier::T2, &["x.use"]),
        &table(permission::Tier::T1),
    )
    .unwrap();
    assert_eq!(built.of("p").map(|(tier, _)| *tier), Some(sdk::Tier::T2));
    assert!(built.of("p").unwrap().1.contains("x.use"));
}

#[test]
fn the_bundled_tables_build_a_grant_table() {
    let grants = Grants::from_table(&BUNDLED_GRANTS, &PERMISSIONS).unwrap();
    for bundled in &BUNDLED_GRANTS {
        let (tier, held) = grants.of(bundled.plugin).unwrap();
        assert_eq!(*tier, sdk_tier(bundled.tier));
        assert_eq!(held.len(), bundled.permissions.len());
    }
}

#[test]
fn the_unchecked_constructor_is_test_only() {
    for manifest in [
        include_str!("../../../crazyagent/Cargo.toml"),
        include_str!("../../../sdk/host/Cargo.toml"),
    ] {
        let mut table = "";
        let mut normal = Vec::new();
        for line in manifest.lines() {
            if line.starts_with('[') {
                table = line;
            }
            if line.starts_with("crazyagent-bus-gateway") {
                normal.push((table, line.contains("test-support")));
            }
        }
        assert_eq!(
            normal,
            [("[dependencies]", false), ("[dev-dependencies]", true)]
        );
    }
    let source = include_str!("grants.rs");
    let gate = "#[cfg(any(test, feature = \"test-support\"))]\n    pub fn new(";
    assert_eq!(source.matches("pub fn new(").count(), 1);
    assert!(source.contains(gate), "Grants::new carries the test gate");
    let own = include_str!("../Cargo.toml");
    assert!(own.contains("[features]\ntest-support = []\n"));
}
