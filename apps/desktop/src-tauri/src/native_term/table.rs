use super::pane::Pane;
use crazyagent::terminal::DESKTOP_VIEWS_MAX;
use std::collections::{HashMap, VecDeque};

/// The most native views open at once across all windows, the engine's own desktop cap.
pub const VIEWS_MAX: usize = DESKTOP_VIEWS_MAX;

/// The most view events held while the host is busy further up the main thread's stack.
pub const OUTBOX_MAX: usize = 64;

/// What a view last told its panel, so `mouse` and `behind` go out only on a change.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Told {
    pub mouse: bool,
    pub behind: bool,
}

/// One open native view: the window that opened it and the agent window it shows, its pane
/// mirror, the engine feed view it reads now, whether the direct path draws it, its page
/// place in CSS pixels, whether a page overlay covers it, what it last told its panel, the
/// wheel's carried fraction and its platform parts.
pub struct Entry<P> {
    pub window: String,
    pub name: String,
    pub pane: Pane,
    pub feed: u64,
    pub direct: bool,
    pub rect: [f64; 4],
    pub covered: bool,
    pub told: Told,
    pub carry: f64,
    pub platform: P,
}

/// Every open native view by the id its page holds, and the engine feed view each reads.
/// One counter gives page and feed ids alike, from 1, never reused, so a late event of a
/// feed a view left can never reach another view.
pub struct Table<P> {
    views: HashMap<u64, Entry<P>>,
    feeds: HashMap<u64, u64>,
    next: u64,
}

impl<P> Default for Table<P> {
    fn default() -> Self {
        Table {
            views: HashMap::new(),
            feeds: HashMap::new(),
            next: 1,
        }
    }
}

impl<P> Table<P> {
    /// Whether no view may open now. Output: the answer.
    pub fn full(&self) -> bool {
        self.views.len() >= VIEWS_MAX
    }

    /// Whether no view is open. Output: the answer.
    pub fn is_empty(&self) -> bool {
        self.views.is_empty()
    }

    /// Takes the next id. Output: an id never handed out before.
    pub fn fresh(&mut self) -> u64 {
        let id = self.next;
        self.next = self.next.checked_add(1).expect("ids never run out");
        assert!(id > 0 && self.next > id);
        id
    }

    /// Adds a view under its page id, reading the feed view its entry names. Inputs: the
    /// page id and the entry.
    pub fn insert(&mut self, page: u64, entry: Entry<P>) {
        assert!(!self.full(), "the host checks the cap first");
        assert!(page > 0 && page < self.next && entry.feed < self.next);
        assert!(!self.views.contains_key(&page) && !self.feeds.contains_key(&entry.feed));
        self.feeds.insert(entry.feed, page);
        self.views.insert(page, entry);
        assert_eq!(self.views.len(), self.feeds.len());
    }

    /// A view a window may act on: one it opened. Another window's view reads as unknown,
    /// so a page learns nothing of views it does not own. Inputs: the window label and the
    /// page id. Output: the entry, or `native_terminal_view_unknown`.
    pub fn owned(&mut self, window: &str, page: u64) -> Result<&mut Entry<P>, &'static str> {
        assert!(!window.is_empty());
        let entry = self
            .views
            .get_mut(&page)
            .filter(|entry| entry.window == window)
            .ok_or("native_terminal_view_unknown")?;
        assert_eq!(entry.window, window);
        Ok(entry)
    }

    /// A view by page id. Input: the page id. Output: the entry, if open.
    pub fn get_mut(&mut self, page: u64) -> Option<&mut Entry<P>> {
        self.views.get_mut(&page)
    }

    /// A view by page id, to read. Input: the page id. Output: the entry, if open.
    pub fn get(&self, page: u64) -> Option<&Entry<P>> {
        self.views.get(&page)
    }

    /// The page id of the view reading a feed view now. Input: the feed id. Output: the
    /// page id, or none for a feed no view reads any more.
    pub fn page_of(&self, feed: u64) -> Option<u64> {
        let page = self.feeds.get(&feed).copied();
        assert!(page.is_none_or(|page| self.views.get(&page).is_some_and(|e| e.feed == feed)));
        page
    }

    /// Moves a view to a fresh feed id, engine-fed from now on, when its direct path is
    /// refused. Input: the page id. Output: (old feed id, new feed id), or none.
    pub fn refeed(&mut self, page: u64) -> Option<(u64, u64)> {
        let old = self.views.get(&page)?.feed;
        let new = self.fresh();
        self.feeds.remove(&old);
        self.feeds.insert(new, page);
        let entry = self.views.get_mut(&page)?;
        entry.feed = new;
        entry.direct = false;
        assert_eq!(self.feeds.get(&new), Some(&page));
        assert!(!self.feeds.contains_key(&old));
        Some((old, new))
    }

    /// Takes a view out. Input: the page id. Output: the entry, if it was open.
    pub fn remove(&mut self, page: u64) -> Option<Entry<P>> {
        let entry = self.views.remove(&page)?;
        self.feeds.remove(&entry.feed);
        assert!(!self.feeds.values().any(|p| *p == page));
        assert_eq!(self.views.len(), self.feeds.len());
        Some(entry)
    }

    /// The page ids of every open view. Output: the ids.
    pub fn pages(&self) -> Vec<u64> {
        self.views.keys().copied().collect()
    }

    /// Every open view, to draw or restyle. Output: the entries.
    pub fn entries_mut(&mut self) -> impl Iterator<Item = (&u64, &mut Entry<P>)> {
        self.views.iter_mut()
    }
}

/// View events AppKit sent while the host was busy further up the main thread's stack, kept
/// in order for the outermost host call to apply as it ends, and the views whose event
/// found it full, which the host ends with `NATIVE_OVERFLOW`.
pub struct Outbox<T> {
    queue: VecDeque<(u64, T)>,
    over: Vec<u64>,
}

impl<T> Default for Outbox<T> {
    fn default() -> Self {
        Outbox {
            queue: VecDeque::new(),
            over: Vec::new(),
        }
    }
}

impl<T> Outbox<T> {
    /// Keeps one event. Inputs: the page id and the event. Output: whether it was kept; a
    /// full outbox keeps nothing and marks the view overflowed.
    pub fn push(&mut self, page: u64, event: T) -> bool {
        assert!(page > 0);
        if self.queue.len() >= OUTBOX_MAX {
            if !self.over.contains(&page) && self.over.len() < VIEWS_MAX {
                self.over.push(page);
            }
            return false;
        }
        self.queue.push_back((page, event));
        assert!(self.queue.len() <= OUTBOX_MAX);
        true
    }

    /// Takes every kept event, oldest first, and the overflowed views. Output: both.
    pub fn take(&mut self) -> (Vec<(u64, T)>, Vec<u64>) {
        let taken: Vec<(u64, T)> = self.queue.drain(..).collect();
        let over = std::mem::take(&mut self.over);
        assert!(self.queue.is_empty() && self.over.is_empty());
        assert!(over.len() <= VIEWS_MAX);
        (taken, over)
    }
}

/// What a view must tell its panel after a change: mouse reporting turned on or off, or the
/// view scrolled off or back to the bottom. Inputs: what it told last and what holds now.
/// Output: (mouse, behind), each only when it changed.
pub fn changes(told: Told, now: Told) -> (Option<bool>, Option<bool>) {
    let mouse = (told.mouse != now.mouse).then_some(now.mouse);
    let behind = (told.behind != now.behind).then_some(now.behind);
    assert!(mouse.is_none() || told.mouse != now.mouse);
    assert!(behind.is_none() || told.behind != now.behind);
    (mouse, behind)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(window: &str, feed: u64) -> Entry<()> {
        Entry {
            window: window.into(),
            name: "team-1".into(),
            pane: Pane::new(feed, true),
            feed,
            direct: true,
            rect: [0.0, 0.0, 100.0, 50.0],
            covered: false,
            told: Told::default(),
            carry: 0.0,
            platform: (),
        }
    }

    #[test]
    fn another_windows_view_reads_as_unknown() {
        let mut table = Table::default();
        let page = table.fresh();
        table.insert(page, entry("main", page));
        assert!(table.owned("main", page).is_ok());
        assert_eq!(
            table.owned("wall-1", page).err(),
            Some("native_terminal_view_unknown")
        );
        assert_eq!(
            table.owned("main", page + 1).err(),
            Some("native_terminal_view_unknown")
        );
    }

    #[test]
    fn a_refed_view_keeps_its_page_id_and_drops_its_old_feed() {
        let mut table = Table::default();
        let page = table.fresh();
        table.insert(page, entry("main", page));
        let (old, new) = table.refeed(page).expect("open view");
        assert_eq!(old, page);
        assert!(new > old);
        assert_eq!(table.page_of(old), None);
        assert_eq!(table.page_of(new), Some(page));
        let view = table.owned("main", page).expect("same page id");
        assert!(!view.direct);
        let next = table.fresh();
        assert!(next > new, "a feed id is never reused");
        assert!(table.remove(page).is_some());
        assert_eq!(table.page_of(new), None);
        assert!(table.is_empty());
    }

    #[test]
    fn the_cap_holds_and_ids_are_never_reused() {
        let mut table = Table::default();
        let mut seen = std::collections::HashSet::new();
        while !table.full() {
            let page = table.fresh();
            assert!(seen.insert(page));
            table.insert(page, entry("main", page));
        }
        assert_eq!(table.pages().len(), VIEWS_MAX);
        let first = *seen.iter().min().expect("ids");
        assert!(table.remove(first).is_some());
        assert!(!table.full());
        assert!(!seen.contains(&table.fresh()));
    }

    #[test]
    fn a_full_outbox_keeps_nothing_more_and_keeps_order() {
        let mut outbox = Outbox::default();
        for at in 0..OUTBOX_MAX {
            assert!(outbox.push(1, at));
        }
        assert!(!outbox.push(2, OUTBOX_MAX));
        assert!(!outbox.push(2, OUTBOX_MAX + 1));
        let (taken, over) = outbox.take();
        assert_eq!(taken.len(), OUTBOX_MAX);
        assert!(
            taken
                .iter()
                .enumerate()
                .all(|(at, (page, n))| *page == 1 && *n == at)
        );
        assert_eq!(over, vec![2]);
        let (none, clear) = outbox.take();
        assert!(none.is_empty() && clear.is_empty());
    }

    #[test]
    fn mouse_and_behind_are_told_only_on_a_change() {
        let before = Told::default();
        assert_eq!(changes(before, before), (None, None));
        let now = Told {
            mouse: true,
            behind: false,
        };
        assert_eq!(changes(before, now), (Some(true), None));
        let back = Told {
            mouse: true,
            behind: true,
        };
        assert_eq!(changes(now, back), (None, Some(true)));
    }
}
