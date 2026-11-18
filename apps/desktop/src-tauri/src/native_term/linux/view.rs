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

        view.focus(true);
        assert_eq!(take_seen(), vec![ViewEvent::Focus(true)]);
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

        view.focus(false);
        assert_eq!(take_seen(), vec![ViewEvent::Focus(false)]);
        page.grab_focus();
        step("page-ime");
        pump_until("page composition", || page_text(&page) == "v:xyé");

        view.focus(true);
        assert_eq!(take_seen(), vec![ViewEvent::Focus(true)]);
        step("native-ime");
        pump_until("view composition", || seen_count() >= 1);
        pump_for(Duration::from_millis(300));
        assert_eq!(take_seen(), vec![text("á".as_bytes())]);
        assert_eq!(page_text(&page), "v:xyé");

        view.cover(true);
        assert_eq!(take_seen(), vec![ViewEvent::Focus(false)]);
        page.grab_focus();
        step("covered");
        pump_until("covered typing", || page_text(&page) == "v:xyéz");
        assert_eq!(take_seen(), Vec::new());
        drop(view);
    }
}
