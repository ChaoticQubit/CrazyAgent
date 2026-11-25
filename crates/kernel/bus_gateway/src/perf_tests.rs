use kernel_plugin_host::generated::permissions::{BUNDLED_GRANTS, PERMISSIONS};
use sdk::{Capability, Refusal};
use std::hint::black_box;

use crate::perf_kit::{Row, SAMPLES, time};
use crate::{AllOn, Gateway, Grants, Refusals};

const BATCH: usize = 1000;
const TABLE_BATCH: usize = 100;
const PLUGIN: &str = "agent-quit";
const INPUT: u64 = 41;

/// The refusal sink of the timing rows; an admitted call never reaches it.
struct Unused;

impl Refusals for Unused {
    fn refused(&self, _: &'static str, _: &Capability, _: &Refusal) {
        unreachable!("an admitted call is never refused");
    }
}

fn row(name: &'static str, batch: usize) -> Row {
    Row {
        layer: 1,
        module: "bus_gateway",
        name,
        batch,
    }
}

/// Times every public call of the bus gateway on the generated grant table: building the
/// table, the gateway and a bound caller, and one admitted call of the bundled quit plugin
/// to `settings.save_quit` with a provider that adds one, the same provider the 2.4.4 row
/// calls directly. Checks each answer once first. Prints one PERF line per call.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn bus_gateway_calls() {
    let grants = Grants::from_table(&BUNDLED_GRANTS, &PERMISSIONS).expect("generated table");
    let gateway = Gateway::new(&grants, &Unused, &AllOn);
    let caller = gateway.bind(PLUGIN);
    let capability = sdk::settings::SAVE_QUIT;
    let call = |input: u64| {
        caller.call(
            &capability,
            input,
            |_: &u64| Ok(()),
            |input: u64| Ok(input + 1),
            |_: &u64| Ok(()),
        )
    };
    assert_eq!(call(INPUT), Ok(INPUT + 1));
    assert_eq!(caller.plugin(), PLUGIN);
    time(
        &row("grants.Grants.from_table", TABLE_BATCH),
        SAMPLES,
        || {
            let _ = black_box(Grants::from_table(
                black_box(&BUNDLED_GRANTS),
                black_box(&PERMISSIONS),
            ));
        },
    );
    time(&row("gateway.Gateway.new", BATCH), SAMPLES, || {
        black_box(Gateway::new(black_box(&grants), &Unused, &AllOn));
    });
    time(&row("gateway.Gateway.bind", BATCH), SAMPLES, || {
        black_box(black_box(&gateway).bind(black_box(PLUGIN)));
    });
    time(&row("gateway.Caller.plugin", BATCH), SAMPLES, || {
        black_box(black_box(&caller).plugin());
    });
    time(&row("gateway.Caller.call", BATCH), SAMPLES, || {
        let _ = black_box(call(black_box(INPUT)));
    });
}
