use super::{BridgeError, IntentReceipt, NativeBridge, bounded, caused, failure};
use crazyagent::engine::owner::{OwnerMutation, OwnerRead};
use crazyagent::notifications::{NoticeAction, NoticePage, NoticeQuery, NoticeSettings};

/// The highest draft generation the window may send, the largest integer JavaScript holds
/// exactly.
const GENERATION_MAX: u64 = 9_007_199_254_740_991;

/// Maps an engine refusal of a notification read: a refused query as itself, anything else
/// as a failed owner read with the engine code behind it. Input: the engine code. Output:
/// the bridge error.
pub(crate) fn read_failure(code: &str) -> BridgeError {
    if code == "notification_query_invalid" {
        return failure("notification_query_invalid");
    }
    caused("owner_read_failed", code)
}

impl NativeBridge {
    /// Reads one page of the notification store for the bell, the panel and the project's
    /// Needs you view (DEC-136a). Input: the query. Output: the page with both counts, or
    /// `notification_query_invalid` for a limit or bound the engine refuses.
    pub async fn notifications(&self, query: NoticeQuery) -> Result<NoticePage, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        let page = self
            .reader()?
            .notifications(query.clone())
            .await
            .map_err(|error| read_failure(&error.0))?;
        assert!(page.items.len() <= usize::from(query.limit) + 1);
        bounded(page)
    }

    /// Reads the notification settings: each kind's delivery, the Done retention and the
    /// macOS notifications choice. Input: none. Output: the settings, or the defaults before
    /// the first save.
    pub async fn notification_settings(&self) -> Result<NoticeSettings, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        let settings = self
            .reader()?
            .notification_settings()
            .await
            .map_err(|error| read_failure(&error.0))?;
        assert!(!settings.kinds.is_empty());
        bounded(settings)
    }

    /// Sends one change to the notification store as a retained owner intent: read, unread,
    /// delete, a fold change, read all, saved settings or a client report. Inputs: the change
    /// and a fresh draft generation. Output: the intent receipt; the engine refuses a change
    /// out of bounds in the receipt.
    pub async fn notification_action(
        &mut self,
        action: NoticeAction,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.allow_new_auth().await?;
        if self.blocked() {
            return Err(failure("pending_intent"));
        }
        if generation == 0 || generation > GENERATION_MAX {
            return Err(failure("draft_generation_limit"));
        }
        self.controller()?
            .read(OwnerRead::NotificationSettings)
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))?;
        self.controller()?
            .begin_mutation(OwnerMutation::Notifications { action }, generation)
            .map_err(|error| caused("owner_not_ready", &error.0))?;
        assert!(self.status().pending);
        self.deliver_intent(false).await
    }
}
