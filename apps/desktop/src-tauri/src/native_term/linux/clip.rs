#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_term::codec::COPY_BYTES_MAX;
    use crate::native_term::linux::rows::{checkpoint, pump_for, pump_until, row_dir};
    use gtk::gdk::EventType;
    use gtk::prelude::*;
    use serde_json::json;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    const KEY: Option<EventType> = Some(EventType::KeyPress);

    #[test]
    fn only_a_key_or_button_event_reads_or_writes_a_board() {
        for kind in [
            EventType::KeyPress,
            EventType::ButtonPress,
            EventType::DoubleButtonPress,
            EventType::TripleButtonPress,
            EventType::ButtonRelease,
        ] {
            assert!(user_input(Some(kind)), "{kind:?}");
        }
        for kind in [
            EventType::KeyRelease,
            EventType::MotionNotify,
            EventType::Scroll,
            EventType::Expose,
            EventType::FocusChange,
        ] {
            assert!(!user_input(Some(kind)), "{kind:?}");
        }
        assert!(!user_input(None));
    }

    #[test]
    fn a_read_longer_than_a_copy_is_dropped() {
        assert_eq!(bounded(Some("a")), Some("a".to_owned()));
        assert_eq!(bounded(Some("")), None);
        assert_eq!(bounded(None), None);
        let most = "x".repeat(COPY_BYTES_MAX);
        assert_eq!(
            bounded(Some(&most)).map(|text| text.len()),
            Some(COPY_BYTES_MAX)
        );
        assert_eq!(bounded(Some(&format!("{most}x"))), None);
    }

    fn read_now(clip: &Clip, board: Board, input: Option<EventType>) -> Option<String> {
        let got = Rc::new(RefCell::new(None));
        let called = Rc::new(RefCell::new(false));
        let (into, done) = (Rc::clone(&got), Rc::clone(&called));
        clip.read(board, input, move |text| {
            *into.borrow_mut() = text;
            *done.borrow_mut() = true;
        });
        assert!(*called.borrow(), "a private board answers at once");
        got.take()
    }

    #[test]
    fn a_private_board_keeps_each_board_in_process() {
        let clip = Clip::private();
        assert!(clip.is_private());
        assert_eq!(read_now(&clip, Board::Clipboard, KEY), None);
        assert!(clip.write(Board::Clipboard, "one", KEY));
        assert!(clip.write(Board::Primary, "two", KEY));
        assert_eq!(
            read_now(&clip, Board::Clipboard, KEY),
            Some("one".to_owned())
        );
        assert_eq!(read_now(&clip, Board::Primary, KEY), Some("two".to_owned()));
        assert!(!clip.write(Board::Clipboard, "idle", None));
        assert!(!clip.write(Board::Clipboard, "", KEY));
        assert!(!clip.write(Board::Clipboard, &"x".repeat(COPY_BYTES_MAX + 1), KEY));
        assert_eq!(
            read_now(&clip, Board::Clipboard, KEY),
            Some("one".to_owned())
        );
        assert_eq!(read_now(&clip, Board::Clipboard, None), None);
    }

    fn farm_text(name: &str) -> String {
        let dir = row_dir().expect("NATIVE_ROW_DIR");
        std::fs::read_to_string(dir.join(name))
            .unwrap_or_default()
            .trim_end_matches('\n')
            .to_owned()
    }

    fn step(name: &str) {
        checkpoint(name, &json!({ "step": name }));
    }

    /// What the display row's key handler did: the two writes' results and the two reads'
    /// texts once they came back.
    #[derive(Default)]
    struct Seen {
        wrote: Option<(bool, bool)>,
        clipboard: Option<Option<String>>,
        primary: Option<Option<String>>,
    }

    /// The desktop boards on a real display: a key press writes CLIPBOARD and PRIMARY and
    /// the farm's own tools read both; a write outside an input event is refused and the
    /// farm still reads the old text; the farm writes both boards and a key press reads
    /// them back through GTK's async read; a read outside an input event gets nothing.
    /// Spike run 3 read nothing from PRIMARY on sway; this is that red row.
    #[test]
    #[ignore = "needs a display and the farm's clipboard tools: the farm runs it under Xvfb and sway"]
    fn the_desktop_boards_take_and_give_text_only_on_user_input() {
        assert!(
            row_dir().is_some(),
            "NATIVE_ROW_DIR: the farm reads and writes the boards"
        );
        gtk::init().expect("a display");
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title("native-row");
        window.set_default_size(320, 200);
        window.show_all();
        pump_until("the window", || {
            window.window().is_some_and(|gdk| gdk.is_viewable())
        });
        let clip = Rc::new(Clip::desktop());
        assert!(!clip.is_private());
        let seen = Rc::new(RefCell::new(Seen::default()));
        {
            let (clip, seen) = (Rc::clone(&clip), Rc::clone(&seen));
            window.connect_key_press_event(move |_, event| {
                let input = Some(event.event_type());
                match event.keyval().to_unicode() {
                    Some('w') => {
                        let wrote = (
                            clip.write(Board::Clipboard, "native-clip-1", input),
                            clip.write(Board::Primary, "native-primary-1", input),
                        );
                        seen.borrow_mut().wrote = Some(wrote);
                    }
                    Some('r') => {
                        let into = Rc::clone(&seen);
                        clip.read(Board::Clipboard, input, move |text| {
                            into.borrow_mut().clipboard = Some(text);
                        });
                        let into = Rc::clone(&seen);
                        clip.read(Board::Primary, input, move |text| {
                            into.borrow_mut().primary = Some(text);
                        });
                    }
                    _ => {}
                }
                gtk::glib::Propagation::Stop
            });
        }
        pump_for(Duration::from_millis(300));

        step("write");
        pump_until("the key's writes", || seen.borrow().wrote.is_some());
        assert_eq!(seen.borrow().wrote, Some((true, true)));
        step("written");
        assert_eq!(farm_text("farm-clipboard.txt"), "native-clip-1");
        assert_eq!(farm_text("farm-primary.txt"), "native-primary-1");

        assert!(!clip.write(Board::Clipboard, "native-idle", None));
        step("refused");
        assert_eq!(farm_text("farm-clipboard.txt"), "native-clip-1");

        step("read");
        pump_until("both reads", || {
            let seen = seen.borrow();
            seen.clipboard.is_some() && seen.primary.is_some()
        });
        assert_eq!(
            seen.borrow().clipboard,
            Some(Some("farm-clip-2".to_owned()))
        );
        assert_eq!(
            seen.borrow().primary,
            Some(Some("farm-primary-2".to_owned()))
        );

        let idle = Rc::new(RefCell::new(None));
        let into = Rc::clone(&idle);
        clip.read(Board::Clipboard, None, move |text| {
            *into.borrow_mut() = Some(text);
        });
        pump_until("the refused read", || idle.borrow().is_some());
        assert_eq!(*idle.borrow(), Some(None));
        unsafe { window.destroy() };
    }
}
