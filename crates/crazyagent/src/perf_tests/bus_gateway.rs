use crate::perf_kit::{Row, SAMPLES, time};
use std::hint::black_box;

const BATCH: usize = 1000;
const INPUT: u64 = 41;

/// Times the 2.4.4 path of a bus gateway call: the engine called the provider directly,
/// with no grant table, gateway or caller binding, so only `gateway.Caller.call` has a
/// row, timing the same provider the 2.5 row hands the gateway. Checks the answer once
/// first. Prints one PERF line.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn bus_gateway_calls() {
    let provide = |input: u64| -> Result<u64, &'static str> { Ok(input + 1) };
    assert_eq!(provide(INPUT), Ok(INPUT + 1));
    time(
        &Row {
            layer: 1,
            module: "bus_gateway",
            name: "gateway.Caller.call",
            batch: BATCH,
        },
        SAMPLES,
        || {
            let _ = black_box(black_box(provide)(black_box(INPUT)));
        },
    );
}
