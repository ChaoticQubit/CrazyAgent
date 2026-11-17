use std::time::{Duration, Instant};

/// How long a poll waits between two reads of its check.
const POLL: Duration = Duration::from_millis(20);

/// The furthest deadline a poll takes, far past any row's need.
const DEADLINE_MAX: Duration = Duration::from_secs(300);

/// Reads `ready` until it holds or `deadline` passes, every `POLL`: the one sync poll test
/// code shares for state that gives no event to wait on, such as a process group gone, a
/// file written or a service running. Inputs: the deadline, at most five minutes away, and
/// the check. Output: whether the check held before the deadline.
#[must_use]
pub fn poll_until(deadline: Instant, mut ready: impl FnMut() -> bool) -> bool {
    assert!(
        deadline <= Instant::now() + DEADLINE_MAX,
        "a poll waits at most {DEADLINE_MAX:?}"
    );
    loop {
        if ready() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A check that holds on its second read is read twice and answers true; one that never
    /// holds by a deadline already past is read once and answers false.
    #[test]
    fn a_poll_answers_whether_its_check_held_before_the_deadline() {
        let mut reads = 0;
        let held = poll_until(Instant::now() + Duration::from_secs(5), || {
            reads += 1;
            reads == 2
        });
        assert!(held);
        assert_eq!(reads, 2);
        let mut late = 0;
        let held = poll_until(Instant::now(), || {
            late += 1;
            false
        });
        assert!(!held);
        assert_eq!(late, 1);
    }

    /// A deadline past five minutes is refused before the check is read.
    #[test]
    #[should_panic(expected = "a poll waits at most")]
    fn a_poll_past_five_minutes_is_refused() {
        let _ = poll_until(Instant::now() + DEADLINE_MAX * 2, || true);
    }
}
