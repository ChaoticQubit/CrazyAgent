use std::cell::RefCell;

use sdk::{Capability, Refusal, Tier};

use super::*;

/// A capability the caller rows call, granted to `low`.
const USE: Capability = Capability {
    name: "test.use",
    tier: Tier::T1,
    needs: "test.use",
};

/// The refusal sink of the caller rows: keeps each refused plugin and capability.
#[derive(Default)]
struct Seen {
    refused: RefCell<Vec<(&'static str, &'static str)>>,
}

impl Refusals for Seen {
    fn refused(&self, plugin: &'static str, capability: &Capability, _: &Refusal) {
        self.refused.borrow_mut().push((plugin, capability.name));
    }
}

/// Calls `USE` through a caller, recording each step it runs. Inputs: the caller and the
/// steps. Output: the call's result, the plugin id the provider was handed.
fn traced(caller: Caller<'_>, steps: &RefCell<Vec<&'static str>>) -> Result<&'static str, Refusal> {
    caller.call(
        &USE,
        caller.plugin(),
        |_: &&'static str| {
            steps.borrow_mut().push("check_input");
            Ok(())
        },
        |plugin: &'static str| {
            steps.borrow_mut().push("provide");
            Ok(plugin)
        },
        |_: &&'static str| {
            steps.borrow_mut().push("check_output");
            Ok(())
        },
    )
}

/// Whether a source names an item capgen generates into the SDK: a capability namespace
/// module or a provider trait. Input: the source text. Output: true when it names one.
fn names_generated_item(source: &str) -> bool {
    [
        "sdk::settings",
        "sdk::secrets",
        "settings::",
        "secrets::",
        "Provide",
    ]
    .iter()
    .any(|name| source.contains(name))
}

#[test]
fn a_caller_runs_admit_and_its_checks_in_order_as_its_bound_plugin() {
    let grants = Grants::new(&[("low", Tier::T1, &["test.use"]), ("reader", Tier::T1, &[])])
        .expect("the table builds");
    let seen = Seen::default();
    let gateway = Gateway::new(&grants, &seen);
    let steps = RefCell::new(Vec::new());
    let low = gateway.bind("low");
    assert_eq!(low.plugin(), "low");
    assert_eq!(traced(low, &steps), Ok("low"));
    assert_eq!(*steps.borrow(), ["check_input", "provide", "check_output"]);
    assert!(seen.refused.borrow().is_empty());
    steps.borrow_mut().clear();
    assert_eq!(
        traced(gateway.bind("reader"), &steps),
        Err(Refusal::NotGranted {
            capability: "test.use",
            needs: "test.use",
        })
    );
    assert!(steps.borrow().is_empty());
    assert_eq!(*seen.refused.borrow(), [("reader", "test.use")]);
}

#[test]
fn the_gateway_core_names_no_generated_item() {
    assert!(names_generated_item(
        "use sdk::settings::{self, QuitSettings};"
    ));
    assert!(names_generated_item("settings: &'p dyn settings::Provide,"));
    let lib = include_str!("lib.rs");
    let core = &lib[..lib
        .find("#[cfg(test)]")
        .expect("lib.rs holds its test modules")];
    for (path, source) in [
        ("lib.rs", core),
        ("gateway.rs", include_str!("gateway.rs")),
        ("grants.rs", include_str!("grants.rs")),
    ] {
        assert!(
            !names_generated_item(source),
            "{path} names a generated item"
        );
    }
}

#[test]
fn the_gateway_depends_on_the_sdk_and_not_its_host() {
    let manifest = include_str!("../Cargo.toml");
    let mut table = "";
    let mut sdk = Vec::new();
    for line in manifest.lines() {
        if line.starts_with('[') {
            table = line;
        }
        if line.starts_with("crazyagent-sdk") {
            sdk.push((table, line.split_whitespace().next().unwrap_or_default()));
        }
    }
    assert_eq!(sdk, [("[dependencies]", "crazyagent-sdk")]);
}
