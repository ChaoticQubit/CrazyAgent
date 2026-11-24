use std::hint::black_box;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::migrations::{backup_time, stamp, stamp_time};
use crate::namespace::Namespace;
use crate::perf_kit::{Row, SAMPLES, SAMPLES_MIN, line, time};
use crate::state_log::replay::{Logged, chunk};
use crate::state_log::{
    AggregateName, Chunk, Clock, Draft, SNAPSHOT_CHUNK, check, read_chunk, snapshot_bodies,
};

#[cfg(unix)]
mod root_files;
#[cfg(unix)]
mod sized;
mod store;

const BATCH: usize = 1000;
const SNAPSHOT_BATCH: usize = 10;
const PLUGIN: &str = "acme-embeddings-small-model-ranker-v2-plus";
const BACKUP_NAME: &str = "9-to-10-20261124T082500123Z";
const STAMP: &str = "20261124T082500123Z";
const AGGREGATE: &str = "plugins";
pub(crate) const AT_MS: u64 = 1_795_508_700_123;
const BODY_BYTES: usize = 1024;
const CHUNKS: usize = 4;
const ROWS_PER_CHUNK: usize = 8;

/// One storage row at layer 1. Inputs: the dotted call name and the calls each sample
/// batches. Output: the row.
pub(crate) fn row(name: &'static str, batch: usize) -> Row {
    Row {
        layer: 1,
        module: "storage",
        name,
        batch,
    }
}

/// The nanoseconds since a start. Input: the start. Output: the span.
pub(crate) fn nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).expect("span fits u64")
}

/// Times a call whose fixture each sample sets up again: `one` sets it up, times one call
/// and returns that span, a tenth of the samples running first as warm-up. Prints the PERF
/// line. Inputs: the row (batch 1), the samples and the sample. Output: none.
pub(crate) fn sampled(row: &Row, samples: usize, mut one: impl FnMut() -> u64) {
    assert!(samples >= SAMPLES_MIN);
    assert_eq!(row.batch, 1);
    for _ in 0..samples / 10 {
        one();
    }
    let mut spans: Vec<u64> = (0..samples).map(|_| one()).collect();
    assert_eq!(spans.len(), samples);
    spans.sort_unstable();
    println!("{}", line(row, &spans));
}

/// The time the pure rows format and read: 2026-11-24 08:25:00.123 UTC. Output: the time.
pub(crate) fn at() -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(AT_MS)
}

/// A snapshot run of [`CHUNKS`] chunks of [`ROWS_PER_CHUNK`] rows each, about 1 KiB of
/// JSON per chunk. Output: the chunks.
pub(crate) fn chunks() -> Vec<Chunk> {
    (0..CHUNKS)
        .map(|at| Chunk {
            table: "plugin_records".to_owned(),
            rows: (0..ROWS_PER_CHUNK)
                .map(|row| {
                    serde_json::json!([
                        "acme-embeddings",
                        format!("key-{at}-{row}"),
                        "dmFsdWUtdmFsdWUtdmFsdWUtdmFsdWUtdmFsdWUtdmFsdWUtdmFsdWU=",
                        AT_MS
                    ])
                })
                .collect(),
        })
        .collect()
}

/// Times the storage calls that do no I/O: names, stamps, drafts, clocks and snapshot
/// bodies, each checked once first. Prints one PERF line per call. The manual clock and
/// its setter are test-only calls, timed because they are public; the snapshot bodies row
/// clones its run of chunks inside the timed span, since the call takes them by value.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn storage_pure_calls() {
    let aggregate = AggregateName::new(AGGREGATE);
    let body = vec![b'a'; BODY_BYTES];
    let draft = Draft {
        aggregate,
        kind: "RecordPut",
        schema: 1,
        at_ms: AT_MS,
        body: &body,
    };
    let run = chunks();
    let (bodies, _) = snapshot_bodies(aggregate, run.clone()).expect("snapshot bodies");
    let event = Logged {
        seq: 7,
        revision: 3,
        aggregate,
        kind: SNAPSHOT_CHUNK.to_owned(),
        schema: 1,
        at_ms: AT_MS,
        body: bodies[0].clone(),
    };
    let manual = Clock::manual(AT_MS);
    assert!(Namespace::for_plugin(PLUGIN).is_ok());
    assert_eq!(backup_time(BACKUP_NAME), Some(STAMP));
    assert_eq!(stamp(at()).as_deref(), Ok(STAMP));
    assert_eq!(stamp_time(STAMP), Some(at()));
    assert!(check(&draft).is_ok());
    assert!(AggregateName::shaped(AGGREGATE));
    assert_eq!(aggregate.name(), AGGREGATE);
    assert_eq!(manual.now_ms(), AT_MS);
    assert!(Clock::System.now_ms() > AT_MS - 86_400_000 * 365);
    assert_eq!(read_chunk(AGGREGATE, &bodies[0]), Some(chunks().remove(0)));
    assert_eq!(chunk(&event), Ok(chunks().remove(0)));

    time(
        &row("namespace.Namespace.for_plugin", BATCH),
        SAMPLES,
        || {
            let _ = black_box(Namespace::for_plugin(black_box(PLUGIN)));
        },
    );
    time(&row("migrations.backup_time", BATCH), SAMPLES, || {
        black_box(backup_time(black_box(BACKUP_NAME)));
    });
    time(&row("migrations.stamp", BATCH), SAMPLES, || {
        let _ = black_box(stamp(black_box(at())));
    });
    time(&row("migrations.stamp_time", BATCH), SAMPLES, || {
        black_box(stamp_time(black_box(STAMP)));
    });
    #[cfg(unix)]
    time(&row("restore.restore_failed", BATCH), SAMPLES, || {
        black_box(crate::restore::restore_failed(black_box("copy changed")));
    });
    time(&row("state_log.check", BATCH), SAMPLES, || {
        let _ = black_box(check(black_box(&draft)));
    });
    time(
        &row("state_log.aggregate.AggregateName.shaped", BATCH),
        SAMPLES,
        || {
            black_box(AggregateName::shaped(black_box(AGGREGATE)));
        },
    );
    time(
        &row("state_log.aggregate.AggregateName.new", BATCH),
        SAMPLES,
        || {
            black_box(AggregateName::new(black_box(AGGREGATE)));
        },
    );
    time(
        &row("state_log.aggregate.AggregateName.name", BATCH),
        SAMPLES,
        || {
            black_box(black_box(aggregate).name());
        },
    );
    time(&row("state_log.Clock.manual", BATCH), SAMPLES, || {
        black_box(Clock::manual(black_box(AT_MS)));
    });
    time(&row("state_log.Clock.now_ms", BATCH), SAMPLES, || {
        black_box(black_box(&Clock::System).now_ms());
    });
    time(&row("state_log.Clock.set_ms", BATCH), SAMPLES, || {
        let _ = black_box(black_box(&manual).set_ms(black_box(AT_MS)));
    });
    time(
        &row("state_log.snapshot_bodies", SNAPSHOT_BATCH),
        SAMPLES,
        || {
            let _ = black_box(snapshot_bodies(
                black_box(aggregate),
                black_box(&run).clone(),
            ));
        },
    );
    time(
        &row("state_log.read_chunk", SNAPSHOT_BATCH),
        SAMPLES,
        || {
            black_box(read_chunk(black_box(AGGREGATE), black_box(&bodies[0])));
        },
    );
    time(
        &row("state_log.replay.chunk", SNAPSHOT_BATCH),
        SAMPLES,
        || {
            let _ = black_box(chunk(black_box(&event)));
        },
    );
}
