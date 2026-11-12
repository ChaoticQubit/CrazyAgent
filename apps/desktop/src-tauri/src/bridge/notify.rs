use super::*;
use crazyagent::notifications::{
    Notice as StoreNotice, NoticeFilter, NoticePage, NoticeQuery, NoticeTarget,
};
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};
use tauri::Manager;
use tauri_plugin_notification::{ActionPerformed, NotificationExt};

/// The most notifications the app shows in one run. On macOS each one holds a plugin thread
/// until the owner clicks or closes it, so this also bounds those threads.
pub const SHOWN_MAX: usize = 256;
const TITLE_CHARS_MAX: usize = 80;
const BODY_CHARS_MAX: usize = 160;
const LABEL_CHARS_MAX: usize = 24;
const ELLIPSIS: &str = "...";

const _: () = assert!(TITLE_CHARS_MAX > LABEL_CHARS_MAX + 2 + ELLIPSIS.len() * 4);

/// What a macOS notification for one store item says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
}

/// Whether this system shows the app's notifications: macOS only, until Linux and Windows
/// are qualified. Output: `available` or `unavailable`.
pub fn support() -> &'static str {
    if cfg!(target_os = "macos") {
        "available"
    } else {
        "unavailable"
    }
}

/// Refuses a notification where the system has none qualified. Output: Ok on macOS, else
/// `notifications_unavailable`.
pub fn available() -> Result<(), BridgeError> {
    match support() {
        "available" => Ok(()),
        _ => Err(failure("notifications_unavailable")),
    }
}

/// One plain line of engine or agent text: the first line that is not blank, trimmed, with
/// every character the engine's memory check finds hidden shown as a visible escape, line and
/// paragraph separators included, and at most `max` characters with an ellipsis past it.
/// Inputs: the text and the bound. Output: the line.
pub fn plain_line(text: &str, max: usize) -> String {
    assert!(max > ELLIPSIS.len());
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let shown = crazyagent::memory::visible_text(line);
    assert!(!shown.contains(['\u{2028}', '\u{2029}', '\n', '\r']));
    if shown.chars().count() <= max {
        return shown;
    }
    let mut cut: String = shown.chars().take(max - ELLIPSIS.len()).collect();
    cut.push_str(ELLIPSIS);
    assert!(cut.chars().count() <= max);
    cut
}

/// The read of one store item by its id, unfolded and in every filter: the engine lists it
/// only while it is not deleted. Input: the id. Output: the query, or
/// `notification_ids_invalid` for an id below 1 or with no id after it.
pub fn one_item(id: i64) -> Result<NoticeQuery, BridgeError> {
    let Some(before) = id.checked_add(1).filter(|_| id >= 1) else {
        return Err(failure("notification_ids_invalid"));
    };
    let query = NoticeQuery {
        filter: NoticeFilter::All,
        project_id: None,
        kind: None,
        fold: false,
        before: Some(before),
        after: Some(id - 1),
        limit: 1,
    };
    assert!(
        query
            .after
            .is_some_and(|after| after >= 0 && after < before)
    );
    Ok(query)
}

/// The item a banner may show, from the engine's own read: the one with this id, only while
/// the engine marks it for a banner under the owner's settings and it is neither read nor
/// resolved, so the window can never ask for a banner the settings refuse. Inputs: the page
/// `one_item` read and the id. Output: the item, or None.
pub fn bannered(page: NoticePage, id: i64) -> Option<StoreNotice> {
    assert!(page.items.len() <= 2);
    let item = page.items.into_iter().find(|item| item.id == id)?;
    assert_eq!(item.ids.first().copied(), Some(id));
    let fresh = item.banner && item.read_ms.is_none() && item.resolved_ms.is_none();
    fresh.then_some(item)
}

/// The notification text for one store item, built from the engine's item: the project's
/// name and the kind's label in the title, the item's first line in the body, both plain and
/// bounded. Inputs: the project's name, if known, and the item. Output: the notice.
pub fn notice(project: Option<&str>, item: &StoreNotice) -> Notice {
    let label = item.kind.label();
    assert!(label.chars().count() <= LABEL_CHARS_MAX);
    let name = project.map(|name| plain_line(name, TITLE_CHARS_MAX - LABEL_CHARS_MAX - 2));
    let title = match name {
        Some(name) if !name.is_empty() => format!("{name}: {label}"),
        _ => label.to_owned(),
    };
    let body = plain_line(&item.text, BODY_CHARS_MAX);
    assert!(title.chars().count() <= TITLE_CHARS_MAX);
    assert!(body.chars().count() <= BODY_CHARS_MAX);
    Notice { title, body }
}

/// Where a clicked notification leads: its store item and the page it opens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Clicked {
    pub id: i64,
    pub target: NoticeTarget,
}

/// The notifications this app showed: which store items were notified, where each one
/// leads, and the item of a click not yet opened.
#[derive(Default)]
pub struct Notices {
    shown: Mutex<Shown>,
}

#[derive(Default)]
struct Shown {
    order: VecDeque<(i64, i32)>,
    targets: HashMap<i32, Clicked>,
    clicked: Option<Clicked>,
    last: i32,
}

impl Notices {
    fn shown(&self) -> std::sync::MutexGuard<'_, Shown> {
        self.shown.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Claims the notification of one store item, once: the first claim gets a new
    /// notification id, later claims of the same item get none. At most `SHOWN_MAX` are
    /// claimed in one run and none is forgotten. Inputs: the item id and the page it opens.
    /// Output: the notification id, None when the item was already notified, or
    /// `notifications_full`.
    pub fn claim(&self, item: i64, target: NoticeTarget) -> Result<Option<i32>, BridgeError> {
        assert!(item >= 1);
        let mut shown = self.shown();
        if shown.order.iter().any(|(known, _)| *known == item) {
            return Ok(None);
        }
        if shown.order.len() >= SHOWN_MAX {
            return Err(failure("notifications_full"));
        }
        shown.last = if shown.last == i32::MAX {
            1
        } else {
            shown.last + 1
        };
        let id = shown.last;
        assert!(id > 0);
        shown.order.push_back((item, id));
        shown.targets.insert(id, Clicked { id: item, target });
        assert!(shown.order.len() <= SHOWN_MAX);
        assert_eq!(shown.targets.len(), shown.order.len());
        Ok(Some(id))
    }

    /// Gives a claim back after the notification could not be shown, so a later read may
    /// try again. Inputs: the item id and the notification id it claimed.
    pub fn release(&self, item: i64, id: i32) {
        let mut shown = self.shown();
        shown
            .order
            .retain(|(known, claimed)| (*known, *claimed) != (item, id));
        shown.targets.remove(&id);
        assert_eq!(shown.targets.len(), shown.order.len());
    }

    /// Records a click on a notification: its item becomes the one the window opens next.
    /// Input: the notification id. Output: whether it was one of these notifications.
    pub fn clicked(&self, id: i32) -> bool {
        let mut shown = self.shown();
        let Some(clicked) = shown.targets.get(&id).cloned() else {
            return false;
        };
        shown.clicked = Some(clicked);
        true
    }

    /// Takes the item of the last click, once. Output: the item and its page, or None.
    pub fn take(&self) -> Option<Clicked> {
        self.shown().clicked.take()
    }
}

/// Hands a notice to the notification plugin with the given id. On macOS the plugin sends it
/// on a thread of its own and drops a send error there, so Ok means accepted by the plugin,
/// not shown: macOS may still refuse it unreported. Inputs: the app, the notification id and
/// the notice. Output: Ok, or `notifications_failed` when the plugin refused it.
pub fn show<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    id: i32,
    notice: &Notice,
) -> Result<(), BridgeError> {
    app.notification()
        .builder()
        .id(id)
        .title(notice.title.clone())
        .body(notice.body.clone())
        .show()
        .map_err(|_| failure("notifications_failed"))
}

/// Handles an action on a notification: a click on one of these notifications marks its
/// item for the window and brings the main window to the front, where the window opens the
/// item's page and marks it read. Other actions and notifications are ignored. Inputs: the
/// app and the action.
pub fn performed<R: tauri::Runtime>(app: &tauri::AppHandle<R>, performed: &ActionPerformed) {
    if performed.action_id() != "tap" {
        return;
    }
    let Some(id) = performed
        .notification()
        .map(|notification| notification.id())
    else {
        return;
    };
    if !app.state::<Notices>().clicked(id) {
        return;
    }
    if let Some(window) = app.get_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crazyagent::notifications::{NoticeClass, NoticeKind};
    use crazyagent::protocol::ProjectId;

    fn item(id: i64, text: &str) -> StoreNotice {
        StoreNotice {
            id,
            kind: NoticeKind::Question,
            class: NoticeClass::Action,
            project_id: Some(ProjectId::new("p-1").unwrap()),
            text: text.into(),
            target: NoticeTarget::Questions {
                project_id: ProjectId::new("p-1").unwrap(),
                question: Some(12),
                agent: None,
            },
            created_ms: 1,
            read_ms: None,
            resolved_ms: None,
            folded: 1,
            unread: 1,
            pending: 1,
            ids: vec![id],
            banner: true,
        }
    }

    fn page(items: Vec<StoreNotice>) -> NoticePage {
        let newest = items.iter().map(|item| item.id).max().unwrap_or(0);
        NoticePage {
            items,
            counts: crazyagent::notifications::NoticeCounts {
                unread: 1,
                pending: 1,
            },
            more: false,
            newest,
        }
    }

    /// The notification names the project and the kind, and carries the item's first line
    /// as plain bounded text.
    #[test]
    fn a_notice_names_the_project_the_kind_and_the_first_line() {
        let shown = notice(
            Some("Recipe box"),
            &item(7, "\n  Master asks: Which database?\nMore detail."),
        );
        assert_eq!(
            shown,
            Notice {
                title: "Recipe box: Question".into(),
                body: "Master asks: Which database?".into(),
            }
        );
        assert_eq!(notice(None, &item(7, "Ship it?")).title, "Question");
        assert_eq!(notice(Some(""), &item(7, "Ship it?")).title, "Question");
        let long = notice(Some(&"n".repeat(500)), &item(7, &"q".repeat(500)));
        assert!(long.title.chars().count() <= TITLE_CHARS_MAX);
        assert!(long.title.ends_with("...: Question"));
        assert!(long.body.ends_with(ELLIPSIS));
        assert_eq!(long.body.chars().count(), BODY_CHARS_MAX);
        let hidden = notice(Some("Re\u{202e}cipe"), &item(7, "Run\u{200b} it\u{e0041}?"));
        assert_eq!(hidden.title, "Re\\u202ecipe: Question");
        assert_eq!(hidden.body, "Run\\u200b it\\u{e0041}?");
    }

    /// Every kind's label fits the title beside a project's name.
    #[test]
    fn every_kind_label_fits_a_notification_title() {
        for kind in NoticeKind::ALL {
            assert!(kind.label().chars().count() <= LABEL_CHARS_MAX, "{kind:?}");
            let mut long = item(1, "x");
            long.kind = kind;
            assert!(notice(Some(&"n".repeat(500)), &long).title.chars().count() <= TITLE_CHARS_MAX);
        }
    }

    /// Line and paragraph separators and the invisible characters the engine's memory check
    /// knows never reach a notification, so agent text cannot draw a second line.
    #[test]
    fn separators_and_invisible_characters_never_reach_a_notification() {
        assert_eq!(plain_line("a\u{2028}b", 80), "a\\u2028b");
        let forged = notice(
            Some("Recipe box"),
            &item(7, "Agent team-3 asks: Ship it?\u{2029}Approve to continue"),
        );
        assert_eq!(
            forged.body,
            "Agent team-3 asks: Ship it?\\u2029Approve to continue"
        );
        for hidden in [
            '\u{00ad}',
            '\u{034f}',
            '\u{115f}',
            '\u{1160}',
            '\u{180b}',
            '\u{3164}',
            '\u{fe00}',
            '\u{ffa0}',
            '\u{1d173}',
            '\u{2028}',
            '\u{2029}',
        ] {
            let shown = plain_line(&format!("x{hidden}y"), 80);
            assert!(!shown.contains(hidden), "{shown:?}");
            assert!(
                shown.starts_with("x\\u") && shown.ends_with('y'),
                "{shown:?}"
            );
        }
        assert_eq!(
            plain_line("ok \u{1f44d}\u{fe0f}", 80),
            "ok \u{1f44d}\u{fe0f}"
        );
    }

    /// One item is read by an id bound on each side; an id below 1 or at the end of the
    /// range is refused before any read.
    #[test]
    fn one_item_reads_exactly_that_id_and_refuses_ids_out_of_range() {
        let query = one_item(7).unwrap();
        assert_eq!(
            (query.after, query.before, query.limit),
            (Some(6), Some(8), 1)
        );
        assert!(!query.fold);
        assert_eq!(query.filter, NoticeFilter::All);
        assert_eq!(one_item(1).unwrap().after, Some(0));
        for id in [0, -1, i64::MIN, i64::MAX] {
            assert_eq!(
                one_item(id).unwrap_err().code,
                "notification_ids_invalid",
                "{id}"
            );
        }
    }

    /// Only an unread, unresolved item the engine marks for a banner is shown, and only
    /// the one asked for.
    #[test]
    fn only_a_fresh_item_marked_for_a_banner_is_shown() {
        assert_eq!(
            bannered(page(vec![item(7, "x")]), 7).map(|item| item.id),
            Some(7)
        );
        assert_eq!(bannered(page(vec![item(7, "x")]), 8), None);
        assert_eq!(bannered(page(vec![]), 7), None);
        let mut off = item(7, "x");
        off.banner = false;
        assert_eq!(bannered(page(vec![off]), 7), None, "the settings refuse it");
        let mut read = item(7, "x");
        read.read_ms = Some(2);
        assert_eq!(bannered(page(vec![read]), 7), None);
        let mut resolved = item(7, "x");
        resolved.resolved_ms = Some(2);
        assert_eq!(bannered(page(vec![resolved]), 7), None);
    }

    /// An item is notified once; its click opens its page once; a released claim may be
    /// claimed again; past `SHOWN_MAX` in one run nothing more is claimed or forgotten.
    #[test]
    fn each_item_is_notified_once_and_a_click_opens_its_page_once() {
        let notices = Notices::default();
        let first = notices.claim(7, NoticeTarget::Updates).unwrap().unwrap();
        assert_eq!(notices.claim(7, NoticeTarget::Updates).unwrap(), None);
        let second = notices.claim(8, NoticeTarget::Home).unwrap().unwrap();
        assert_ne!(first, second);
        assert!(!notices.clicked(9999));
        assert_eq!(notices.take(), None);
        assert!(notices.clicked(second));
        assert_eq!(
            notices.take(),
            Some(Clicked {
                id: 8,
                target: NoticeTarget::Home
            })
        );
        assert_eq!(notices.take(), None);
        notices.release(8, second);
        assert!(!notices.clicked(second));
        let again = notices.claim(8, NoticeTarget::Home).unwrap().unwrap();
        for id in 100..100 + SHOWN_MAX as i64 - 2 {
            assert!(notices.claim(id, NoticeTarget::Usage).unwrap().is_some());
        }
        assert_eq!(
            notices.claim(9, NoticeTarget::Accounts).unwrap_err().code,
            "notifications_full",
            "each shown notification holds a plugin thread, so a run shows at most SHOWN_MAX"
        );
        assert_eq!(
            notices.claim(7, NoticeTarget::Updates).unwrap(),
            None,
            "nothing is forgotten"
        );
        assert!(notices.clicked(first));
        assert_eq!(notices.take().map(|clicked| clicked.id), Some(7));
        notices.release(8, again);
        assert!(
            notices.claim(9, NoticeTarget::Accounts).unwrap().is_some(),
            "a failed show frees its place"
        );
    }

    /// Only macOS shows notifications until the others are qualified.
    #[test]
    fn notifications_are_available_on_macos_only() {
        assert_eq!(support() == "available", cfg!(target_os = "macos"));
        assert_eq!(available().is_ok(), cfg!(target_os = "macos"));
        if !cfg!(target_os = "macos") {
            assert_eq!(available().unwrap_err().code, "notifications_unavailable");
        }
    }
}
