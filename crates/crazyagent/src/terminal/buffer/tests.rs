use super::*;

#[test]
fn a_viewer_reads_output_in_order_and_then_the_end() {
    let (feed, mut chunks) = buffer();
    assert_eq!(chunks.try_recv(), Err(TryRecvError::Empty));
    assert!(feed.push(b"one"));
    assert!(feed.push(&vec![b'x'; CHUNK_BYTES_MAX + 1]));
    assert_eq!(chunks.try_recv().unwrap(), b"one");
    assert_eq!(chunks.try_recv().unwrap().len(), CHUNK_BYTES_MAX);
    assert_eq!(chunks.try_recv().unwrap(), b"x");
    drop(feed);
    assert_eq!(chunks.try_recv(), Err(TryRecvError::Disconnected));
    assert_eq!(chunks.blocking_recv(), None);
}

#[test]
fn a_slow_viewer_loses_the_oldest_output_and_is_told_how_much() {
    let (feed, mut chunks) = buffer();
    let chunk = vec![b'a'; CHUNK_BYTES_MAX];
    let total = VIEWER_BUFFER_BYTES_MAX / CHUNK_BYTES_MAX + 10;
    for _ in 0..total {
        assert!(feed.push(&chunk), "a slow viewer is never dropped");
    }
    assert!(feed.push(b"newest"));
    let pushed = total * CHUNK_BYTES_MAX + b"newest".len();
    let marker = String::from_utf8(chunks.try_recv().unwrap()).unwrap();
    let skipped: usize = marker
        .strip_prefix("\r\n\u{1b}[0m[")
        .and_then(|rest| rest.strip_suffix(" bytes of output skipped]\r\n"))
        .unwrap_or_else(|| panic!("{marker:?}"))
        .parse()
        .unwrap();
    assert!(skipped > 0);
    let mut read = 0;
    let mut last = Vec::new();
    while let Ok(next) = chunks.try_recv() {
        read += next.len();
        last = next;
    }
    assert_eq!(last, b"newest", "the newest output is always kept");
    assert_eq!(
        skipped + read,
        pushed,
        "nothing is lost without being counted"
    );
    assert!(read <= VIEWER_BUFFER_BYTES_MAX);
}

#[test]
fn the_host_forgets_a_viewer_that_went_away() {
    let (feed, chunks) = buffer();
    assert!(feed.open());
    drop(chunks);
    assert!(!feed.open());
    assert!(!feed.push(b"late"));
}

#[test]
fn an_async_viewer_wakes_for_output_and_for_the_end() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let (feed, mut chunks) = buffer();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(feed.push(b"later"));
        std::thread::sleep(std::time::Duration::from_millis(50));
    });
    runtime.block_on(async {
        assert_eq!(chunks.recv().await.unwrap(), b"later");
        assert_eq!(chunks.recv().await, None);
    });
    writer.join().unwrap();
}

#[test]
fn the_skipped_marker_fits_one_chunk() {
    assert_eq!(
        skipped_marker(12),
        b"\r\n\x1b[0m[12 bytes of output skipped]\r\n"
    );
    assert!(skipped_marker(u64::MAX).len() <= CHUNK_BYTES_MAX);
}
