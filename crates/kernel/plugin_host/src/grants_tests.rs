use crate::grants::{GrantState, GrantedCapability, PluginGrants, list};
use crate::registry::{Switch, set_enabled};
use crate::registry_tests::{NEAR, case, off, trusted};
use crate::views::Origin;

fn entry<'a>(listing: &'a [PluginGrants], plugin: &str, origin: Origin) -> &'a PluginGrants {
    listing
        .iter()
        .find(|grants| grants.plugin == plugin && grants.origin == origin)
        .unwrap_or_else(|| panic!("no {plugin}"))
}

/// The owner's listing names every plugin, sorted by id, with its tier, where it is from,
/// whether it is on, and each capability its permissions grant with that capability's tier.
#[test]
fn the_listing_shows_each_plugins_granted_capabilities_with_their_tier() {
    let case = case();
    trusted(&case, NEAR);
    let listing = list(&case.db, &case.engine()).unwrap();
    let ids: Vec<&str> = listing
        .iter()
        .map(|grants| grants.plugin.as_str())
        .collect();
    assert_eq!(ids, ["agent-quit", "near", "ollama", "openai", "voyage"]);
    assert_eq!(
        entry(&listing, "agent-quit", Origin::Bundled),
        &PluginGrants {
            plugin: "agent-quit".to_owned(),
            tier: "t2".to_owned(),
            origin: Origin::Bundled,
            state: GrantState::On,
            essential: true,
            capabilities: vec![GrantedCapability {
                name: "settings.save_quit".to_owned(),
                tier: "kernel".to_owned(),
            }],
        }
    );
    let near = entry(&listing, "near", Origin::User);
    assert_eq!(
        (
            near.tier.as_str(),
            near.state,
            near.essential,
            near.capabilities.len()
        ),
        ("t2", GrantState::On, false, 0)
    );
    for bundled in ["ollama", "openai", "voyage"] {
        let grants = entry(&listing, bundled, Origin::Bundled);
        assert_eq!(
            (grants.state, grants.capabilities.len()),
            (GrantState::Off, 0)
        );
    }
    let json = serde_json::to_value(&listing).unwrap();
    assert_eq!(json[0]["state"], "on");
    assert_eq!(json[0]["capabilities"][0]["tier"], "kernel");
}

/// A plugin the owner turned off lists as off, and as on again once turned back on; its
/// listed capabilities do not change.
#[test]
fn a_turned_off_plugin_lists_as_off() {
    let case = case();
    trusted(&case, NEAR);
    let state = || {
        let listing = list(&case.db, &case.engine()).unwrap();
        entry(&listing, "near", Origin::User).state
    };
    assert_eq!(state(), GrantState::On);
    set_enabled(&case.db, &case.engine(), "near", off()).unwrap();
    assert_eq!(state(), GrantState::Off);
    set_enabled(&case.db, &case.engine(), "near", Switch::On).unwrap();
    assert_eq!(state(), GrantState::On);
}

/// An essential bundled code plugin turned off with the lock lifted lists as off, and
/// stays marked essential, so the owner's screen still shows its lock.
#[test]
fn an_unlocked_essential_plugin_turned_off_lists_as_off() {
    let case = case();
    let usage = crate::usage::Usage::default();
    let switch = Switch::Off {
        usage: &usage,
        when_off: crate::usage::TurnOff::RefuseNewCalls,
        essential_unlocked: true,
    };
    set_enabled(&case.db, &case.engine(), "agent-quit", switch).unwrap();
    let listing = list(&case.db, &case.engine()).unwrap();
    let quit = entry(&listing, "agent-quit", Origin::Bundled);
    assert_eq!((quit.state, quit.essential), (GrantState::Off, true));
    set_enabled(&case.db, &case.engine(), "agent-quit", Switch::On).unwrap();
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(
        entry(&listing, "agent-quit", Origin::Bundled).state,
        GrantState::On
    );
}
