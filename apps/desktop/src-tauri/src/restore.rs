use crazyagent::restore::RestoreState;
pub use crazyagent::restore::service_running;
use std::path::Path;
use std::time::SystemTime;

/// What Settings shows about bringing back the 2.4 data (L5): until when the pinned backup
/// can be restored, in milliseconds since 1970, and how the last restore ended. Empty when
/// there is nothing to restore and no restore was asked.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct RestoreView {
    pub until_ms: Option<u64>,
    pub last: Option<RestoreState>,
}

/// Reads the view for the engine at `root` without the engine, from its backups folder and
/// its restore record. Input: the engine root. Output: the view.
pub fn view(root: &Path) -> RestoreView {
    assert!(root.is_absolute());
    let until_ms = crazyagent::restore::restorable_until(root).and_then(epoch_ms);
    let view = RestoreView {
        until_ms,
        last: crazyagent::restore::state(root),
    };
    assert!(view.until_ms.is_none_or(|until| until > 0));
    view
}

/// A time as milliseconds since 1970. Input: the time. Output: the count, or none before
/// 1970 or past what a window number holds exactly.
fn epoch_ms(time: SystemTime) -> Option<u64> {
    let ms = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_millis();
    u64::try_from(ms)
        .ok()
        .filter(|ms| *ms > 0 && *ms <= 9_007_199_254_740_991)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_deadline_reaches_the_window_as_exact_milliseconds_and_nothing_else_does() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_millis(1_800_000_000_123);
        assert_eq!(epoch_ms(at), Some(1_800_000_000_123));
        assert_eq!(epoch_ms(SystemTime::UNIX_EPOCH), None);
        let before = SystemTime::UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(epoch_ms(before), None);
        let far = SystemTime::UNIX_EPOCH + Duration::from_millis(9_007_199_254_740_992);
        assert_eq!(epoch_ms(far), None);
    }

    #[test]
    fn an_engine_folder_with_no_backup_and_no_record_shows_nothing_to_restore() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        assert_eq!(view(&root), RestoreView::default());
        let shown = serde_json::to_value(view(&root)).unwrap();
        assert_eq!(shown, serde_json::json!({ "until_ms": null, "last": null }));
    }
}
