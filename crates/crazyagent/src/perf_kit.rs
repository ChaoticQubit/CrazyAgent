use std::time::{Duration, Instant};

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

/// The PERF line of one row from its per-call times in ns, ascending: whole microseconds
/// for a median of 10 us or more, else nanoseconds with the batch. Inputs: the row and
/// the samples. Output: the line; a row out of shape panics.
pub fn line(row: &Row, sorted: &[u64]) -> String {
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
    let (p50, p95) = (rank(sorted, 50), rank(sorted, 95));
    let head = format!("PERF {} {} {}", row.layer, row.module, row.name);
    let n = sorted.len();
    if p50 < 10_000 {
        format!("{head} p50_ns={p50} p95_ns={p95} n={n} batch={}", row.batch)
    } else if row.batch == 1 {
        format!("{head} p50={} p95={} n={n}", p50 / 1000, p95 / 1000)
    } else {
        format!(
            "{head} p50={} p95={} n={n} batch={}",
            p50 / 1000,
            p95 / 1000,
            row.batch
        )
    }
}

/// The seconds a flame graphed row runs when no call count is given.
pub const FLAME_SECS_DEFAULT: u64 = 8;

/// The most seconds a flame graphed row may run.
pub const FLAME_SECS_MAX: u64 = 600;

/// What [`time`] does with one row: time it, skip it while another row is flame graphed,
/// or run it alone, untimed, for a flame graph or a syscall count.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    Time,
    Skip,
    Flame { calls: Option<u64>, secs: u64 },
}

/// The plan for one row from the flame mode's settings: `CRAZYAGENT_PERF_FLAME` names the
/// one row to run, `CRAZYAGENT_PERF_FLAME_CALLS` its exact call count and
/// `CRAZYAGENT_PERF_FLAME_SECS` its run time when no count is given. Unset or empty, every
/// row is timed as usual. Inputs: the three settings and the row's name. Output: the plan;
/// a count or a time that does not read as a number in range panics.
pub fn plan(flame: Option<&str>, calls: Option<&str>, secs: Option<&str>, name: &str) -> Plan {
    let Some(flamed) = flame.filter(|flamed| !flamed.is_empty()) else {
        return Plan::Time;
    };
    if flamed != name {
        return Plan::Skip;
    }
    let calls = calls.map(|calls| {
        calls
            .parse::<u64>()
            .unwrap_or_else(|_| panic!("CRAZYAGENT_PERF_FLAME_CALLS {calls:?} is not a count"))
    });
    let secs = secs.map_or(FLAME_SECS_DEFAULT, |secs| {
        secs.parse::<u64>()
            .unwrap_or_else(|_| panic!("CRAZYAGENT_PERF_FLAME_SECS {secs:?} is not seconds"))
    });
    assert!(
        (1..=FLAME_SECS_MAX).contains(&secs),
        "CRAZYAGENT_PERF_FLAME_SECS {secs} is out of range"
    );
    Plan::Flame { calls, secs }
}

/// Runs one row's step alone, untimed: prints a FLAME-START line, then calls the step
/// `calls` times, or for `secs` seconds when no count is given, then prints a FLAME-END
/// line with the calls made and the time taken. Inputs: the row, the count or the seconds,
/// and the step. Output: the FLAME-END line.
pub fn flame(row: &Row, calls: Option<u64>, secs: u64, mut step: impl FnMut()) -> String {
    let head = format!("{} {} {}", row.layer, row.module, row.name);
    println!("FLAME-START {head}");
    let limit = Duration::from_secs(secs);
    let started = Instant::now();
    let mut made = 0_u64;
    while calls.map_or_else(|| started.elapsed() < limit, |calls| made < calls) {
        step();
        made += 1;
    }
    let ns = started.elapsed().as_nanos();
    assert!(calls.is_none_or(|calls| made == calls));
    let ended = format!("FLAME-END {head} calls={made} ns={ns}");
    println!("{ended}");
    ended
}

/// Runs a row the flame mode skips: its step is called, untimed, as many times as timing
/// it would call it, so a bench whose later steps rely on what this row did still runs.
/// Inputs: the row, the samples and the step. Output: an empty line; nothing is printed.
pub fn skip(row: &Row, samples: usize, mut step: impl FnMut()) -> String {
    assert!(samples >= SAMPLES_MIN);
    assert!((1..=BATCH_MAX).contains(&row.batch));
    for _ in 0..(samples / 10 + samples) * row.batch {
        step();
    }
    String::new()
}

/// Times one row: a warm-up of a tenth of the samples, then each sample runs the step
/// `batch` times and keeps the time per call. Prints the PERF line. Inputs: the row, the
/// samples and the step. Output: the printed line.
pub fn time(row: &Row, samples: usize, mut step: impl FnMut()) -> String {
    assert!(samples >= SAMPLES_MIN);
    assert!((1..=BATCH_MAX).contains(&row.batch));
    let setting = |key: &str| std::env::var(key).ok();
    match plan(
        setting("CRAZYAGENT_PERF_FLAME").as_deref(),
        setting("CRAZYAGENT_PERF_FLAME_CALLS").as_deref(),
        setting("CRAZYAGENT_PERF_FLAME_SECS").as_deref(),
        row.name,
    ) {
        Plan::Time => {}
        Plan::Skip => return skip(row, samples, step),
        Plan::Flame { calls, secs } => return flame(row, calls, secs, step),
    }
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
