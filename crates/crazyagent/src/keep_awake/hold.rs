use super::state::Want;

/// The words the system shows for our hold where it shows any (the Windows power request
/// list, the Linux inhibitor list).
pub const REASON: &str = "CrazyAgent keeps this computer awake while agents work";

const PROBLEM_BYTES_MAX: usize = 200;

/// What the platform holds right now: idle sleep, the closed lid, the display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Held {
    pub idle: bool,
    pub lid: bool,
    pub display: bool,
}

impl Held {
    /// Nothing is held.
    pub const NONE: Self = Self {
        idle: false,
        lid: false,
        display: false,
    };
}

/// Why a hold is not in place: one that was held went away by itself (`Lost`, the next
/// `apply` takes it again), or the platform could not take it (`Unavailable`, with a short
/// plain reason).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoldError {
    Lost,
    Unavailable(String),
}

impl HoldError {
    /// Builds `Unavailable` from the step that failed and its error, cut to a short text.
    /// Inputs: the step name and the error. Output: the error value.
    pub(super) fn unavailable(step: &str, problem: impl std::fmt::Display) -> Self {
        assert!(!step.is_empty());
        let mut text = format!("{step}: {problem}");
        let mut end = text.len().min(PROBLEM_BYTES_MAX);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        assert!(text.len() <= PROBLEM_BYTES_MAX);
        Self::Unavailable(text)
    }
}

impl std::fmt::Display for HoldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lost => write!(f, "the hold was lost"),
            Self::Unavailable(reason) => write!(f, "{reason}"),
        }
    }
}

/// The platform side of Keep awake (DEC-167). `apply` takes, changes or drops the hold so
/// it matches what is wanted and reports what is held; it is also the check that a hold
/// taken earlier is still there. `release` drops everything.
pub trait Hold: std::fmt::Debug {
    fn apply(&mut self, want: &Want) -> Result<Held, HoldError>;
    fn release(&mut self);
}

/// Reads what a platform hold has to do for a wanted state. Input: the wanted state.
/// Output: none for no hold, or whether the hold also keeps the display on.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
pub(super) fn wanted(want: &Want) -> Option<bool> {
    match *want {
        Want::Nothing => None,
        Want::Idle { display } | Want::IdleAndLid { display } => Some(display),
    }
}

/// The hold of a platform Keep awake does not know: it can never be taken.
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
#[derive(Debug)]
struct NoHold;

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
impl Hold for NoHold {
    fn apply(&mut self, want: &Want) -> Result<Held, HoldError> {
        match want {
            Want::Nothing => Ok(Held::NONE),
            _ => Err(HoldError::unavailable("platform", "no hold on this system")),
        }
    }

    fn release(&mut self) {}
}

/// Builds this platform's hold: a `caffeinate` child on macOS, a logind inhibitor lock on
/// Linux, a power request on Windows. Input: the pid of the engine the hold is for, so a
/// macOS hold ends with that process. Output: the hold, holding nothing yet.
pub fn platform_hold(engine_pid: u32) -> Box<dyn Hold + Send> {
    assert!(engine_pid > 1);
    #[cfg(target_os = "macos")]
    let hold = super::hold_macos::MacHold::new(engine_pid);
    #[cfg(target_os = "linux")]
    let hold = super::hold_linux::LinuxHold::default();
    #[cfg(windows)]
    let hold = super::hold_windows::WindowsHold::default();
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    let hold = NoHold;
    Box::new(hold)
}
