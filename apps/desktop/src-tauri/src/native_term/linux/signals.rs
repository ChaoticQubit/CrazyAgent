use std::cell::Cell;
use std::rc::Rc;

use gtk::glib::SignalHandlerId;
use gtk::prelude::*;

/// The signal handlers one native object keeps on a GTK widget, the toplevel or the page.
/// Dropping them disconnects them, unless the widget was destroyed first: its dispose
/// already removed every handler, and disconnecting a removed one is a GLib critical.
pub struct Handlers {
    widget: gtk::Widget,
    ids: Vec<SignalHandlerId>,
    destroyed: Rc<Cell<bool>>,
}

impl Handlers {
    /// Starts a set on a live widget, watching for its destruction. Input: the widget.
    /// Output: the set.
    pub fn new(widget: &impl IsA<gtk::Widget>) -> Handlers {
        let destroyed = Rc::new(Cell::new(false));
        let flag = Rc::clone(&destroyed);
        let watch = widget.connect_destroy(move |_| flag.set(true));
        Handlers {
            widget: widget.upcast_ref::<gtk::Widget>().clone(),
            ids: vec![watch],
            destroyed,
        }
    }

    /// Keeps one more handler of the widget. Input: its id. Output: none.
    pub fn push(&mut self, id: SignalHandlerId) {
        assert!(!self.destroyed.get(), "a destroyed widget takes no handler");
        self.ids.push(id);
    }

    /// Disconnects every handler now, while the widget lives; after its destruction they
    /// are already gone. A second call finds none. Output: none.
    pub fn disconnect(&mut self) {
        let ids = std::mem::take(&mut self.ids);
        if self.destroyed.get() {
            return;
        }
        for id in ids {
            self.widget.disconnect(id);
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
