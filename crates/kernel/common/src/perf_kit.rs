use std::time::Instant;

/// The modules a PERF row may name: the kernel modules, the SDK, the whole kernel for
/// layer 2 and the app for layer 3.
pub const MODULES: [&str; 10] = [
    "common",
    "storage",
    "processes",
    "security",
    "sandbox",
    "plugin_host",
    "bus_gateway",
    "sdk",
    "kernel",
    "app",
];

/// The fewest timed samples a row may take, so its p95 is not its slowest sample.
pub const SAMPLES_MIN: usize = 21;

/// The samples a row takes unless it says otherwise.
pub const SAMPLES: usize = 200;

/// The most calls one sample may batch.
pub const BATCH_MAX: usize = 1_000_000;

/// One timed call: its layer, module, dotted name and the calls each sample batches,
/// fixed in the row's source so both builds time the same batch.
pub struct Row {
    pub layer: u8,
    pub module: &'static str,
    pub name: &'static str,
    pub batch: usize,
}

/// The nearest-rank percentile of ascending samples. Inputs: the samples, at least one,
/// and the percentile, 1 to 100. Output: the sample at that rank.
pub fn rank(sorted: &[u64], percentile: usize) -> u64 {
    assert!(!sorted.is_empty());
    assert!((1..=100).contains(&percentile));
    assert!(sorted.windows(2).all(|pair| pair[0] <= pair[1]));
    let index = (sorted.len() * percentile).div_ceil(100) - 1;
    sorted[index]
}

/// Checks one row's shape and reads its samples. Inputs: the row and its per-call times in
/// ns, ascending. Output: the line head, the sample count, p50 and p95; a row out of shape
/// panics.
fn shaped(row: &Row, sorted: &[u64]) -> (String, usize, u64, u64) {
    assert!((1..=3).contains(&row.layer), "perf row layer {}", row.layer);
    assert!(
        MODULES.contains(&row.module),
        "perf row module {}",
        row.module
    );
    assert!(
        !row.name.is_empty()
            && row
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._".contains(&byte)),
        "perf row name {:?}",
        row.name
    );
    assert!((1..=BATCH_MAX).contains(&row.batch));
    assert!(sorted.len() >= SAMPLES_MIN);
    let head = format!("PERF {} {} {}", row.layer, row.module, row.name);
    (head, sorted.len(), rank(sorted, 50), rank(sorted, 95))
}

/// The PERF line of one row from its per-call times in ns, ascending: nanoseconds with the
/// batch at any median, so no row floors to whole microseconds. Inputs: the row and the
/// samples. Output: the line; a row out of shape panics.
pub fn line(row: &Row, sorted: &[u64]) -> String {
    line_ns(row, sorted)
}

/// Checks that a median of 25431 ns, above the old 10 us switch to whole microseconds,
/// prints as 25431 ns with the batch. Output: none; a floored line fails the row.
#[test]
fn a_median_of_25431_ns_prints_25431_ns() {
    let row = Row {
        layer: 1,
        module: "common",
        name: "hash",
        batch: 1,
    };
    assert_eq!(
        line(&row, &[25_431_u64; SAMPLES_MIN]),
        format!("PERF 1 common hash p50_ns=25431 p95_ns=25431 n={SAMPLES_MIN} batch=1")
    );
}

/// The PERF line of one row in nanoseconds with the batch at any median, for a row whose
/// two builds fall on either side of 10 us, so both builds print the pair in one unit.
/// Inputs: the row and its per-call times in ns, ascending. Output: the line; a row out of
/// shape panics.
pub fn line_ns(row: &Row, sorted: &[u64]) -> String {
    let (head, n, p50, p95) = shaped(row, sorted);
    format!("{head} p50_ns={p50} p95_ns={p95} n={n} batch={}", row.batch)
}

/// Times one row: a warm-up of a tenth of the samples, then each sample runs the step
/// `batch` times and keeps the time per call. Prints the PERF line. Inputs: the row, the
/// samples and the step. Output: the printed line.
pub fn time(row: &Row, samples: usize, mut step: impl FnMut()) -> String {
    assert!(samples >= SAMPLES_MIN);
    assert!((1..=BATCH_MAX).contains(&row.batch));
    let batch = u64::try_from(row.batch).expect("batch fits u64");
    for _ in 0..(samples / 10) * row.batch {
        step();
    }
    let mut per_call: Vec<u64> = (0..samples)
        .map(|_| {
            let started = Instant::now();
            for _ in 0..row.batch {
                step();
            }
            u64::try_from(started.elapsed().as_nanos()).expect("sample fits u64") / batch
        })
        .collect();
    assert_eq!(per_call.len(), samples);
    per_call.sort_unstable();
    let printed = line(row, &per_call);
    println!("{printed}");
    printed
}
