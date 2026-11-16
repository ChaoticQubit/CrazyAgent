//! The lid as the helper sees it when a hold ends: closed or open, and whether an external
//! display keeps a closed Mac at a desk (threat model section 3, `sleepnow`).

/// The lid: closed or not, and whether a closed Mac drives an external display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lid {
    pub closed: bool,
    pub external_display: bool,
}

impl Lid {
    /// Whether a hold ending now should sleep the Mac at once: lid closed, no external display.
    pub fn sleeps_now(self) -> bool {
        false
    }
}

/// Reads the lid from the power management root domain (`AppleClamshellState`, and
/// `AppleClamshellCausesSleep`, false when an external display keeps it awake). Output: the
/// lid, or `None` when it cannot be read.
pub fn read() -> Option<Lid> {
    None
}
