use crate::notifications::{Notice, NoticeFilter, NoticeKind, NoticeQuery, NoticeTarget, PAGE_MAX};
use crate::protocol::{CredentialRef, ProjectId};
use crate::tui::client::notifications::{NoticesEvent, NoticesQuery, NoticesReply};
use crate::tui::ctx::Load;

/// How long one read of what needs the owner stands before the Overview reads it again.
pub(in crate::tui) const POLL_MS: i64 = 5_000;
/// The most Needs you rows the Overview lists; the rest wait in the notifications list.
pub(in crate::tui) const SHOWN_MAX: usize = 5;

/// One project's Needs you on its Overview (DEC-136a decision 4): the store's pending items
/// of the project, folded, joined with the pending sign-ins of the accounts in its pool,
/// newest first. Reads both every `POLL_MS` while the Overview shows; never invents items.
/// Input: replies through `arrive` and the pool's accounts. Output: the rows to list.
pub(in crate::tui) struct NeedsYou {
    project: ProjectId,
    own: Load<Vec<Notice>>,
    sign_ins: Vec<Notice>,
    accounts: Vec<CredentialRef>,
    waiting: Vec<NoticesQuery>,
    read_ms: Option<i64>,
}

impl NeedsYou {
    /// Nothing read yet for one project. Input: the project. Output: the view.
    pub(in crate::tui) fn new(project: ProjectId) -> Self {
        Self {
            project,
            own: Load::Idle,
            sign_ins: Vec::new(),
            accounts: Vec::new(),
            waiting: Vec::new(),
            read_ms: None,
        }
    }

    /// The read of the project's own pending items, folded. Input: the project. Output: the
    /// query.
    pub(in crate::tui) fn own_query(project: &ProjectId) -> NoticesQuery {
        NoticesQuery::Page(NoticeQuery {
            filter: NoticeFilter::Pending,
            project_id: Some(project.clone()),
            kind: None,
            fold: true,
            before: None,
            after: None,
            limit: PAGE_MAX,
        })
    }

    /// The read of every pending sign-in, unfolded, since a sign-in carries no project and
    /// each account is its own item. Input: none. Output: the query.
    pub(in crate::tui) fn sign_ins_query() -> NoticesQuery {
        NoticesQuery::Page(NoticeQuery {
            filter: NoticeFilter::Pending,
            project_id: None,
            kind: Some(NoticeKind::AccountSignIn),
            fold: false,
            before: None,
            after: None,
            limit: PAGE_MAX,
        })
    }

    /// Keeps the accounts of the project's pool, whose sign-ins the rows show. Input: the
    /// account ids. Output: none.
    pub(in crate::tui) fn set_accounts(&mut self, accounts: Vec<CredentialRef>) {
        self.accounts = accounts;
    }

    /// Reads again on the next `due`, as when the Overview opens again. Input: none.
    pub(in crate::tui) fn refresh(&mut self) {
        self.read_ms = None;
    }

    /// The reads to send now: both once `POLL_MS` passed since the last ones and neither
    /// waits for its reply. Input: now in ms. Output: none, or both queries.
    pub(in crate::tui) fn due(&mut self, now_ms: i64) -> Vec<NoticesQuery> {
        let fresh = self
            .read_ms
            .is_some_and(|read| now_ms.saturating_sub(read) < POLL_MS);
        if !self.waiting.is_empty() || fresh {
            return Vec::new();
        }
        self.read_ms = Some(now_ms);
        self.waiting = vec![Self::own_query(&self.project), Self::sign_ins_query()];
        self.waiting.clone()
    }

    /// Takes the reply to one of its reads; any other reply is left alone. A failed
    /// project read shows only before one landed; a failed sign-in read keeps the last.
    /// Input: the event. Output: none.
    pub(in crate::tui) fn arrive(&mut self, event: &NoticesEvent) {
        let Some(at) = self.waiting.iter().position(|query| *query == event.query) else {
            return;
        };
        self.waiting.swap_remove(at);
        let own = event.query == Self::own_query(&self.project);
        match (&event.result, own) {
            (Ok(NoticesReply::Page(page)), true) => self.own = Load::Ready(page.items.clone()),
            (Ok(NoticesReply::Page(page)), false) => self.sign_ins = page.items.clone(),
            (Err(code), true) if self.own.ready().is_none() => {
                self.own = Load::Failed(code.clone());
            }
            _ => {}
        }
        assert!(self.waiting.len() <= 2);
    }

    /// The refusal of the project's read while none landed, for the Overview to show.
    /// Input: none. Output: the code, or None.
    pub(in crate::tui) fn problem(&self) -> Option<&str> {
        match &self.own {
            Load::Failed(code) => Some(code),
            _ => None,
        }
    }

    /// The rows: the project's own pending items and the pending sign-ins of its pool's
    /// accounts, newest first; none until the project's read landed. Input: none.
    /// Output: the rows.
    pub(in crate::tui) fn rows(&self) -> Vec<Notice> {
        let Some(own) = self.own.ready() else {
            return Vec::new();
        };
        let pooled = self.sign_ins.iter().filter(|item| {
            matches!(&item.target, NoticeTarget::AccountSignIn { account } if self.accounts.contains(account))
        });
        let mut rows: Vec<Notice> = pooled.chain(own.iter()).cloned().collect();
        rows.sort_by_key(|row| std::cmp::Reverse(row.id));
        rows.dedup_by_key(|row| row.id);
        assert!(rows.len() <= own.len() + self.sign_ins.len());
        rows
    }

    /// Whether a row asks for more than the project's pool, which the unassigned Overview's
    /// pool picker answers by itself. Input: none. Output: true when any row targets
    /// something other than the project's pool.
    pub(in crate::tui) fn beyond_pool(&self) -> bool {
        self.rows()
            .iter()
            .any(|row| !matches!(row.target, NoticeTarget::ProjectPool { .. }))
    }

    /// The rows the Overview lists, at most `SHOWN_MAX`. Input: none. Output: the rows.
    pub(in crate::tui) fn shown(&self) -> Vec<Notice> {
        let mut rows = self.rows();
        rows.truncate(SHOWN_MAX);
        rows
    }
}

#[cfg(all(test, unix))]
mod tests;
