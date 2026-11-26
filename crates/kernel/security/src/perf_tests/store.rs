use crate::perf_kit::{Row, SAMPLES, time};
use crate::store::{checked_name, reference};
use std::hint::black_box;

const BATCH: usize = 1000;
const SERVICE: &str = "dev.crazyagent.plugins";
const ACCOUNT: &str = "plugin-key-openai";

fn row(name: &'static str) -> Row {
    Row {
        layer: 1,
        module: "security",
        name,
        batch: BATCH,
    }
}

/// Times the two store name calls that reach kernel_common from this crate: one name check
/// alone and the whole reference a caller builds, after checking each answer once.
/// Prints one PERF line per call and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn store_name_calls() {
    assert_eq!(checked_name(SERVICE).ok(), Some(SERVICE));
    assert_eq!(
        reference(SERVICE, ACCOUNT)
            .map(|found| found.as_str().to_owned())
            .ok(),
        Some(format!("{SERVICE}:{ACCOUNT}"))
    );
    time(&row("store.checked_name"), SAMPLES, || {
        let _ = black_box(checked_name(black_box(SERVICE)));
    });
    time(&row("store.reference"), SAMPLES, || {
        let _ = black_box(reference(black_box(SERVICE), black_box(ACCOUNT)));
    });
}
