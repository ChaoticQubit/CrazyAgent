use super::*;

/// The count of team loop wakes so far, read before a tick so a wake during it is not
/// missed. Output: the count.
pub(super) fn wakes_seen() -> u64 {
    *WAKES.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Waits until a wake newer than `seen` or `tick` has passed. Inputs: the count read before
/// the tick and the tick. Output: the count now.
pub(super) fn wait_for_wake(seen: u64, tick: Duration) -> u64 {
    let wakes = WAKES.lock().unwrap_or_else(PoisonError::into_inner);
    let (wakes, waited) = WOKEN
        .wait_timeout_while(wakes, tick, |wakes| *wakes == seen)
        .unwrap_or_else(PoisonError::into_inner);
    assert!(waited.timed_out() || *wakes != seen);
    *wakes
}
