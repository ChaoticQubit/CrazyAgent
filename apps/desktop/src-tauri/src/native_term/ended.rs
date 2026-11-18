use crate::bridge::terminal::NATIVE_OVERFLOW;

/// The end codes a reopen cannot help: the app's access to the engine changed (the engine's
/// `Closed` reason), the engine refused a request as invalid, an attach asked for a name or
/// size it can never take, the engine opened another agent, a newer view took the terminal,
/// or the view's own typing overflowed or its drawing failed.
pub const FINAL_CODES: [&str; 8] = [
    "owner_grant_stale_or_revoked",
    "terminal_request_invalid",
    "terminal_agent_invalid",
    "terminal_size_bounds",
    "terminal_agent_mismatch",
    "terminal_view_replaced",
    NATIVE_OVERFLOW,
    "native_terminal_gpu",
];

/// Whether a view's end is final, so a view that follows its agent stops opening it again:
/// true for `FINAL_CODES`; false for an agent that ended, restarted or has not started, a
/// lost connection, an attach that failed for a passing cause or any other code, which the
/// page retries up to its cap as the xterm panel retries every end. Input: the plain code.
/// Output: the answer.
pub fn ended_final(code: &str) -> bool {
    assert!(!code.is_empty());
    let last = FINAL_CODES.contains(&code);
    assert!(!last || code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_ends_a_reopen_cannot_help_are_final() {
        for code in FINAL_CODES {
            assert!(ended_final(code), "{code}");
        }
        assert!(ended_final("native_terminal_input_overflow"));
        let retried = [
            "terminal_ended",
            "exited",
            "terminal_detached",
            "terminal_agent_unknown",
            "terminal_pane_unknown",
            "terminal_teammates_server_ended",
            "terminal_unavailable",
            "terminal_closed",
            "terminal_connection_lost",
            "terminal_connection_closed",
            "terminal_viewers_full",
            "terminal_attach_timeout",
            "terminal_attach_failed",
            "native_terminal_view_unknown",
        ];
        for code in retried {
            assert!(!ended_final(code), "{code}");
        }
    }
}
