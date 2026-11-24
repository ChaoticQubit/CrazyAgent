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
    let _ = (sorted, percentile);
    todo!()
}

/// The PERF line of one row from its per-call times in ns, ascending: whole microseconds
/// for a median of 10 us or more, else nanoseconds with the batch. Inputs: the row and
/// the samples. Output: the line; a row out of shape panics.
pub fn line(row: &Row, sorted: &[u64]) -> String {
    let _ = (row, sorted);
    todo!()
}

/// Times one row: a warm-up of a tenth of the samples, then each sample runs the step
/// `batch` times and keeps the time per call. Prints the PERF line. Inputs: the row, the
/// samples and the step. Output: the printed line.
pub fn time(row: &Row, samples: usize, step: impl FnMut()) -> String {
    let _ = (row, samples, step, Instant::now());
    todo!()
}
