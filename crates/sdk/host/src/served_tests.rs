use std::cell::{Cell, RefCell};

use bus_gateway::{Gateway, Grants, HOPS_MAX, Refusals, Switches};
use kernel_plugin_host::permission::{self, Permission};
use sdk::fixture_served::host::Host;
use sdk::fixture_served::{clock, echo, tick};
use sdk::{Capability, Refusal, Tier};

use crate::fixture_served::{Bound, Providers, Served};

/// The plugin every row calls `echo.ping` from.
pub(super) const CALLER: &str = "app-plugin";

/// The plugin that serves the `echo` stem.
pub(super) const ECHO: &str = "echo-plugin";

/// The plugin that serves the `tick` stem.
const TICK: &str = "tick-plugin";

/// The permission table of `capgen/fixture_served`: kernel `clock.now` and the served
/// `echo.ping` and `tick.count`.
const TABLE: [Permission; 3] = [
    Permission {
        name: "clock.now",
        tier: permission::Tier::Kernel,
        needs: "clock.read",
        emits: &[],
    },
    Permission {
        name: "echo.ping",
        tier: permission::Tier::T1,
        needs: "echo.call",
        emits: &[],
    },
    Permission {
        name: "tick.count",
        tier: permission::Tier::T1,
        needs: "tick.read",
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

/// Switches with one plugin off and every other on; an empty id leaves all on.
struct OneOff(&'static str);

impl Switches for OneOff {
    fn is_on(&self, plugin: &str) -> bool {
        plugin != self.0
    }
}

/// The lower call the echo plugin makes from its own host.
#[derive(Clone, Copy)]
enum Lower {
    Clock,
    Echo,
    Tick,
}

/// The echo plugin's served code: counts its runs and makes one lower call.
struct EchoPlugin {
    lower: Lower,
    runs: Cell<u8>,
}

impl EchoPlugin {
    /// An echo plugin that has not run. Input: its lower call. Output: the plugin.
    fn new(lower: Lower) -> Self {
        Self {
            lower,
            runs: Cell::new(0),
        }
    }
}

impl echo::Serve for EchoPlugin {
    fn ping(&self, host: &dyn Host, n: u32) -> Result<u32, Refusal> {
        self.runs.set(self.runs.get() + 1);
        match self.lower {
            Lower::Clock => clock::now(host).map(|_| n),
            Lower::Echo => echo::ping(host, n),
            Lower::Tick => tick::count(host),
        }
    }
}

/// The tick plugin's served code: reads the clock from its own host and answers 7.
struct TickPlugin;

impl tick::Serve for TickPlugin {
    fn count(&self, host: &dyn Host) -> Result<u32, Refusal> {
        clock::now(host).map(|_| 7)
    }
}

/// The kernel clock: keeps each caller id the gateway hands it and answers 42.
#[derive(Default)]
struct ClockProvider {
    callers: RefCell<Vec<&'static str>>,
}

impl clock::Provide for ClockProvider {
    fn now(&self, caller: &'static str) -> Result<u64, Refusal> {
        self.callers.borrow_mut().push(caller);
        Ok(42)
    }
}

/// The grant table of the rows: `app-plugin` T2 and `echo-plugin` T1 with the given
/// permissions, `tick-plugin` T1 holding `clock.read`; `echo-plugin` serves `echo` and
/// `tick-plugin` serves `tick`. Inputs: the caller's and the echo plugin's permissions.
/// Output: the table.
pub(super) fn grants(caller: &'static [&'static str], provider: &'static [&'static str]) -> Grants {
    Grants::new_serving(
        &[
            (CALLER, Tier::T2, caller),
            (ECHO, Tier::T1, provider),
            (TICK, Tier::T1, &["clock.read"]),
        ],
        &[("echo", ECHO), ("tick", TICK)],
        &TABLE,
    )
    .expect("the table builds")
}

/// The `Failed` refusal of a gateway code. Input: the code. Output: the refusal.
fn failed(code: &str) -> Refusal {
    Refusal::Failed {
        code: code.to_owned(),
    }
}

/// Calls `echo.ping(5)` as `app-plugin` through a host serving the echo and tick plugins.
/// Inputs: the grants, the switches, the echo plugin, the clock and the sink. Output: the
/// call's result.
fn ping(
    grants: &Grants,
    switches: &dyn Switches,
    plugin: &EchoPlugin,
    clock: &ClockProvider,
    seen: &Seen,
) -> Result<u32, Refusal> {
    let gateway = Gateway::new(grants, seen, switches);
    let providers = Providers { clock };
    let served = Served {
        echo: plugin,
        tick: &TickPlugin,
    };
    let host = Bound::serving(gateway.bind(CALLER), &providers, &served);
    echo::ping(&host, 5)
}

#[test]
fn the_provider_calls_lower_tiers_under_its_own_id() {
    let (plugin, clock, seen) = (
        EchoPlugin::new(Lower::Clock),
        ClockProvider::default(),
        Seen::default(),
    );
    let grants = grants(&["echo.call"], &["clock.read"]);
    assert_eq!(ping(&grants, &OneOff(""), &plugin, &clock, &seen), Ok(5));
    assert_eq!(plugin.runs.get(), 1);
    assert_eq!(*clock.callers.borrow(), [ECHO]);
    assert!(seen.refused.borrow().is_empty());
}

#[test]
fn the_caller_needs_echo_call() {
    let (plugin, clock, seen) = (
        EchoPlugin::new(Lower::Clock),
        ClockProvider::default(),
        Seen::default(),
    );
    let grants = grants(&[], &["clock.read"]);
    assert_eq!(
        ping(&grants, &OneOff(""), &plugin, &clock, &seen),
        Err(Refusal::NotGranted {
            capability: "echo.ping",
            needs: "echo.call",
        })
    );
    assert_eq!(plugin.runs.get(), 0);
    assert!(clock.callers.borrow().is_empty());
    assert_eq!(*seen.refused.borrow(), [(CALLER, "echo.ping")]);
}

#[test]
fn the_provider_needs_clock_read() {
    let (plugin, clock, seen) = (
        EchoPlugin::new(Lower::Clock),
        ClockProvider::default(),
        Seen::default(),
    );
    let grants = grants(&["echo.call"], &[]);
    assert_eq!(
        ping(&grants, &OneOff(""), &plugin, &clock, &seen),
        Err(failed("capability_provider_refused"))
    );
    assert_eq!(plugin.runs.get(), 1);
    assert!(clock.callers.borrow().is_empty());
    assert_eq!(*seen.refused.borrow(), [(ECHO, "clock.now")]);
}

/// The provider's inner call is judged on the provider's grants alone: a caller holding
/// `clock.read` does not lend it to an echo plugin that lacks it, while the caller's own
/// `clock.now` still runs under the caller's id.
#[test]
fn a_served_provider_reads_through_a_host_bound_to_its_own_id() {
    let (plugin, clock, seen) = (
        EchoPlugin::new(Lower::Clock),
        ClockProvider::default(),
        Seen::default(),
    );
    let grants = grants(&["echo.call", "clock.read"], &[]);
    assert_eq!(
        ping(&grants, &OneOff(""), &plugin, &clock, &seen),
        Err(failed("capability_provider_refused"))
    );
    assert!(clock.callers.borrow().is_empty());
    assert_eq!(*seen.refused.borrow(), [(ECHO, "clock.now")]);
    let gateway = Gateway::new(&grants, &seen, &OneOff(""));
    let providers = Providers { clock: &clock };
    let served = Served {
        echo: &plugin,
        tick: &TickPlugin,
    };
    let host = Bound::serving(gateway.bind(CALLER), &providers, &served);
    assert_eq!(clock::now(&host), Ok(42));
    assert_eq!(*clock.callers.borrow(), [CALLER]);
}

#[test]
fn a_host_built_with_bound_new_answers_capability_unserved() {
    let (clock, seen) = (ClockProvider::default(), Seen::default());
    let grants = grants(&["echo.call", "tick.read"], &["clock.read"]);
    let gateway = Gateway::new(&grants, &seen, &OneOff(""));
    let providers = Providers { clock: &clock };
    let host = Bound::new(gateway.bind(CALLER), &providers);
    assert_eq!(echo::ping(&host, 5), Err(failed("capability_unserved")));
    assert_eq!(tick::count(&host), Err(failed("capability_unserved")));
    assert!(clock.callers.borrow().is_empty());
    assert!(seen.refused.borrow().is_empty());
}

#[test]
fn a_switched_off_echo_plugin_answers_capability_provider_off() {
    let (plugin, clock, seen) = (
        EchoPlugin::new(Lower::Clock),
        ClockProvider::default(),
        Seen::default(),
    );
    let grants = grants(&["echo.call"], &["clock.read"]);
    assert_eq!(
        ping(&grants, &OneOff(ECHO), &plugin, &clock, &seen),
        Err(failed("capability_provider_off"))
    );
    assert_eq!(plugin.runs.get(), 0);
    assert!(seen.refused.borrow().is_empty());
}

#[test]
fn an_echo_plugin_calling_echo_ping_stops_at_the_hop_cap() {
    let (plugin, clock, seen) = (
        EchoPlugin::new(Lower::Echo),
        ClockProvider::default(),
        Seen::default(),
    );
    let grants = grants(&["echo.call"], &["echo.call"]);
    assert_eq!(
        ping(&grants, &OneOff(""), &plugin, &clock, &seen),
        Err(failed("capability_call_too_deep"))
    );
    assert_eq!(plugin.runs.get(), HOPS_MAX);
    assert!(seen.refused.borrow().is_empty());
}

/// `Serve::ping` takes the host and the input, never a caller id: the shape is checked by
/// the compiler through a function pointer of exactly that type.
#[test]
fn the_provider_sees_no_caller_id() {
    let shape: fn(&EchoPlugin, &dyn Host, u32) -> Result<u32, Refusal> =
        <EchoPlugin as echo::Serve>::ping;
    let (clock, seen) = (ClockProvider::default(), Seen::default());
    let grants = grants(&[], &["clock.read"]);
    let gateway = Gateway::new(&grants, &seen, &OneOff(""));
    let providers = Providers { clock: &clock };
    let host = Bound::new(gateway.bind(ECHO), &providers);
    assert_eq!(shape(&EchoPlugin::new(Lower::Clock), &host, 3), Ok(3));
    assert_eq!(*clock.callers.borrow(), [ECHO]);
}

#[test]
fn a_served_provider_calls_another_served_stem() {
    let (plugin, clock, seen) = (
        EchoPlugin::new(Lower::Tick),
        ClockProvider::default(),
        Seen::default(),
    );
    let grants = grants(&["echo.call"], &["tick.read"]);
    assert_eq!(ping(&grants, &OneOff(""), &plugin, &clock, &seen), Ok(7));
    assert_eq!(plugin.runs.get(), 1);
    assert_eq!(*clock.callers.borrow(), [TICK]);
    assert!(seen.refused.borrow().is_empty());
}
