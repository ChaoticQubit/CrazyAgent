use super::*;

/// Hands each line a durable harness writes to its output file to the session until the
/// holder of the file is gone and the file is read to its end. A line longer than
/// `LINE_BYTES_MAX` is passed over to its newline and reported once as skipped. A log read
/// to its end past `DURABLE_OUTPUT_KEPT_MAX` is emptied at a line boundary, so the log on
/// disk stays bounded and the agent runs on. Takes the output file, open for reading and
/// writing, the holder probe and the sender.
pub(super) fn tail(output: File, probe: File, sender: SyncSender<Line>) {
    let mut reader = BufReader::new(output);
    let mut line = Vec::new();
    let mut skipping = false;
    let mut draining = false;
    loop {
        assert!(line.len() <= LINE_BYTES_MAX);
        let limit = u64::try_from(LINE_BYTES_MAX + 1 - line.len()).unwrap_or(1);
        let count = match reader.by_ref().take(limit).read_until(b'\n', &mut line) {
            Ok(count) => count,
            Err(_) => {
                let _ = sender.send(Line::Closed);
                return;
            }
        };
        let ended = line.last() == Some(&b'\n');
        if line.len() > LINE_BYTES_MAX && !ended {
            line.clear();
            if !skipping && sender.send(Line::Skipped).is_err() {
                return;
            }
            skipping = true;
            continue;
        }
        if ended {
            let whole = std::mem::take(&mut line);
            if std::mem::take(&mut skipping) {
                continue;
            }
            if sender.send(Line::Bytes(whole)).is_err() {
                return;
            }
            continue;
        }
        if count > 0 {
            continue;
        }
        if draining {
            if !skipping
                && !line.is_empty()
                && sender.send(Line::Bytes(std::mem::take(&mut line))).is_err()
            {
                return;
            }
            let _ = sender.send(Line::Closed);
            return;
        }
        if holder_alive(&probe) {
            if (line.is_empty() || skipping) && trim_read_output(&mut reader) {
                eprintln!("launch_output_trimmed over={DURABLE_OUTPUT_KEPT_MAX}");
            }
            std::thread::sleep(TAIL_POLL);
        } else {
            draining = true;
        }
    }
}
