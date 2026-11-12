use super::*;
use crate::notifications::{NoticeAction, NoticePage, NoticeQuery, NoticeSettings};

impl OwnerController {
    /// Reads one page of the notification store (DEC-136a): items by filter, project, kind
    /// and folding, with the unread and pending counts. Input: the query. Output: the page,
    /// or `notification_query_invalid` for a limit or bound the engine refuses.
    pub async fn notifications(&mut self, query: NoticeQuery) -> Result<NoticePage, EngineError> {
        crate::notifications::validate_query(&query)?;
        match self.read(OwnerRead::Notifications { query }).await?.data {
            OwnerData::Notifications { page } => Ok(page),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Reads the notification settings: delivery per kind, the Done retention and the
    /// macOS banners choice. Input: none. Output: the stored settings, or the defaults
    /// before the first save.
    pub async fn notification_settings(&mut self) -> Result<NoticeSettings, EngineError> {
        match self.read(OwnerRead::NotificationSettings).await?.data {
            OwnerData::NotificationSettings { settings } => Ok(settings),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Sends one change to the notification store as a retained owner mutation: read,
    /// unread, delete, read all, save settings or a client report. Inputs: the change and
    /// a nonzero client generation. Output: the engine response; a refused change comes back
    /// as a rejected result.
    pub async fn notification_action(
        &mut self,
        action: NoticeAction,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        if self.pending.is_some() || self.pending_account_profile.is_some() {
            return Err(EngineError("controller_pending_intent".into()));
        }
        self.read(OwnerRead::NotificationSettings).await?;
        self.begin_mutation(OwnerMutation::Notifications { action }, generation)?;
        self.send_pending().await
    }
}
