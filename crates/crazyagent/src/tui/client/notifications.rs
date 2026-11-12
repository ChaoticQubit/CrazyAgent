use super::settings::{applied, problem};
use super::*;
use crate::notifications::{NoticeAction, NoticeFilter, NoticePage, NoticeQuery, NoticeSettings};

/// One notification request a screen or the shell sends (DEC-136a): read one page of the
/// store, read the settings, or change the store. Runs on the owner worker; the reply
/// arrives as `OwnerEvent::Notices` with this query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum NoticesQuery {
    Page(NoticeQuery),
    Settings,
    Act {
        action: NoticeAction,
        generation: u64,
    },
}

impl NoticesQuery {
    /// The one-row read the shell sends for the header counts: every item of every
    /// project, folded, so the page's counts cover the whole store. Input: none.
    /// Output: the query.
    pub(in crate::tui) fn counts() -> Self {
        Self::Page(NoticeQuery {
            filter: NoticeFilter::All,
            project_id: None,
            kind: None,
            fold: true,
            before: None,
            after: None,
            limit: 1,
        })
    }
}

/// What the engine answered a notification request with: a page, the settings, or that
/// the change took effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum NoticesReply {
    Page(NoticePage),
    Settings(NoticeSettings),
    Done,
}

/// One notification reply for the screens: the query it answers and the engine's answer,
/// or a bounded plain code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct NoticesEvent {
    pub query: NoticesQuery,
    pub result: Result<NoticesReply, String>,
}

impl NoticesEvent {
    /// The reply for a request the worker could not run because the trusted connection is
    /// gone. Takes the query. Returns the event for that query.
    pub(in crate::tui) fn unavailable(query: NoticesQuery) -> Self {
        Self::failed(query, "trusted_connection_unavailable")
    }

    /// The reply for a request that failed before it reached the engine.
    /// Takes the query and the problem code. Returns the event for that query.
    pub(in crate::tui) fn failed(query: NoticesQuery, code: &str) -> Self {
        Self {
            query,
            result: Err(code.to_owned()),
        }
    }
}

impl Connection {
    /// Queues one notification request on the paired owner worker.
    /// Takes the query. Returns an error while another native request is pending.
    pub(in crate::tui) fn notices(&mut self, query: NoticesQuery) -> Result<(), String> {
        self.owner_job(Job::Notices(Box::new(query)))
    }
}

impl NativeOwner {
    /// Runs one notification request through the trusted owner controller.
    /// Takes the query. Returns the event; a transport failure detaches the connection so
    /// the shell reconnects.
    pub(super) async fn notices(&mut self, query: NoticesQuery) -> NoticesEvent {
        let mut lost = false;
        let result = self.notices_reply(query.clone(), &mut lost).await;
        if lost {
            self.detach();
        }
        NoticesEvent { query, result }
    }

    async fn notices_reply(
        &mut self,
        query: NoticesQuery,
        lost: &mut bool,
    ) -> Result<NoticesReply, String> {
        let controller = self.settings_controller()?;
        match query {
            NoticesQuery::Page(query) => controller
                .notifications(query)
                .await
                .map(NoticesReply::Page)
                .map_err(|error| problem(&error, lost)),
            NoticesQuery::Settings => controller
                .notification_settings()
                .await
                .map(NoticesReply::Settings)
                .map_err(|error| problem(&error, lost)),
            NoticesQuery::Act { action, generation } => {
                let reply = controller.notification_action(action, generation).await;
                applied(reply, "notification_action_outcome_unknown", lost)
                    .map(|()| NoticesReply::Done)
            }
        }
    }
}
