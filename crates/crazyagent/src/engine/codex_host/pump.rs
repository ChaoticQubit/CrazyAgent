use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::Instant;

/// The most bytes one read takes from a pumped pipe.
pub(super) const CHUNK_BYTES: usize = 4096;

/// A wake a custody thread sleeps on with a bound, raised by the threads that pump its pipes
/// and by its host, so custody waits for work instead of on a fixed tick where no poll(2)
/// exists. A raise while no one waits is kept, and ends the next wait at once.
pub(super) struct Signal {}

impl Signal {
    /// Makes a signal that is not raised. Output: the signal.
    pub(super) fn new() -> Self {
        todo!()
    }

    /// Raises the signal, waking its waiter or ending its next wait at once. Input: the
    /// signal. Output: none.
    pub(super) fn raise(&self) {
        todo!()
    }

    /// Waits until the signal is raised or `until` passes, then lowers it. Inputs: the
    /// signal and the instant to stop at. Output: none.
    pub(super) fn wait_until(&self, until: Instant) {
        let _ = until;
        todo!()
    }
}

/// The custody end of a pipe a thread reads ahead into a short bounded queue: a read takes
/// what the queue holds, says `WouldBlock` while it holds nothing, and reads 0 once the pipe
/// has ended and the queue is empty. While the queue is full the thread stops reading, so
/// the writer waits on the pipe.
pub(super) struct Output {}

impl Read for Output {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let _ = buf;
        todo!()
    }
}

/// The custody end of a pipe a thread writes to: a write hands its bytes to the thread
/// whole, says `WouldBlock` while the thread still holds a write it has not begun, and
/// `BrokenPipe` once the pipe has failed. Dropping it closes the pipe after the held writes.
pub(super) struct Input {}

impl Write for Input {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = buf;
        todo!()
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Starts the thread that reads `pipe` ahead for custody, raising `signal` after each chunk
/// and at the pipe's end. Inputs: the pipe, the custody signal and the thread's name.
/// Output: the custody end, or the error starting the thread.
pub(super) fn output<R: Read + Send + 'static>(
    pipe: R,
    signal: Arc<Signal>,
    name: &str,
) -> io::Result<Output> {
    let _ = (pipe, signal, name);
    todo!()
}

/// Starts the thread that writes custody's writes to `pipe`, raising `signal` after each one
/// and when the pipe fails. Inputs: the pipe, the custody signal and the thread's name.
/// Output: the custody end, or the error starting the thread.
pub(super) fn input<W: Write + Send + 'static>(
    pipe: W,
    signal: Arc<Signal>,
    name: &str,
) -> io::Result<Input> {
    let _ = (pipe, signal, name);
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    /// Reads a pumped output until it ends, waiting on its signal while it would block, at
    /// most 10 s. Inputs: the output and its signal. Output: every byte read.
    fn read_to_end(output: &mut Output, signal: &Signal) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut read = Vec::new();
        let mut bytes = [0; CHUNK_BYTES];
        loop {
            assert!(Instant::now() < deadline, "the pumped output never ended");
            match output.read(&mut bytes) {
                Ok(0) => return read,
                Ok(count) => read.extend_from_slice(&bytes[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    signal.wait_until(Instant::now() + Duration::from_millis(250));
                }
                Err(error) => panic!("the pumped output failed: {error}"),
            }
        }
    }

    /// Writes `bytes` whole through a pumped input, waiting on its signal while it would
    /// block, at most 10 s. Inputs: the input, its signal and the bytes. Output: none.
    fn write_whole(input: &mut Input, signal: &Signal, bytes: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                Instant::now() < deadline,
                "the pumped input never took the write"
            );
            match input.write(bytes) {
                Ok(count) => {
                    assert_eq!(count, bytes.len());
                    return;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    signal.wait_until(Instant::now() + Duration::from_millis(250));
                }
                Err(error) => panic!("the pumped input failed: {error}"),
            }
        }
    }

    #[test]
    fn a_raised_signal_ends_the_next_wait_at_once_and_a_lowered_one_waits_to_its_bound() {
        let signal = Signal::new();
        signal.raise();
        let started = Instant::now();
        signal.wait_until(started + Duration::from_secs(5));
        assert!(started.elapsed() < Duration::from_secs(1));
        let started = Instant::now();
        signal.wait_until(started + Duration::from_millis(100));
        assert!(started.elapsed() >= Duration::from_millis(100));
    }

    #[test]
    fn a_signal_raised_by_another_thread_wakes_its_waiter() {
        let signal = Arc::new(Signal::new());
        let raiser = Arc::clone(&signal);
        let started = Instant::now();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            raiser.raise();
        });
        signal.wait_until(started + Duration::from_secs(5));
        assert!(started.elapsed() < Duration::from_secs(2));
        thread.join().unwrap();
    }

    #[test]
    fn a_pumped_output_reads_its_pipe_in_order_then_its_end() {
        let (reader, mut writer) = std::io::pipe().unwrap();
        let signal = Arc::new(Signal::new());
        let mut output = output(reader, Arc::clone(&signal), "pump-test-output").unwrap();
        writer.write_all(b"abc").unwrap();
        writer.write_all(b"def").unwrap();
        drop(writer);
        assert_eq!(read_to_end(&mut output, &signal), b"abcdef");
        let mut bytes = [0; 8];
        assert_eq!(output.read(&mut bytes).unwrap(), 0);
    }

    #[test]
    fn a_pumped_output_with_nothing_yet_would_block_until_its_signal_says_otherwise() {
        let (reader, mut writer) = std::io::pipe().unwrap();
        let signal = Arc::new(Signal::new());
        let mut output = output(reader, Arc::clone(&signal), "pump-test-output").unwrap();
        let mut bytes = [0; 8];
        let error = output.read(&mut bytes).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        writer.write_all(b"x").unwrap();
        let started = Instant::now();
        loop {
            signal.wait_until(Instant::now() + Duration::from_secs(5));
            match output.read(&mut bytes) {
                Ok(count) => {
                    assert_eq!(&bytes[..count], b"x");
                    break;
                }
                Err(error) => assert_eq!(error.kind(), io::ErrorKind::WouldBlock),
            }
            assert!(started.elapsed() < Duration::from_secs(5));
        }
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_pumped_output_holds_its_writer_while_its_reader_falls_behind_and_loses_nothing() {
        let (reader, mut writer) = std::io::pipe().unwrap();
        let signal = Arc::new(Signal::new());
        let mut output = output(reader, Arc::clone(&signal), "pump-test-output").unwrap();
        let sent: Vec<u8> = (0..4 * 1024 * 1024).map(|at: u32| at as u8).collect();
        let written = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&written);
        let bytes = sent.clone();
        let thread = std::thread::spawn(move || {
            writer.write_all(&bytes).unwrap();
            done.store(true, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !written.load(Ordering::SeqCst),
            "the pump read past its queue"
        );
        assert_eq!(read_to_end(&mut output, &signal), sent);
        thread.join().unwrap();
    }

    #[test]
    fn a_pumped_input_writes_each_write_in_order_and_closes_its_pipe_when_dropped() {
        let (mut reader, writer) = std::io::pipe().unwrap();
        let signal = Arc::new(Signal::new());
        let mut input = input(writer, Arc::clone(&signal), "pump-test-input").unwrap();
        let thread = std::thread::spawn(move || {
            let mut read = Vec::new();
            reader.read_to_end(&mut read).unwrap();
            read
        });
        write_whole(&mut input, &signal, b"one");
        write_whole(&mut input, &signal, b"two");
        drop(input);
        assert_eq!(thread.join().unwrap(), b"onetwo");
    }

    #[test]
    fn a_pumped_input_holds_one_write_beyond_the_one_its_pipe_is_taking() {
        let (mut reader, writer) = std::io::pipe().unwrap();
        let signal = Arc::new(Signal::new());
        let mut input = input(writer, Arc::clone(&signal), "pump-test-input").unwrap();
        let first = vec![1_u8; 1024 * 1024];
        let second = vec![2_u8; 1024];
        assert_eq!(input.write(&first).unwrap(), first.len());
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(input.write(&second).unwrap(), second.len());
        let refused = input.write(b"third").unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::WouldBlock);
        let thread = std::thread::spawn(move || {
            let mut read = Vec::new();
            reader.read_to_end(&mut read).unwrap();
            read
        });
        write_whole(&mut input, &signal, b"third");
        drop(input);
        let read = thread.join().unwrap();
        assert_eq!(read.len(), first.len() + second.len() + 5);
        assert!(read[..first.len()].iter().all(|byte| *byte == 1));
        assert_eq!(&read[first.len()..first.len() + second.len()], &second[..]);
        assert_eq!(&read[first.len() + second.len()..], b"third");
    }

    #[test]
    fn a_pumped_input_whose_pipe_closed_says_broken_pipe() {
        let (reader, writer) = std::io::pipe().unwrap();
        let signal = Arc::new(Signal::new());
        let mut input = input(writer, Arc::clone(&signal), "pump-test-input").unwrap();
        drop(reader);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match input.write(b"lost") {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => {
                    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "a closed pipe was never reported"
            );
            signal.wait_until(Instant::now() + Duration::from_millis(250));
        }
    }
}
