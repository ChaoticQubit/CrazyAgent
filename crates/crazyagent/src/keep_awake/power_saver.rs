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

/// The power line's state (QUESTION 39g): the cookie of the hold it has, whether the owner
/// picked a profile during this hold, and what the daemon offers while the line rests,
/// from a first look, the last hold, or the last failure.
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

    /// Brings the hold to the ask: holds power saver once while it is wanted, keeps a
    /// profile the owner picked for the rest of that hold, and lets go when it is not.
    /// Input: whether the running hold asks for low power mode. Output: the low power
    /// support the line has now.
    pub(super) fn step(&mut self, wanted: bool) -> LowPowerSupport {
        assert!(self.cookie.is_none() || !self.owner_changed);
        let support = if wanted { self.hold() } else { self.rest() };
        assert!(wanted || self.cookie.is_none());
        assert!(self.cookie.is_none() || support == LowPowerSupport::Held);
        assert!(!self.owner_changed || support == LowPowerSupport::OwnerChanged);
        support
    }

    /// Holds power saver for a hold that asks for it, unless the owner picked a profile
    /// during this hold. Output: held, owner changed, or not available with why.
    fn hold(&mut self) -> LowPowerSupport {
        if self.owner_changed {
            assert!(self.cookie.is_none());
            return LowPowerSupport::OwnerChanged;
        }
        if self.cookie.is_some() {
            return self.check();
        }
        match self.saver.hold() {
            Ok(cookie) => {
                self.cookie = Some(cookie);
                self.looked = Some(LowPowerSupport::Ready);
                LowPowerSupport::Held
            }
            Err(problem) => {
                let support = not_available(&problem);
                self.looked = Some(support.clone());
                support
            }
        }
    }

    /// Looks whether the daemon still lists the line's hold; one it dropped means the
    /// owner picked a profile, which is kept. A look that fails counts as still held, and
    /// the next reading looks again. Output: held or owner changed.
    fn check(&mut self) -> LowPowerSupport {
        assert!(self.cookie.is_some());
        match self.saver.holds_ours() {
            Ok(true) => LowPowerSupport::Held,
            Ok(false) => {
                self.cookie = None;
                self.owner_changed = true;
                LowPowerSupport::OwnerChanged
            }
            Err(problem) => {
                let words = not_available(&problem);
                eprintln!("keep awake: the power saver holds could not be read: {words:?}");
                LowPowerSupport::Held
            }
        }
    }

    /// Lets go of the line's hold, if it has one, and says what the daemon offers, looking
    /// once when nothing said so yet. Output: ready, or not available with why.
    fn rest(&mut self) -> LowPowerSupport {
        self.owner_changed = false;
        if let Some(cookie) = self.cookie.take() {
            self.saver.release(cookie);
            self.looked = Some(LowPowerSupport::Ready);
        }
        let looked = match self.looked.clone() {
            Some(looked) => looked,
            None => {
                let found = match self.saver.present() {
                    Ok(()) => LowPowerSupport::Ready,
                    Err(problem) => not_available(&problem),
                };
                self.looked = Some(found.clone());
                found
            }
        };
        assert_ne!(looked, LowPowerSupport::Held);
        assert_ne!(looked, LowPowerSupport::OwnerChanged);
        looked
    }
}

/// The words the low power row shows for a daemon that did not answer as asked, with the
/// daemon's own text cut to `REASON_CHARS_MAX` characters and no control character. Input:
/// why. Output: not available with those words.
fn not_available(problem: &SaverError) -> LowPowerSupport {
    let reason = match problem {
        SaverError::NotRunning => NOT_RUNNING.to_owned(),
        SaverError::Refused => REFUSED.to_owned(),
        SaverError::Failed(text) => {
            let shown = text.chars().filter(|character| !character.is_control());
            format!("{FAILED}{}", shown.take(REASON_CHARS_MAX).collect::<String>())
        }
    };
    assert!(reason.starts_with("Not available: "));
    assert!(reason.chars().count() <= FAILED.chars().count() + REASON_CHARS_MAX);
    LowPowerSupport::NotAvailable { reason }
}
