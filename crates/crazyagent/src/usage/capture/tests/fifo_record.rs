use super::*;

#[test]
fn a_record_that_is_a_fifo_is_skipped_without_waiting_for_a_writer() {
    let home = tempfile::tempdir().unwrap();
    let fifo = home.path().join("projects/-work-alpha/pipe.jsonl");
    mkfifo(&fifo);
    let (sent, read) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut progress = Progress::default();
        let found = read_record(&fifo, Format::Claude, &mut progress, NOW);
        let _ = sent.send((found, progress));
    });
    assert_eq!(
        read.recv_timeout(std::time::Duration::from_secs(5)),
        Ok(((Vec::new(), None), Progress::default())),
        "a FIFO record is skipped without waiting for a writer"
    );
}
