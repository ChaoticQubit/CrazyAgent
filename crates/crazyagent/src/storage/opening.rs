/// The proof that a write runs while storage opens its database under the open's exclusive
/// lock, before any engine host of that database starts. The storage open and its schema
/// steps are the only writers that hold one, and only storage makes one (v2.5 L6). It
/// carries the open's clock for the events those writers append.
#[derive(Clone, Debug)]
pub struct Opening {
    at_ms: u64,
}

impl Opening {
    /// The proof for one storage open. Input: the open's clock, in ms since the epoch.
    /// Output: the proof.
    #[cfg(any(unix, test))]
    pub(in crate::storage) fn new(at_ms: u64) -> Self {
        assert!(at_ms > 0, "an opening reads the clock");
        let opening = Self { at_ms };
        assert_eq!(opening.at_ms(), at_ms);
        opening
    }

    /// The proof for one storage open at the system clock's now, on every platform the open
    /// runs on. Output: the proof.
    pub(in crate::storage) fn now() -> Self {
        let at_ms = crate::state_log::replay::Clock::System.now_ms();
        assert!(at_ms > 0, "an opening reads the clock");
        Self { at_ms }
    }

    /// The open's clock, which the events of its writers carry. Output: ms since the epoch.
    pub fn at_ms(&self) -> u64 {
        self.at_ms
    }

    /// A proof for rows outside storage. Input: the clock. Output: the proof.
    #[cfg(test)]
    pub(crate) fn for_test(at_ms: u64) -> Self {
        Self::new(at_ms)
    }
}
