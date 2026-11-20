use std::cell::RefCell;

use gtk::gdk::{self, EventType};

use crate::native_term::codec::COPY_BYTES_MAX;

/// A desktop board: the clipboard that Copy and Paste use, or the primary selection that
/// a selection fills and a middle click pastes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Board {
    Clipboard,
    Primary,
}

impl Board {
    fn slot(self) -> usize {
        match self {
            Board::Clipboard => 0,
            Board::Primary => 1,
        }
    }

    fn atom(self) -> gdk::Atom {
        match self {
            Board::Clipboard => gdk::SELECTION_CLIPBOARD,
            Board::Primary => gdk::SELECTION_PRIMARY,
        }
    }
}

/// The seat guard: whether an event the user made is being handled, so a board may be
/// read or written. Only a key press or a button press or release counts. Input: the
/// type of GTK's current event, if any. Output: the answer.
pub fn user_input(kind: Option<EventType>) -> bool {
    matches!(
        kind,
        Some(
            EventType::KeyPress
                | EventType::ButtonPress
                | EventType::DoubleButtonPress
                | EventType::TripleButtonPress
                | EventType::ButtonRelease
        )
    )
}

#[cfg(test)]
thread_local! {
    /// The event type a unit row stands in for GTK's current one, while it sets one.
    static AS_IF: std::cell::Cell<Option<Option<EventType>>> =
        const { std::cell::Cell::new(None) };
    /// Whether a seat is there, as a unit row stands it in for the display's, while it sets one.
    static AS_SEAT: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// The type of the event GTK is handling now, for the seat guard; a unit row may stand
/// one in. Output: it, or none outside an event handler.
fn current_input() -> Option<EventType> {
    #[cfg(test)]
    if let Some(kind) = AS_IF.with(std::cell::Cell::get) {
        return kind;
    }
    gtk::current_event().map(|event| event.event_type())
}

/// A board's text as a paste takes it: 1 to `COPY_BYTES_MAX` bytes, else none. Input:
/// the text read. Output: the text kept.
pub fn bounded(text: Option<&str>) -> Option<String> {
    text.filter(|text| !text.is_empty() && text.len() <= COPY_BYTES_MAX)
        .map(str::to_owned)
}

/// The boards the views copy to and paste from: the desktop's CLIPBOARD and PRIMARY
/// through GTK, or in a `native-webdriver` build both kept in process memory, so a driven
/// copy or paste never touches the owner's boards. Every read and write passes the seat
/// guard, which reads GTK's current event itself; a desktop read is async and never runs a
/// nested main loop.
pub struct Clip {
    private: Option<RefCell<[Option<String>; 2]>>,
}

impl Clip {
    /// The desktop's boards through GTK. Output: them.
    pub fn desktop() -> Clip {
        Clip { private: None }
    }

    /// Empty boards kept in process memory. Output: them.
    pub fn private() -> Clip {
        Clip {
            private: Some(RefCell::new([None, None])),
        }
    }

    /// Whether the boards are the in-process ones. Output: the answer.
    pub fn is_private(&self) -> bool {
        self.private.is_some()
    }

    /// Puts plain text on a board in place of what it held, inside a user event only.
    /// Inputs: the board and 1 to `COPY_BYTES_MAX` bytes of text. Output: whether the write
    /// was made; a desktop board takes it as GTK hands it on.
    pub fn write(&self, board: Board, text: &str) -> bool {
        if !user_input(current_input()) || text.is_empty() || text.len() > COPY_BYTES_MAX {
            return false;
        }
        match &self.private {
            Some(boards) => boards.borrow_mut()[board.slot()] = Some(text.to_owned()),
            None => gtk::Clipboard::get(&board.atom()).set_text(text),
        }
        true
    }

    /// Puts plain text on a board for the terminal window's own Copy command, which comes
    /// over IPC with no GTK event: it needs the display's seat in place of a user event, and
    /// only the terminal window's IPC path calls it. Inputs: the board and 1 to
    /// `COPY_BYTES_MAX` bytes of text. Output: whether the write was made.
    pub fn write_command(&self, _board: Board, _text: &str) -> bool {
        false
    }

    /// Reads a board's plain text inside a user event only; the reply comes at once from
    /// a private board or a refused read, and from GTK's main loop for a desktop board.
    /// Inputs: the board and the reply. Output: none; the reply gets the text, bounded as a
    /// paste takes it, or none.
    pub fn read(&self, board: Board, reply: impl FnOnce(Option<String>) + 'static) {
        if !user_input(current_input()) {
            reply(None);
            return;
        }
        match &self.private {
            Some(boards) => {
                let text = boards.borrow()[board.slot()].clone();
                reply(bounded(text.as_deref()));
            }
            None => {
                gtk::Clipboard::get(&board.atom()).request_text(move |_, text| reply(bounded(text)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_term::linux::rows::{checkpoint, pump_for, pump_until, row_dir};
    use gtk::gdk::EventType;
    use gtk::prelude::*;
    use serde_json::json;
    use std::cell::{Cell, RefCell};
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

    /// Runs a board call as if GTK were handling an event of a type, or none. Inputs: the
    /// type and the call. Output: what the call gives.
    fn as_if<R>(kind: Option<EventType>, call: impl FnOnce() -> R) -> R {
        AS_IF.with(|slot| slot.set(Some(kind)));
        let out = call();
        AS_IF.with(|slot| slot.set(None));
        out
    }

    /// Runs a board call as if the display had a seat, or none. Inputs: whether it has one
    /// and the call. Output: what the call gives.
    fn with_seat<R>(seat: bool, call: impl FnOnce() -> R) -> R {
        AS_SEAT.with(|slot| slot.set(Some(seat)));
        let out = call();
        AS_SEAT.with(|slot| slot.set(None));
        out
    }

    fn read_now(clip: &Clip, board: Board, input: Option<EventType>) -> Option<String> {
        let got = Rc::new(RefCell::new(None));
        let called = Rc::new(RefCell::new(false));
        let (into, done) = (Rc::clone(&got), Rc::clone(&called));
        as_if(input, || {
            clip.read(board, move |text| {
                *into.borrow_mut() = text;
                *done.borrow_mut() = true;
            })
        });
        assert!(*called.borrow(), "a private board answers at once");
        got.take()
    }

    #[test]
    fn a_private_board_keeps_each_board_in_process() {
        let clip = Clip::private();
        assert!(clip.is_private());
        let write = |board, text: &str, input| as_if(input, || clip.write(board, text));
        assert_eq!(read_now(&clip, Board::Clipboard, KEY), None);
        assert!(write(Board::Clipboard, "one", KEY));
        assert!(write(Board::Primary, "two", KEY));
        assert_eq!(
            read_now(&clip, Board::Clipboard, KEY),
            Some("one".to_owned())
        );
        assert_eq!(read_now(&clip, Board::Primary, KEY), Some("two".to_owned()));
        assert!(!write(Board::Clipboard, "idle", None));
        assert!(!write(Board::Clipboard, "", KEY));
        assert!(!write(
            Board::Clipboard,
            &"x".repeat(COPY_BYTES_MAX + 1),
            KEY
        ));
        assert_eq!(
            read_now(&clip, Board::Clipboard, KEY),
            Some("one".to_owned())
        );
        assert_eq!(read_now(&clip, Board::Clipboard, None), None);
    }

    #[test]
    fn a_command_write_needs_the_seat_not_an_event() {
        let clip = Clip::private();
        let command = |text: &str, seat| {
            with_seat(seat, || {
                as_if(None, || clip.write_command(Board::Clipboard, text))
            })
        };
        assert!(
            command("copied", true),
            "a command writes with a seat and no event"
        );
        assert_eq!(
            read_now(&clip, Board::Clipboard, KEY),
            Some("copied".to_owned())
        );
        assert!(!command("seatless", false), "a command needs the seat");
        assert!(!command("", true));
        assert!(!command(&"x".repeat(COPY_BYTES_MAX + 1), true));
        assert_eq!(
            read_now(&clip, Board::Clipboard, KEY),
            Some("copied".to_owned())
        );
        assert!(
            !with_seat(true, || as_if(None, || clip
                .write(Board::Clipboard, "event"))),
            "a plain write still needs a user event"
        );
    }

    /// The page's Copy on a real display. The command comes over IPC with no GTK event, which
    /// a glib timeout after a key press stands in for: a plain write from that timeout is
    /// refused, a command write puts its text on the desktop CLIPBOARD, and the farm reads it.
    #[test]
    #[ignore = "needs a display and the farm's clipboard tools: the farm runs it under Xvfb and sway"]
    fn a_command_write_reaches_the_desktop_board_with_no_event() {
        assert!(
            row_dir().is_some(),
            "NATIVE_ROW_DIR: the farm reads the board"
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
        let wrote = Rc::new(Cell::new(None));
        {
            let (clip, wrote) = (Rc::clone(&clip), Rc::clone(&wrote));
            window.connect_key_press_event(move |_, event| {
                if event.keyval().to_unicode() == Some('c') {
                    let (clip, wrote) = (Rc::clone(&clip), Rc::clone(&wrote));
                    let _ =
                        gtk::glib::timeout_add_local_once(Duration::from_millis(10), move || {
                            let plain = clip.write(Board::Clipboard, "native-plain-1");
                            let command = clip.write_command(Board::Clipboard, "native-command-1");
                            wrote.set(Some((plain, command)));
                        });
                }
                gtk::glib::Propagation::Stop
            });
        }
        pump_for(Duration::from_millis(300));

        step("command");
        pump_until("the timeout's writes", || wrote.get().is_some());
        assert_eq!(
            wrote.get(),
            Some((false, true)),
            "a timeout's plain write is refused and its command write is made"
        );
        step("commanded");
        assert_eq!(
            farm_text("farm-clipboard.txt"),
            "native-command-1",
            "the desktop board holds the command's text"
        );
        unsafe { window.destroy() };
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
                match event.keyval().to_unicode() {
                    Some('w') => {
                        let wrote = (
                            clip.write(Board::Clipboard, "native-clip-1"),
                            clip.write(Board::Primary, "native-primary-1"),
                        );
                        seen.borrow_mut().wrote = Some(wrote);
                    }
                    Some('r') => {
                        let into = Rc::clone(&seen);
                        clip.read(Board::Clipboard, move |text| {
                            into.borrow_mut().clipboard = Some(text);
                        });
                        let into = Rc::clone(&seen);
                        clip.read(Board::Primary, move |text| {
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

        assert!(!clip.write(Board::Clipboard, "native-idle"));
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
        clip.read(Board::Clipboard, move |text| {
            *into.borrow_mut() = Some(text);
        });
        pump_until("the refused read", || idle.borrow().is_some());
        assert_eq!(*idle.borrow(), Some(None));
        unsafe { window.destroy() };
    }

    /// A board call from a glib timeout, which runs under no user event, is refused on a
    /// real display, where the guard reads GTK's current event: the write is not made and
    /// the read of the seeded board gets nothing, and the seed is still there after.
    #[test]
    #[ignore = "needs a display: the farm runs it under Xvfb and sway"]
    fn a_board_call_from_a_timeout_is_refused() {
        gtk::init().expect("a display");
        let clip = Rc::new(Clip::private());
        assert!(as_if(KEY, || clip.write(Board::Clipboard, "seeded")));
        let seen = Rc::new(RefCell::new(None));
        {
            let (clip, seen) = (Rc::clone(&clip), Rc::clone(&seen));
            let _ = gtk::glib::timeout_add_local_once(Duration::from_millis(10), move || {
                let wrote = clip.write(Board::Clipboard, "timeout");
                let read = Rc::new(RefCell::new(None));
                let into = Rc::clone(&read);
                clip.read(Board::Clipboard, move |text| {
                    *into.borrow_mut() = Some(text)
                });
                *seen.borrow_mut() = Some((wrote, read.take()));
            });
        }
        pump_until("the timeout", || seen.borrow().is_some());
        assert_eq!(
            *seen.borrow(),
            Some((false, Some(None))),
            "a timeout neither writes nor reads a board"
        );
        assert_eq!(
            read_now(&clip, Board::Clipboard, KEY),
            Some("seeded".to_owned())
        );
    }
}
