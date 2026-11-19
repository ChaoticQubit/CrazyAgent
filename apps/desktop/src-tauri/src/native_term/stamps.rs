use std::time::{SystemTime, UNIX_EPOCH};

/// When a native view last turned from hidden to shown, and the first frame it presented
/// after that, in ms since 1970, so a native-webdriver trial times a switch by the host's
/// own stamps (DEC-187), with the draws it skipped since that uncover and the first one's
/// reason. All are none or 0 while the view is hidden.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ShowStamps {
    pub uncovered_ms: Option<f64>,
    pub presented_ms: Option<f64>,
    pub skipped: u32,
    pub skip_first: Option<&'static str>,
}

impl ShowStamps {
    /// Clears both stamps as the view hides. Output: none.
    pub fn hidden(&mut self) {
        *self = ShowStamps::default();
        assert!(self.uncovered_ms.is_none());
        assert!(self.presented_ms.is_none());
        assert_eq!(self.skipped, 0);
    }

    /// Stamps the uncover when the view was hidden until now; a view placed again while
    /// shown keeps both stamps. Inputs: whether it was hidden and the time. Output: none.
    pub fn shown(&mut self, was_hidden: bool, now_ms: f64) {
        assert!(now_ms.is_finite());
        assert!(now_ms > 0.0);
        if was_hidden {
            *self = ShowStamps {
                uncovered_ms: Some(now_ms),
                ..ShowStamps::default()
            };
        }
        assert!(self.uncovered_ms.is_some(), "a shown view was uncovered");
    }

    /// Stamps the first frame presented since the uncover; later frames keep it. Input: the
    /// time. Output: none.
    pub fn presented(&mut self, now_ms: f64) {
        assert!(now_ms.is_finite());
        assert!(self.uncovered_ms.is_some(), "only a shown view presents");
        if self.presented_ms.is_none() {
            self.presented_ms = Some(now_ms);
        }
        assert!(self.presented_ms.is_some());
    }

    /// Counts one draw skipped since the uncover and keeps the first one's reason. Input:
    /// the fixed reason. Output: none.
    pub fn skipped(&mut self, reason: &'static str) {
        assert!(
            self.uncovered_ms.is_some(),
            "only a shown view skips a draw"
        );
        assert!(!reason.is_empty());
        self.skipped = self.skipped.saturating_add(1);
        self.skip_first.get_or_insert(reason);
        assert!(self.skipped >= 1);
        assert!(self.skip_first.is_some());
    }
}

/// The wall clock in ms since 1970, the clock a page's `performance.timeOrigin` counts on.
/// Output: the time.
pub fn now_ms() -> f64 {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock reads after 1970");
    let ms = since.as_secs_f64() * 1000.0;
    assert!(ms.is_finite());
    assert!(ms > 0.0);
    ms
}
