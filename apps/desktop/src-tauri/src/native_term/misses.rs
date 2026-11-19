use std::cmp::Ordering;
use std::collections::VecDeque;

/// How many draws in a row a shown view may skip and still ask the main loop for one more
/// pass; the skip that reaches it is told once, and the view then waits for its next event.
pub const SKIPS_MAX: u32 = 3;

/// How many views a refused cover is remembered for, the oldest going first.
pub const REFUSALS_MAX: usize = 32;

/// A view with no surface yet.
pub const NO_SURFACE: &str = "no_surface";
/// A view whose pane has no screen yet.
pub const NO_SCREEN: &str = "no_screen";
/// No renderer at the view's backing scale.
pub const NO_RENDERER: &str = "no_renderer";
/// Glyphon refused to prepare the frame's text.
pub const FRAME_PREPARE: &str = "frame_prepare";
/// The surface gave no frame in time.
pub const FRAME_TIMEOUT: &str = "frame_timeout";
/// The surface changed and was configured again.
pub const FRAME_OUTDATED: &str = "frame_outdated";
/// The surface refused a frame for another reason.
pub const FRAME_TEXTURE: &str = "frame_texture";
/// Any other draw error.
pub const FRAME_REFUSED: &str = "frame_refused";

/// Every fixed reason a draw is skipped for; any other draw error is `FRAME_REFUSED`.
pub const SKIP_REASONS: [&str; 7] = [
    NO_SURFACE,
    NO_SCREEN,
    NO_RENDERER,
    FRAME_PREPARE,
    FRAME_TIMEOUT,
    FRAME_OUTDATED,
    FRAME_TEXTURE,
];

const _: () = assert!(SKIPS_MAX >= 1);
const _: () = assert!(REFUSALS_MAX >= 1);

/// What a shown view still dirty after a skipped draw asks for: one more main-loop pass,
/// the skip that reached the bound (told once), or nothing until its next event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    Again,
    Bound,
    Wait,
}

/// How one view's draw went: a frame drawn, or skipped for a fixed reason with the view
/// left dirty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Draw {
    Drawn,
    Skipped(&'static str),
}

/// The draws one shown view skipped since its last drawn frame or hide, and why the first
/// of them was skipped (V243-NATIVE-SWITCH-TIME).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Skips {
    count: u32,
    first: Option<&'static str>,
}

impl Skips {
    /// Counts one skipped draw. Input: its fixed reason. Output: what the view asks for.
    pub fn skipped(&mut self, reason: &'static str) -> Next {
        assert!(!reason.is_empty(), "a skip names its reason");
        self.count = self.count.saturating_add(1);
        let first = *self.first.get_or_insert(reason);
        assert!(self.count >= 1);
        assert!(!first.is_empty());
        match self.count.cmp(&SKIPS_MAX) {
            Ordering::Less => Next::Again,
            Ordering::Equal => Next::Bound,
            Ordering::Greater => Next::Wait,
        }
    }

    /// Starts the count again, on a drawn frame or a hide. Output: none.
    pub fn reset(&mut self) {
        *self = Skips::default();
        assert_eq!(self.count, 0);
        assert!(self.first.is_none());
    }

    /// Why the first skip since the last reset happened. Output: its reason, or none.
    pub fn first(&self) -> Option<&'static str> {
        assert_eq!(self.first.is_some(), self.count > 0);
        self.first
    }
}

/// The fixed reason of a draw error. Input: the error. Output: its reason in
/// `SKIP_REASONS`, or `FRAME_REFUSED` for any other text.
pub fn skip_reason(code: &str) -> &'static str {
    let reason = SKIP_REASONS
        .into_iter()
        .find(|reason| *reason == code)
        .unwrap_or(FRAME_REFUSED);
    assert!(!reason.is_empty());
    reason
}

/// The views whose cover(true) the host refused since their last hide was taken or
/// cleared, so each hide's refusal is told once; at most `REFUSALS_MAX`, oldest first out.
#[derive(Debug, Default)]
pub struct Refusals {
    views: VecDeque<u64>,
}

impl Refusals {
    /// No refusals held, for a thread-local start. Output: the empty set.
    pub const fn new() -> Self {
        Refusals {
            views: VecDeque::new(),
        }
    }

    /// Notes a refused cover(true). Input: the view id. Output: whether it is the first
    /// since the view was last cleared, which the caller tells.
    pub fn refused(&mut self, page: u64) -> bool {
        assert!(page > 0, "a view id is never 0");
        if self.views.contains(&page) {
            return false;
        }
        if self.views.len() == REFUSALS_MAX {
            self.views.pop_front();
        }
        self.views.push_back(page);
        assert!(self.views.len() <= REFUSALS_MAX);
        true
    }

    /// Forgets a view's refusal: its cover was taken, it is shown again or closed.
    /// Input: the view id. Output: none.
    pub fn cleared(&mut self, page: u64) {
        self.views.retain(|held| *held != page);
        assert!(!self.views.contains(&page));
    }
}
