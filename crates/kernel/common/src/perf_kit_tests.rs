use crate::perf_kit::{Row, SAMPLES_MIN, line, rank, time};

fn row(name: &'static str, batch: usize) -> Row {
    Row {
        layer: 1,
        module: "common",
        name,
        batch,
    }
}

#[test]
fn the_rank_is_the_nearest_rank_of_the_sorted_samples() {
    let sorted: Vec<u64> = (1..=200).collect();
    assert_eq!(rank(&sorted, 50), 100);
    assert_eq!(rank(&sorted, 95), 190);
    assert_eq!(rank(&sorted, 100), 200);
    let odd: Vec<u64> = (1..=21).collect();
    assert_eq!(rank(&odd, 50), 11);
    assert_eq!(rank(&odd, 95), 20);
}

#[test]
fn a_median_under_ten_microseconds_prints_nanoseconds_and_the_batch() {
    let mut sorted = vec![9_999_u64; SAMPLES_MIN];
    sorted[SAMPLES_MIN - 1] = 12_000;
    assert_eq!(
        line(&row("names.is_snake", 1000), &sorted),
        format!("PERF 1 common names.is_snake p50_ns=9999 p95_ns=9999 n={SAMPLES_MIN} batch=1000")
    );
}

#[test]
fn a_median_of_ten_microseconds_or_more_prints_whole_microseconds() {
    let mut sorted = vec![10_000_u64; SAMPLES_MIN];
    sorted[SAMPLES_MIN - 1] = 25_999;
    assert_eq!(
        line(&row("hash", 1), &sorted),
        format!("PERF 1 common hash p50=10 p95=10 n={SAMPLES_MIN}")
    );
    let slow = vec![1_234_567_u64; SAMPLES_MIN];
    assert_eq!(
        line(&row("hash", 4), &slow),
        format!("PERF 1 common hash p50=1234 p95=1234 n={SAMPLES_MIN} batch=4")
    );
}

#[test]
fn a_timed_row_calls_the_step_batch_times_per_sample_after_a_warm_up() {
    let mut calls = 0_usize;
    let printed = time(&row("names.is_dotted", 3), SAMPLES_MIN, || calls += 1);
    assert!(calls >= 3 * SAMPLES_MIN);
    assert_eq!((calls - 3 * SAMPLES_MIN) % 3, 0);
    assert!(printed.starts_with("PERF 1 common names.is_dotted p50"));
    assert!(printed.contains(&format!(" n={SAMPLES_MIN}")));
}

#[test]
#[should_panic(expected = "perf row name")]
fn a_row_name_with_a_space_is_refused() {
    let _ = line(&row("names is_snake", 1), &[1; SAMPLES_MIN]);
}

#[test]
#[should_panic(expected = "perf row module")]
fn a_row_of_an_unknown_module_is_refused() {
    let unknown = Row {
        layer: 1,
        module: "kernel_common",
        name: "hash",
        batch: 1,
    };
    let _ = line(&unknown, &[1; SAMPLES_MIN]);
}
