use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::gdk::keys::Key;
use gtk::gdk::keys::constants as key;
use gtk::gdk::{EventKey, ModifierType};
use gtk::glib::{Propagation, SignalHandlerId, WeakRef};
use gtk::prelude::*;

use super::keys;
use crate::native_term::event::{Sink, ViewEvent};
use crate::native_term::pane::Input;

/// The modifiers a chord is read against; Lock, Num Lock, the pointer buttons and the
/// input method's forward bit never change a chord.
const CHORD_MODIFIERS: ModifierType = ModifierType::SHIFT_MASK
    .union(ModifierType::CONTROL_MASK)
    .union(ModifierType::MOD1_MASK)
    .union(ModifierType::MOD4_MASK)
    .union(ModifierType::MOD5_MASK)
    .union(ModifierType::SUPER_MASK)
    .union(ModifierType::HYPER_MASK)
    .union(ModifierType::META_MASK);

/// The largest input method commit a view passes on, in bytes; a larger one is dropped.
pub const COMMIT_BYTES_MAX: usize = 1 << 20;

/// The view command a key chord makes, by the Linux terminal convention: Ctrl+Shift+C
/// copies, Ctrl+Shift+V pastes, Ctrl+Shift+A selects all and Shift+Insert pastes. Inputs:
/// the key and the modifier state. Output: the command, or none for any other key.
pub fn chord(keyval: Key, state: ModifierType) -> Option<ViewEvent> {
    let held = state & CHORD_MODIFIERS;
    let command = if held == ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK {
        match keyval.to_lower() {
            key::c => ViewEvent::Copy,
            key::v => ViewEvent::Paste,
            key::a => ViewEvent::SelectAll,
            _ => return None,
        }
    } else if held == ModifierType::SHIFT_MASK && matches!(keyval, key::Insert | key::KP_Insert) {
        ViewEvent::Paste
    } else {
        return None;
    };
    assert!(matches!(
        command,
        ViewEvent::Copy | ViewEvent::Paste | ViewEvent::SelectAll
    ));
    Some(command)
}

/// The input one input method commit makes: its UTF-8 bytes as one text input, the one
/// path text takes (spike run 3: IBus commits every plain letter). Input: the committed
/// text. Output: the input, or none for empty text or text over `COMMIT_BYTES_MAX` bytes.
pub fn commit_input(text: &str) -> Option<Input> {
    if text.is_empty() || text.len() > COMMIT_BYTES_MAX {
        return None;
    }
    Some(Input::Text(text.as_bytes().to_vec()))
}

/// Where keyboard focus stands for one view: whether it holds native focus, whether its
/// toplevel has keyboard focus and whether a page element covers it. Keys go to the view
/// only while it is active: native, in a focused toplevel and uncovered. A cover drops
/// native focus, which only the page gives back, as on macOS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Focus {
    native: bool,
    toplevel: bool,
    covered: bool,
}

impl Focus {
    /// A new view's focus: not native and uncovered. Input: whether the toplevel has
    /// keyboard focus. Output: the focus.
    pub const fn new(toplevel: bool) -> Focus {
        Focus {
            native: false,
            toplevel,
            covered: false,
        }
    }

    /// Whether keys go to the view. Output: the answer.
    pub fn active(self) -> bool {
        self.native && self.toplevel && !self.covered
    }

    /// Whether the view holds native focus, whatever its toplevel's. Output: the answer.
    pub fn is_native(self) -> bool {
        self.native
    }

    /// Gives or takes native focus; a covered view refuses it. Input: on or off. Output:
    /// the new activity when it changed.
    pub fn native(&mut self, on: bool) -> Option<bool> {
        self.change(|focus| focus.native = on && !focus.covered)
    }

    /// Notes the toplevel gaining or losing keyboard focus. Input: on or off. Output: the
    /// new activity when it changed.
    pub fn toplevel(&mut self, on: bool) -> Option<bool> {
        self.change(|focus| focus.toplevel = on)
    }

    /// Covers or uncovers the view; covering drops native focus. Input: covered or not.
    /// Output: the new activity when it changed.
    pub fn cover(&mut self, covered: bool) -> Option<bool> {
        self.change(|focus| {
            focus.covered = covered;
            focus.native &= !covered;
        })
    }

    fn change(&mut self, edit: impl FnOnce(&mut Focus)) -> Option<bool> {
        let before = self.active();
        edit(self);
        assert!(!(self.covered && self.native));
        let after = self.active();
        (before != after).then_some(after)
    }
}

/// Edits a view's shared focus. Inputs: the focus and the edit. Output: the new activity
/// when it changed.
fn edit(focus: &Cell<Focus>, change: impl FnOnce(&mut Focus) -> Option<bool>) -> Option<bool> {
    let mut next = focus.get();
    let changed = change(&mut next);
    focus.set(next);
    changed
}

/// Moves the input method's focus with the view's and tells the host. Inputs: the change,
/// the context, the view id and the sink. Output: none.
fn apply(change: Option<bool>, im: &gtk::IMMulticontext, id: u64, sink: Sink) {
    let Some(active) = change else {
        return;
    };
    if active {
        im.focus_in();
    } else {
        im.focus_out();
    }
    sink(id, ViewEvent::Focus(active));
}

/// Takes one key press while the view is active, ahead of the webview: a chord becomes
/// its command, the input method gets the key next, and a key it does not take goes to
/// the key map. Inputs: the context, the focus, the view id, the sink and the event.
/// Output: whether the key goes on to the page, which it does only while inactive.
fn press(
    im: &gtk::IMMulticontext,
    focus: &Cell<Focus>,
    id: u64,
    sink: Sink,
    event: &EventKey,
) -> Propagation {
    if !focus.get().active() {
        return Propagation::Proceed;
    }
    let (keyval, state) = (event.keyval(), event.state());
    if let Some(command) = chord(keyval, state) {
        sink(id, command);
        return Propagation::Stop;
    }
    if im.filter_keypress(event) {
        return Propagation::Stop;
    }
    if let Some(input) = keys::key_input(keyval, state) {
        sink(id, ViewEvent::Key(input));
    }
    Propagation::Stop
}

/// The Linux side of one native terminal view's input: it takes the toplevel's keys
/// ahead of the webview while active, through its own input method context, and hands
/// each key, commit and focus change to the host's sink with the view's id. Native focus
/// takes GTK focus off the page, as the macOS view takes first responder, and gives it
/// back on release; the page taking GTK focus releases native focus.
pub struct View {
    id: u64,
    sink: Sink,
    im: gtk::IMMulticontext,
    focus: Rc<Cell<Focus>>,
    toplevel: gtk::Window,
    page: RefCell<Option<WeakRef<gtk::Widget>>>,
    handlers: Vec<SignalHandlerId>,
    commit: Option<SignalHandlerId>,
}

impl View {
    /// Wires a view to a realized toplevel, without native focus. Inputs: the toplevel,
    /// the view id and the sink. Output: the view, or
    /// `native_terminal_surface_unavailable` when the toplevel has no GDK window.
    pub fn new(toplevel: &gtk::Window, id: u64, sink: Sink) -> Result<View, &'static str> {
        assert!(id > 0);
        let gdk = toplevel
            .window()
            .ok_or("native_terminal_surface_unavailable")?;
        let im = gtk::IMMulticontext::new();
        im.set_client_window(Some(&gdk));
        let commit = im.connect_commit(move |_, text| {
            if let Some(input) = commit_input(text) {
                sink(id, ViewEvent::Key(input));
            }
        });
        let focus = Rc::new(Cell::new(Focus::new(toplevel.has_toplevel_focus())));
        let on_press = {
            let (im, focus) = (im.clone(), Rc::clone(&focus));
            toplevel.connect_key_press_event(move |_, event| press(&im, &focus, id, sink, event))
        };
        let on_release = {
            let (im, focus) = (im.clone(), Rc::clone(&focus));
            toplevel.connect_key_release_event(move |_, event| {
                if !focus.get().active() {
                    return Propagation::Proceed;
                }
                im.filter_keypress(event);
                Propagation::Stop
            })
        };
        let on_focus_in = {
            let (im, focus) = (im.clone(), Rc::clone(&focus));
            toplevel.connect_focus_in_event(move |_, _| {
                apply(edit(&focus, |f| f.toplevel(true)), &im, id, sink);
                Propagation::Proceed
            })
        };
        let on_focus_out = {
            let (im, focus) = (im.clone(), Rc::clone(&focus));
            toplevel.connect_focus_out_event(move |_, _| {
                apply(edit(&focus, |f| f.toplevel(false)), &im, id, sink);
                Propagation::Proceed
            })
        };
        let on_page_focus = {
            let (im, focus) = (im.clone(), Rc::clone(&focus));
            toplevel.connect_set_focus(move |_, widget| {
                if widget.is_some() && focus.get().is_native() {
                    apply(edit(&focus, |f| f.native(false)), &im, id, sink);
                }
            })
        };
        Ok(View {
            id,
            sink,
            im,
            focus,
            toplevel: toplevel.clone(),
            page: RefCell::new(None),
            handlers: vec![
                on_press,
                on_release,
                on_focus_in,
                on_focus_out,
                on_page_focus,
            ],
            commit: Some(commit),
        })
    }

    /// Gives or takes native focus, as a click in the host rectangle or the page asks;
    /// refused while covered. Input: on or off. Output: none.
    pub fn focus(&self, on: bool) {
        self.moving(|f| f.native(on));
    }

    /// Covers or uncovers the view as the page reports an element over it; covering
    /// releases native focus, so keys go back to the page. Input: covered or not.
    /// Output: none.
    pub fn cover(&self, covered: bool) {
        self.moving(|f| f.cover(covered));
    }

    /// Applies a focus change, moving GTK focus off the page before the view's input
    /// method takes focus, and back to it after the view's lets go. Input: the change.
    /// Output: none.
    fn moving(&self, change: impl FnOnce(&mut Focus) -> Option<bool>) {
        let was = self.focus.get().is_native();
        let mut next = self.focus.get();
        let changed = change(&mut next);
        if !was && next.is_native() {
            self.take_page_focus();
        }
        self.focus.set(next);
        apply(changed, &self.im, self.id, self.sink);
        if was && !next.is_native() {
            self.give_page_focus();
        }
    }

    fn take_page_focus(&self) {
        let page = self
            .toplevel
            .focused_widget()
            .map(|widget| widget.downgrade());
        *self.page.borrow_mut() = page;
        self.toplevel.set_focus(None::<&gtk::Widget>);
        assert!(self.toplevel.focused_widget().is_none());
    }

    fn give_page_focus(&self) {
        let Some(page) = self
            .page
            .borrow_mut()
            .take()
            .and_then(|weak| weak.upgrade())
        else {
            return;
        };
        let toplevel: &gtk::Widget = self.toplevel.upcast_ref();
        if page.toplevel().as_ref() == Some(toplevel) {
            page.grab_focus();
        }
    }
}

impl Drop for View {
    /// Disconnects the toplevel and input method handlers, releases the context and
    /// gives GTK focus back to the page if the view held native focus. Output: none.
    fn drop(&mut self) {
        let native = self.focus.get().is_native();
        for handler in self.handlers.drain(..) {
            self.toplevel.disconnect(handler);
        }
        if let Some(commit) = self.commit.take() {
            self.im.disconnect(commit);
        }
        if self.focus.get().active() {
            self.im.focus_out();
        }
        self.im.set_client_window(None::<&gtk::gdk::Window>);
        if native {
            self.give_page_focus();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_term::linux::rows::{checkpoint, pump_for, pump_until, row_dir};
    use crate::native_term::pane::Named;
    use serde_json::json;
    use std::cell::RefCell;
    use std::time::Duration;
    use webkit2gtk::WebViewExt;

    /// The view id the display row's events carry.
    const ID: u64 = 7;

    /// A page whose textarea has focus and whose title mirrors the textarea, so the row
    /// reads what the page got.
    const PAGE: &str = "<!doctype html><title>v:</title><textarea id=\"t\" autofocus style=\"width:90vw;height:90vh\"></textarea><script>const t=document.getElementById('t');t.addEventListener('input',()=>{document.title='v:'+t.value});t.focus();</script>";

    thread_local! {
        static SEEN: RefCell<Vec<(u64, ViewEvent)>> = const { RefCell::new(Vec::new()) };
    }

    fn seen(id: u64, event: ViewEvent) {
        SEEN.with(|seen| seen.borrow_mut().push((id, event)));
    }

    fn seen_count() -> usize {
        SEEN.with(|seen| seen.borrow().len())
    }

    fn take_seen() -> Vec<ViewEvent> {
        SEEN.with(|seen| {
            let taken: Vec<(u64, ViewEvent)> = seen.borrow_mut().drain(..).collect();
            assert!(taken.iter().all(|(id, _)| *id == ID));
            taken.into_iter().map(|(_, event)| event).collect()
        })
    }

    fn text(bytes: &[u8]) -> ViewEvent {
        ViewEvent::Key(Input::Text(bytes.to_vec()))
    }

    #[test]
    fn the_terminal_chords_copy_paste_and_select_all() {
        let control_shift = ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK;
        assert_eq!(chord(key::C, control_shift), Some(ViewEvent::Copy));
        assert_eq!(chord(key::V, control_shift), Some(ViewEvent::Paste));
        assert_eq!(chord(key::A, control_shift), Some(ViewEvent::SelectAll));
        assert_eq!(chord(key::c, control_shift), Some(ViewEvent::Copy));
        let noise = ModifierType::LOCK_MASK
            | ModifierType::MOD2_MASK
            | ModifierType::MODIFIER_RESERVED_25_MASK
            | ModifierType::BUTTON1_MASK;
        assert_eq!(chord(key::V, control_shift | noise), Some(ViewEvent::Paste));
        let shift = ModifierType::SHIFT_MASK;
        assert_eq!(chord(key::Insert, shift), Some(ViewEvent::Paste));
        assert_eq!(chord(key::KP_Insert, shift), Some(ViewEvent::Paste));
    }

    #[test]
    fn other_chords_stay_keys() {
        let control = ModifierType::CONTROL_MASK;
        let shift = ModifierType::SHIFT_MASK;
        assert_eq!(chord(key::c, control), None);
        assert_eq!(
            chord(key::C, control | shift | ModifierType::MOD1_MASK),
            None
        );
        assert_eq!(
            chord(key::C, control | shift | ModifierType::SUPER_MASK),
            None
        );
        assert_eq!(chord(key::X, control | shift), None);
        assert_eq!(chord(key::Insert, ModifierType::empty()), None);
        assert_eq!(chord(key::Insert, control), None);
        assert_eq!(chord(key::Insert, control | shift), None);
    }

    #[test]
    fn a_commit_is_one_text_input_within_its_bound() {
        assert_eq!(commit_input("a"), Some(Input::Text(b"a".to_vec())));
        assert_eq!(
            commit_input("é"),
            Some(Input::Text("é".as_bytes().to_vec()))
        );
        assert_eq!(commit_input(""), None);
        let most = "x".repeat(COMMIT_BYTES_MAX);
        assert!(commit_input(&most).is_some());
        assert_eq!(commit_input(&format!("{most}x")), None);
    }

    #[test]
    fn keys_go_to_the_view_only_with_native_and_toplevel_focus_and_no_cover() {
        let mut focus = Focus::new(true);
        assert!(!focus.active());
        assert_eq!(focus.native(true), Some(true));
        assert_eq!(focus.native(true), None);
        assert_eq!(focus.toplevel(false), Some(false));
        assert_eq!(focus.native(false), None);
        assert_eq!(focus.toplevel(true), None);
        assert_eq!(focus.native(true), Some(true));
        assert!(focus.active());
    }

    #[test]
    fn a_cover_drops_native_focus_until_the_page_gives_it_back() {
        let mut focus = Focus::new(true);
        assert_eq!(focus.native(true), Some(true));
        assert_eq!(focus.cover(true), Some(false));
        assert_eq!(focus.native(true), None);
        assert!(!focus.active());
        assert_eq!(focus.cover(false), None);
        assert!(!focus.active());
        assert_eq!(focus.native(true), Some(true));
    }

    fn page_text(page: &webkit2gtk::WebView) -> String {
        page.title()
            .map(|title| title.to_string())
            .unwrap_or_default()
    }

    fn step(name: &str) {
        checkpoint(name, &json!({ "step": name }));
    }

    /// A WebKit page's textarea has focus and takes typed and composed text; then the
    /// view takes native focus and gets each key and each input method commit once, while
    /// the page gets none; a cover sends keys back to the page. Spike run 3 left open
    /// whether the view composes on sway after the page composed; this is that red row.
    #[test]
    #[ignore = "needs a display, IBus and the farm's typing: the farm runs it under Xvfb and sway"]
    fn a_focused_view_takes_keys_and_commits_after_the_page_had_them() {
        assert!(
            row_dir().is_some(),
            "NATIVE_ROW_DIR: the farm types the keys"
        );
        gtk::init().expect("a display");
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title("native-row");
        window.set_default_size(640, 480);
        let page = webkit2gtk::WebView::new();
        window.add(&page);
        window.show_all();
        page.load_html(PAGE, None);
        pump_until("page load", || {
            !page.is_loading() && page_text(&page) == "v:"
        });
        page.grab_focus();
        pump_for(Duration::from_millis(300));
        let view = View::new(&window, ID, seen).expect("a view");

        step("page");
        pump_until("page typing", || page_text(&page) == "v:xy");
        assert_eq!(take_seen(), Vec::new());

        assert!(page.is_focus());
        view.focus(true);
        assert_eq!(take_seen(), vec![ViewEvent::Focus(true)]);
        assert!(
            !page.is_focus(),
            "native focus takes GTK focus off the page"
        );
        step("native");
        pump_until("view keys", || seen_count() >= 4);
        pump_for(Duration::from_millis(200));
        assert_eq!(
            take_seen(),
            vec![
                text(b"a"),
                text(b"b"),
                text(b"\r"),
                ViewEvent::Key(Input::Key {
                    key: Named::Up,
                    mods: 0
                }),
            ]
        );
        assert_eq!(page_text(&page), "v:xy");

        page.grab_focus();
        assert_eq!(
            take_seen(),
            vec![ViewEvent::Focus(false)],
            "the page taking GTK focus releases native focus"
        );
        step("page-ime");
        pump_until("page composition", || page_text(&page) == "v:xyé");

        view.focus(true);
        assert_eq!(take_seen(), vec![ViewEvent::Focus(true)]);
        assert!(!page.is_focus());
        step("native-ime");
        pump_until("view composition", || seen_count() >= 1);
        pump_for(Duration::from_millis(300));
        assert_eq!(take_seen(), vec![text("á".as_bytes())]);
        assert_eq!(page_text(&page), "v:xyé");

        view.cover(true);
        assert_eq!(take_seen(), vec![ViewEvent::Focus(false)]);
        assert!(page.is_focus(), "a cover gives GTK focus back to the page");
        view.focus(true);
        assert_eq!(
            take_seen(),
            Vec::new(),
            "a covered view refuses native focus"
        );
        step("covered");
        pump_until("covered typing", || page_text(&page) == "v:xyéz");
        assert_eq!(take_seen(), Vec::new());
        drop(view);
    }
}
