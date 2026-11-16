use super::state::LowPowerSupport;

/// The words when no power profiles daemon answers on the system bus (QUESTION 39g).
pub(super) const NOT_RUNNING: &str = "Not available: power-profiles-daemon is not running";

/// The words when the system refused the hold.
pub(super) const REFUSED: &str = "Not available: the system did not allow power saver";

/// The start of the words when the hold failed another way; the daemon's text follows, cut
/// to `REASON_CHARS_MAX` characters with control characters dropped.
pub(super) const FAILED: &str = "Not available: power saver could not be held: ";

/// The most characters of a daemon's own text the low power row shows.
pub(super) const REASON_CHARS_MAX: usize = 120;

/// Why the daemon did not answer as asked: no daemon on the bus, the system refused, or
/// another failure with its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SaverError {
    NotRunning,
    Refused,
    Failed(String),
}

/// The platform's power saver as the power line uses it: on Linux, power-profiles-daemon
/// over the system bus. A hold lives until `release`, until the engine's bus connection
/// closes, or until the owner picks a profile, which makes the daemon drop it.
pub(super) trait Saver: Send {
    /// Whether a daemon answers at all. Output: Ok, or why not.
    fn present(&mut self) -> Result<(), SaverError>;

    /// Asks the daemon to hold power saver. Output: the hold's cookie, or why not.
    fn hold(&mut self) -> Result<u32, SaverError>;

    /// Whether the daemon still lists our hold. Output: the answer, or why it could not be
    /// read.
    fn holds_ours(&mut self) -> Result<bool, SaverError>;

    /// Ends our hold. Input: the cookie `hold` gave.
    fn release(&mut self, cookie: u32);
}

/// The power line's state (QUESTION 39g): the hold it has, whether the owner picked a
/// profile during this hold, and what a first look at the daemon found.
pub(super) struct PowerSaver {
    saver: Box<dyn Saver>,
    cookie: Option<u32>,
    owner_changed: bool,
    looked: Option<LowPowerSupport>,
}

impl PowerSaver {
    /// A power line around a saver that holds nothing yet. Input: the saver. Output: the
    /// line.
    pub(super) fn new(saver: Box<dyn Saver>) -> Self {
        Self {
            saver,
            cookie: None,
            owner_changed: false,
            looked: None,
        }
    }

    /// Brings the hold to the ask. Input: whether the running hold asks for low power mode.
    /// Output: the low power support the line has now.
    pub(super) fn step(&mut self, wanted: bool) -> LowPowerSupport {
        let red_stub = (wanted, self.cookie, self.owner_changed, self.looked.is_some());
        assert!(red_stub.1.is_none() || !red_stub.2);
        LowPowerSupport::Unread
    }
}
