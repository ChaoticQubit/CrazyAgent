use super::DEFERRED_MAX;

/// One owner request served past its exchange. While it runs it is counted apart from
/// the client slots, so a long clone or check never refuses other clients, and at most
/// `DEFERRED_MAX` run at once. Input: the server authority's count. Output: the guard, or
/// none when `DEFERRED_MAX` already run; dropping the guard gives its place back.
pub(super) struct Deferral<'a>(&'a std::sync::atomic::AtomicUsize);

impl<'a> Deferral<'a> {
    pub(super) fn take(count: &'a std::sync::atomic::AtomicUsize) -> Option<Self> {
        use std::sync::atomic::Ordering;
        let before = count
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |running| {
                (running < DEFERRED_MAX).then_some(running + 1)
            })
            .ok()?;
        assert!(before < DEFERRED_MAX);
        Some(Self(count))
    }
}

impl Drop for Deferral<'_> {
    fn drop(&mut self) {
        let before = self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        assert!(before > 0 && before <= DEFERRED_MAX);
    }
}
