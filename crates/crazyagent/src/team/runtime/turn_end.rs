use super::TurnEnd;

/// How an engine-typed turn ended once its app finished it: a rate limit read during the
/// turn wins over the app's own end, so pool failover hands the task on; otherwise a
/// successful end stays a success even after an interrupt, so finished work is never
/// cancelled. Inputs: whether the turn read a rate limit, whether it was interrupted and the
/// app's success. Output: the turn's end.
pub(super) fn launched_end(limited: bool, interrupted: bool, success: bool) -> TurnEnd {
    match (limited, success, interrupted) {
        (true, ..) => TurnEnd::RateLimited,
        (false, true, _) => TurnEnd::Success,
        (false, false, true) => TurnEnd::Interrupted,
        (false, false, false) => TurnEnd::Failed,
    }
}

#[cfg(all(test, unix))]
#[path = "limit_order_tests.rs"]
mod tests;
