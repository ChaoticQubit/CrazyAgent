use super::*;

fn output(chunk: Result<Chunk, TryRecvError>) -> Vec<u8> {
    match chunk {
        Ok(Chunk::Output(bytes)) => bytes,
        other => panic!("expected output, read {other:?}"),
    }
}

#[test]
fn a_viewer_reads_output_in_order_and_then_the_end() {
    let (feed, mut chunks) = buffer();
    assert_eq!(chunks.try_recv(), Err(TryRecvError::Empty));
    assert!(feed.push(b"one"));
    assert!(feed.push(&vec![b'x'; CHUNK_BYTES_MAX + 1]));
    let first = output(chunks.try_recv());
    assert_eq!(first.len(), CHUNK_BYTES_MAX);
    assert!(first.starts_with(b"onex"));
    assert_eq!(output(chunks.try_recv()), b"xxxx");
    drop(feed);
    assert_eq!(chunks.try_recv(), Err(TryRecvError::Disconnected));
    assert_eq!(chunks.blocking_recv(), None);
}

#[test]
fn a_viewer_past_the_byte_bound_reads_behind_once_and_then_its_end() {
    let (feed, mut chunks) = buffer();
    let chunk = vec![b'a'; CHUNK_BYTES_MAX];
    for _ in 0..VIEWER_BUFFER_BYTES_MAX / CHUNK_BYTES_MAX + 10 {
        assert!(
            feed.push(&chunk),
            "a slow viewer is never dropped by the host"
        );
    }
    assert!(feed.push(b"newest"));
    assert_eq!(chunks.try_recv(), Ok(Chunk::Behind));
    assert_eq!(chunks.try_recv(), Err(TryRecvError::Disconnected));
    assert!(!feed.open(), "a viewer that read Behind is forgotten");
    assert!(!feed.push(b"later"));
}

#[test]
fn output_the_host_lost_tells_the_viewer_it_is_behind() {
    let (feed, mut chunks) = buffer();
    assert!(feed.push(b"before"));
    assert!(feed.lost());
    assert!(feed.push(b"after"));
    assert_eq!(chunks.try_recv(), Ok(Chunk::Behind));
    assert_eq!(chunks.blocking_recv(), None);
    assert!(!feed.lost());
}

#[test]
fn many_tiny_writes_reach_a_viewer_that_has_not_read_yet_whole_and_in_order() {
    let (feed, mut chunks) = buffer();
    let mut pushed = Vec::new();
    for index in 0..STREAM_CHUNKS_MAX * 4 {
        let write = format!("\x1b[{}m{}", index % 8, index % 10).into_bytes();
        assert!(feed.push(&write));
        pushed.extend_from_slice(&write);
    }
    assert!(pushed.len() < VIEWER_BUFFER_BYTES_MAX);
    let mut read = Vec::new();
    loop {
        match chunks.try_recv() {
            Ok(Chunk::Output(next)) => read.extend_from_slice(&next),
            Ok(Chunk::Behind) => panic!("small writes under the byte bound never fall behind"),
            Err(_) => break,
        }
    }
    assert_eq!(
        String::from_utf8_lossy(&read),
        String::from_utf8_lossy(&pushed),
        "small writes far under the byte bound are never dropped, however many there are"
    );
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
        assert_eq!(chunks.recv().await, Some(Chunk::Output(b"later".to_vec())));
        assert_eq!(chunks.recv().await, None);
    });
    writer.join().unwrap();
}
