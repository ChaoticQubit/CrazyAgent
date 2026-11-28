use super::error;
use crate::config::EffectiveConfig;
use crate::engine::EngineError;
use crate::engine::owner::{Admission, OwnerPageCursor, OwnerView};
use crate::protocol::Cursor;

/// The cursor to an owner page's next page. Input: the config, admission, view and
/// revision the page was read at, whether more rows follow, and the last id read. Output:
/// the cursor past that id when more follow, none otherwise, or `owner_page_progress` when
/// more follow but no id was read.
pub(super) fn page_next(
    config: &EffectiveConfig,
    a: &Admission,
    view: OwnerView,
    revision: u64,
    more: bool,
    last: Option<String>,
) -> Result<Option<OwnerPageCursor>, EngineError> {
    if !more {
        return Ok(None);
    }
    Ok(Some(OwnerPageCursor {
        db_instance: a.db_instance.clone(),
        owner_id: a.owner_id.clone(),
        engine_epoch: a.engine_epoch.clone(),
        owner_epoch: a.owner_epoch,
        config_id: config.id.clone(),
        at_revision: Cursor::new(revision),
        view,
        after_id: last.ok_or_else(|| error("owner_page_progress"))?,
    }))
}

#[cfg(all(test, target_os = "macos"))]
mod perf_tests;
