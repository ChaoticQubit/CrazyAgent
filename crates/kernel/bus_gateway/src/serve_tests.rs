use std::cell::{Cell, RefCell};

use kernel_plugin_host::permission::{self, Permission};
use sdk::{Capability, Refusal, Tier};

use super::*;

/// The served capability every row calls: `echo.ping`, tier t1, needing `echo.call`.
const PING: Capability = Capability {
    name: "echo.ping",
    tier: Tier::T1,
    needs: "echo.call",
};

/// A second capability of the served stem, tier t2, so a t1 caller is below it.
const LOUD: Capability = Capability {
    name: "echo.loud",
    tier: Tier::T2,
    needs: "echo.call",
};

/// A t1 capability of a stem no plugin serves.
const LONE: Capability = Capability {
    name: "lone.ping",
    tier: Tier::T1,
    needs: "echo.call",
};

/// The permission table of the rows: the two `echo` capabilities and `lone.ping`.
const TABLE: [Permission; 3] = [
    Permission {
        name: "echo.ping",
        tier: permission::Tier::T1,
        needs: "echo.call",
        emits: &[],
    },
    Permission {
        name: "echo.loud",
        tier: permission::Tier::T2,
        needs: "echo.call",
        emits: &[],
    },
    Permission {
        name: "lone.ping",
        tier: permission::Tier::T1,
        needs: "echo.call",
        emits: &[],
    },
];

/// The refusal sink of the rows: keeps each refused plugin and capability.
#[derive(Default)]
struct Seen {
    refused: RefCell<Vec<(&'static str, &'static str)>>,
}

impl Refusals for Seen {
    fn refused(&self, plugin: &'static str, capability: &Capability, _: &Refusal) {
        self.refused.borrow_mut().push((plugin, capability.name));
    }
}

/// Switches with one plugin off and every other on.
struct OneOff(&'static str);

impl Switches for OneOff {
    fn is_on(&self, plugin: &str) -> bool {
        plugin != self.0
    }
}

/// The grant table of the rows:
/// - `caller-p`, T2, holds `echo.call`;
/// - `low-p`, T1, holds `echo.call`;
/// - `bare-p`, T2, holds nothing;
/// - `served-p`, T1, holds `echo.call` and serves the `echo` stem.
///
/// Output: the table.
fn grants() -> Grants {
    Grants::new_serving(
        &[
            ("caller-p", Tier::T2, &["echo.call"]),
            ("low-p", Tier::T1, &["echo.call"]),
            ("bare-p", Tier::T2, &[]),
            ("served-p", Tier::T1, &["echo.call"]),
        ],
        &[("echo", "served-p")],
        &TABLE,
    )
    .expect("the table builds")
}

/// A check that always passes. Input: the value. Output: nothing.
fn pass<T>(_: &T) -> Result<(), Refusal> {
    Ok(())
}

/// The `Failed` refusal of a gateway code. Input: the code. Output: the refusal.
fn failed(code: &str) -> Refusal {
    Refusal::Failed {
        code: code.to_owned(),
    }
}

/// Serves `PING` from a caller whose provider calls `PING` again on its own caller, until
/// the gateway refuses. Inputs: the caller and the run counter. Output: the result of the
/// outermost call.
fn recurse(caller: Caller<'_>, runs: &Cell<u8>) -> Result<(), Refusal> {
    caller.serve(
        &PING,
        (),
        pass,
        |provider, ()| {
            runs.set(runs.get() + 1);
            recurse(provider, runs)
        },
        pass,
    )
}

#[test]
fn a_served_call_hands_the_provider_a_caller_bound_to_the_provider() {
    let grants = grants();
    let seen = Seen::default();
    let gateway = Gateway::new(&grants, &seen, &AllOn);
    let caller = gateway.bind("caller-p");
    assert_eq!(
        caller.serve(&PING, (), pass, |provider, ()| Ok(provider.plugin()), pass),
        Ok("served-p")
    );
    assert_eq!(caller.plugin(), "caller-p");
    assert!(seen.refused.borrow().is_empty());
}

#[test]
fn a_caller_without_the_grant_is_refused_before_the_provider_runs() {
    let grants = grants();
    let seen = Seen::default();
    let gateway = Gateway::new(&grants, &seen, &AllOn);
    let ran = Cell::new(false);
    let result = gateway.bind("bare-p").serve(
        &PING,
        (),
        pass,
        |_, ()| {
            ran.set(true);
            Ok(())
        },
        pass,
    );
    assert_eq!(
        result,
        Err(Refusal::NotGranted {
            capability: "echo.ping",
            needs: "echo.call",
        })
    );
    assert!(!ran.get());
    assert_eq!(*seen.refused.borrow(), [("bare-p", "echo.ping")]);
}

#[test]
fn a_caller_below_the_tier_is_refused() {
    let grants = grants();
    let seen = Seen::default();
    let gateway = Gateway::new(&grants, &seen, &AllOn);
    let ran = Cell::new(false);
    let result = gateway.bind("low-p").serve(
        &LOUD,
        (),
        pass,
        |_, ()| {
            ran.set(true);
            Ok(())
        },
        pass,
    );
    assert_eq!(
        result,
        Err(Refusal::TierTooHigh {
            capability: "echo.loud",
        })
    );
    assert!(!ran.get());
    assert_eq!(*seen.refused.borrow(), [("low-p", "echo.loud")]);
}

#[test]
fn an_unserved_stem_fails_capability_unserved_and_sinks_nothing() {
    let grants = grants();
    let seen = Seen::default();
    let gateway = Gateway::new(&grants, &seen, &AllOn);
    let ran = Cell::new(false);
    let result = gateway.bind("caller-p").serve(
        &LONE,
        (),
        pass,
        |_, ()| {
            ran.set(true);
            Ok(())
        },
        pass,
    );
    assert_eq!(result, Err(failed("capability_unserved")));
    assert!(!ran.get());
    assert!(seen.refused.borrow().is_empty());
}

#[test]
fn a_switched_off_provider_fails_capability_provider_off_and_sinks_nothing() {
    let grants = grants();
    let seen = Seen::default();
    let off = OneOff("served-p");
    let gateway = Gateway::new(&grants, &seen, &off);
    let ran = Cell::new(false);
    let result = gateway.bind("caller-p").serve(
        &PING,
        (),
        pass,
        |_, ()| {
            ran.set(true);
            Ok(())
        },
        pass,
    );
    assert_eq!(result, Err(failed("capability_provider_off")));
    assert!(!ran.get());
    assert!(seen.refused.borrow().is_empty());
}

#[test]
fn a_call_at_the_hop_cap_fails_capability_call_too_deep() {
    let grants = grants();
    let seen = Seen::default();
    let gateway = Gateway::new(&grants, &seen, &AllOn);
    let runs = Cell::new(0);
    assert_eq!(
        recurse(gateway.bind("caller-p"), &runs),
        Err(failed("capability_call_too_deep"))
    );
    assert_eq!(runs.get(), HOPS_MAX);
    assert!(seen.refused.borrow().is_empty());
}

#[test]
fn a_provider_gateway_kind_refusal_becomes_capability_provider_refused() {
    let grants = grants();
    let seen = Seen::default();
    let gateway = Gateway::new(&grants, &seen, &AllOn);
    let caller = gateway.bind("caller-p");
    for returned in [
        Refusal::NotGranted {
            capability: "clock.now",
            needs: "clock.read",
        },
        Refusal::TierTooHigh {
            capability: "clock.now",
        },
        Refusal::PluginOff {
            capability: "clock.now",
        },
    ] {
        let result: Result<(), Refusal> =
            caller.serve(&PING, (), pass, |_, ()| Err(returned.clone()), pass);
        assert_eq!(
            result,
            Err(failed("capability_provider_refused")),
            "{returned:?}"
        );
    }
    assert!(seen.refused.borrow().is_empty());
}

#[test]
fn a_provider_rejected_or_failed_passes_through_unchanged() {
    let grants = grants();
    let seen = Seen::default();
    let gateway = Gateway::new(&grants, &seen, &AllOn);
    let caller = gateway.bind("caller-p");
    for returned in [
        Refusal::Rejected {
            code: "owner_page_limit".to_owned(),
        },
        failed("capability_unserved"),
    ] {
        let result: Result<(), Refusal> =
            caller.serve(&PING, (), pass, |_, ()| Err(returned.clone()), pass);
        assert_eq!(result, Err(returned));
    }
    assert!(seen.refused.borrow().is_empty());
}

#[test]
fn the_output_check_runs_on_the_provider_result() {
    let grants = grants();
    let seen = Seen::default();
    let gateway = Gateway::new(&grants, &seen, &AllOn);
    let checked = Cell::new(0_u32);
    let result = gateway.bind("caller-p").serve(
        &PING,
        (),
        pass,
        |_, ()| Ok(7_u32),
        |output: &u32| {
            checked.set(*output);
            Err(failed("capgen_output_bound"))
        },
    );
    assert_eq!(result, Err(failed("capgen_output_bound")));
    assert_eq!(checked.get(), 7);
    assert!(seen.refused.borrow().is_empty());
}

#[test]
fn the_input_check_runs_before_the_provider() {
    let grants = grants();
    let seen = Seen::default();
    let gateway = Gateway::new(&grants, &seen, &AllOn);
    let steps = RefCell::new(Vec::new());
    let caller = gateway.bind("caller-p");
    let refused = caller.serve(
        &PING,
        3_u32,
        |_: &u32| {
            steps.borrow_mut().push("check_input");
            Err(failed("capgen_input_bound"))
        },
        |_, input| {
            steps.borrow_mut().push("provide");
            Ok(input)
        },
        pass,
    );
    assert_eq!(refused, Err(failed("capgen_input_bound")));
    assert_eq!(*steps.borrow(), ["check_input"]);
    steps.borrow_mut().clear();
    let served = caller.serve(
        &PING,
        3_u32,
        |_: &u32| {
            steps.borrow_mut().push("check_input");
            Ok(())
        },
        |_, input| {
            steps.borrow_mut().push("provide");
            Ok(input)
        },
        |_: &u32| {
            steps.borrow_mut().push("check_output");
            Ok(())
        },
    );
    assert_eq!(served, Ok(3));
    assert_eq!(*steps.borrow(), ["check_input", "provide", "check_output"]);
}

#[test]
fn grants_served_twice() {
    assert_eq!(
        Grants::new_serving(
            &[("one-p", Tier::T1, &[]), ("two-p", Tier::T1, &[]),],
            &[("echo", "one-p"), ("echo", "two-p")],
            &TABLE,
        ),
        Err("grants_served_twice")
    );
}

#[test]
fn grants_serves_unknown() {
    assert_eq!(
        Grants::new_serving(&[("one-p", Tier::T1, &[])], &[("nope", "one-p")], &TABLE),
        Err("grants_serves_unknown")
    );
}

#[test]
fn grants_serves_kernel() {
    let table = [Permission {
        name: "clock.now",
        tier: permission::Tier::Kernel,
        needs: "clock.read",
        emits: &[],
    }];
    assert_eq!(
        Grants::new_serving(&[("one-p", Tier::T1, &[])], &[("clock", "one-p")], &table),
        Err("grants_serves_kernel")
    );
}

#[test]
fn grants_provider_tier() {
    assert_eq!(
        Grants::new_serving(&[("one-p", Tier::T2, &[])], &[("echo", "one-p")], &TABLE),
        Err("grants_provider_tier")
    );
    assert!(Grants::new_serving(&[("one-p", Tier::T1, &[])], &[("echo", "one-p")], &TABLE).is_ok());
}

#[test]
fn grants_provider_unknown() {
    assert_eq!(
        Grants::new_serving(&[("one-p", Tier::T1, &[])], &[("echo", "two-p")], &TABLE),
        Err("grants_provider_unknown")
    );
}

#[test]
fn grants_too_many() {
    let names: Vec<&'static str> = (0..=SERVED_STEMS_MAX)
        .map(|index| &*Box::leak(format!("s{index}.go").into_boxed_str()))
        .collect();
    let table: Vec<Permission> = names
        .iter()
        .copied()
        .map(|name| Permission {
            name,
            tier: permission::Tier::T1,
            needs: "s.go",
            emits: &[],
        })
        .collect();
    let served: Vec<(&'static str, &'static str)> = names
        .iter()
        .map(|name| {
            let (stem, _) = name.split_once('.').expect("dotted");
            (stem, "one-p")
        })
        .collect();
    assert_eq!(served.len(), SERVED_STEMS_MAX + 1);
    assert_eq!(
        Grants::new_serving(&[("one-p", Tier::T1, &[])], &served, &table),
        Err("grants_too_many")
    );
    assert!(
        Grants::new_serving(
            &[("one-p", Tier::T1, &[])],
            &served[..SERVED_STEMS_MAX],
            &table,
        )
        .is_ok()
    );
}

#[test]
#[should_panic(expected = "serve runs only plugin-tier capabilities")]
fn serve_on_a_kernel_capability_panics() {
    let kernel = Capability {
        name: "echo.ping",
        tier: Tier::Kernel,
        needs: "echo.call",
    };
    let grants = grants();
    let seen = Seen::default();
    let gateway = Gateway::new(&grants, &seen, &AllOn);
    let _ = gateway
        .bind("caller-p")
        .serve(&kernel, (), pass, |_, ()| Ok(()), pass);
}

#[test]
#[should_panic(expected = "a capability name is dotted")]
fn serve_on_an_undotted_capability_panics() {
    let undotted = Capability {
        name: "echoping",
        tier: Tier::T1,
        needs: "echo.call",
    };
    let grants = grants();
    let seen = Seen::default();
    let gateway = Gateway::new(&grants, &seen, &AllOn);
    let _ = gateway
        .bind("caller-p")
        .serve(&undotted, (), pass, |_, ()| Ok(()), pass);
}

#[test]
#[should_panic(expected = "a provider's tier is at or below the capability's")]
fn serve_with_a_capability_tier_below_the_provider_panics() {
    let table = [Permission {
        name: "echo.ping",
        tier: permission::Tier::T2,
        needs: "echo.call",
        emits: &[],
    }];
    let grants = Grants::new_serving(
        &[
            ("caller-p", Tier::T2, &["echo.call"]),
            ("served-p", Tier::T2, &[]),
        ],
        &[("echo", "served-p")],
        &table,
    )
    .expect("the table builds");
    let seen = Seen::default();
    let gateway = Gateway::new(&grants, &seen, &AllOn);
    let _ = gateway
        .bind("caller-p")
        .serve(&PING, (), pass, |_, ()| Ok(()), pass);
}
