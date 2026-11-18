use std::cell::Cell;
use std::rc::Rc;

use gtk::glib::SignalHandlerId;
use gtk::prelude::*;

/// The signal handlers one native object keeps on a GTK toplevel. Dropping them
/// disconnects them, unless the toplevel was destroyed first: its dispose already removed
/// every handler, and disconnecting a removed one is a GLib critical.
pub struct Handlers {
    toplevel: gtk::Window,
    ids: Vec<SignalHandlerId>,
    destroyed: Rc<Cell<bool>>,
}

impl Handlers {
    /// Starts a set on a live toplevel, watching for its destruction. Input: the toplevel.
    /// Output: the set.
    pub fn new(toplevel: &gtk::Window) -> Handlers {
        let destroyed = Rc::new(Cell::new(false));
        let flag = Rc::clone(&destroyed);
        let watch = toplevel.connect_destroy(move |_| flag.set(true));
        Handlers {
            toplevel: toplevel.clone(),
            ids: vec![watch],
            destroyed,
        }
    }

    /// Keeps one more handler of the toplevel. Input: its id. Output: none.
    pub fn push(&mut self, id: SignalHandlerId) {
        assert!(
            !self.destroyed.get(),
            "a destroyed toplevel takes no handler"
        );
        self.ids.push(id);
    }

    /// Disconnects every handler now, while the toplevel lives; after its destruction they
    /// are already gone. A second call finds none. Output: none.
    pub fn disconnect(&mut self) {
        let ids = std::mem::take(&mut self.ids);
        if self.destroyed.get() {
            return;
        }
        for id in ids {
            self.toplevel.disconnect(id);
        }
        assert!(self.ids.is_empty());
    }
}

impl Drop for Handlers {
    /// Disconnects whatever handlers are left. Output: none.
    fn drop(&mut self) {
        self.disconnect();
    }
}
