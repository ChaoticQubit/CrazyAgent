use std::rc::Rc;

use super::clip::Clip;
use super::surface::{Child, Rect, SCALE_MAX};
use super::view::View;
use crate::native_term::event::Sink;

/// One native terminal's Linux parts: its input view, made at open, and the child surface
/// it draws into, made at its first shown place, since a child needs a rectangle. Focus
/// asked for before that place is owed and applied right after it. The host drops its wgpu
/// surface on the child before the term.
pub struct Term {
    toplevel: gtk::Window,
    page: gtk::Widget,
    view: View,
    child: Option<Child>,
    focus_owed: bool,
}

impl Term {
    /// Wires a term's view to a realized toplevel and the page widget under it, with no
    /// child yet. Inputs: the toplevel, the page widget, the view id, the sink and the
    /// host's boards. Output: the term, or `native_terminal_surface_unavailable`.
    pub fn new(
        toplevel: &gtk::Window,
        page: &gtk::Widget,
        id: u64,
        sink: Sink,
        clip: Rc<Clip>,
    ) -> Result<Term, &'static str> {
        let view = View::new(toplevel, page, id, sink, clip)?;
        Ok(Term {
            toplevel: toplevel.clone(),
            page: page.clone(),
            view,
            child: None,
            focus_owed: false,
        })
    }

    /// Gives the view native focus, as the page asks. The view refuses it before the first
    /// shown place, so until then it is owed and applied right after that place. Output:
    /// none.
    pub fn focus(&mut self) {
        if self.child.is_none() {
            self.focus_owed = true;
        } else {
            self.view.focus(true);
        }
    }

    /// Puts the term over its host rectangle at the toplevel's scale and shows it, or takes
    /// it off screen. The first shown place makes the child and applies owed focus. Inputs:
    /// the rectangle in the toplevel's logical pixels, the scale, 1 to 8, and whether it
    /// shows. Output: the scale the child now draws at, none while hidden, or
    /// `native_terminal_surface_unavailable` when no child can be made.
    pub fn place(
        &mut self,
        rect: Rect,
        scale: i32,
        shown: bool,
    ) -> Result<Option<i32>, &'static str> {
        assert!((1..=SCALE_MAX).contains(&scale));
        if !shown {
            if let Some(child) = &mut self.child {
                child.hide();
            }
            self.view.place(None);
            return Ok(None);
        }
        if self.child.is_none() {
            self.child = Some(Child::new(&self.toplevel, &self.page, rect)?);
        }
        let child = self.child.as_mut().expect("a child made above");
        child.place_scaled(rect, scale);
        child.show();
        let drawn = child.scale();
        self.view.place(Some(rect));
        if std::mem::take(&mut self.focus_owed) {
            self.view.focus(true);
        }
        assert!(!self.focus_owed);
        Ok(Some(drawn))
    }

    /// Covers or uncovers the term as the page reports an element over it. A cover gives
    /// keys back to the page and takes the child off screen; the host's next shown place
    /// puts the child back. Input: covered or not. Output: none.
    pub fn cover(&mut self, covered: bool) {
        self.view.cover(covered);
        if covered && let Some(child) = &mut self.child {
            child.hide();
        }
    }

    /// Whether the host may present now: the child exists, shows and owes a frame that its
    /// last present's callback no longer holds back. Output: the answer.
    pub fn ready(&mut self) -> bool {
        self.child.as_mut().is_some_and(Child::ready)
    }

    /// Notes a present about to happen at the child's size and scale, which the host
    /// configures its wgpu surface to in the same layout as each place. Output: none.
    pub fn presenting(&mut self) {
        let child = self.child.as_mut().expect("a present after a shown place");
        let (size, scale) = (child.size(), child.scale());
        child.presenting(size, scale);
    }

    /// Notes that the present went out. Output: none.
    pub fn presented(&mut self) {
        self.child
            .as_mut()
            .expect("a present after a shown place")
            .presented();
    }

    /// Notes that the draw failed before presenting, so the frame is owed again. Output:
    /// none.
    pub fn not_presented(&mut self) {
        self.child
            .as_mut()
            .expect("a present after a shown place")
            .not_presented();
    }

    /// Notes a change to draw. Output: none.
    pub fn changed(&mut self) {
        if let Some(child) = &mut self.child {
            child.changed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_term::event::ViewEvent;
    use crate::native_term::linux::rows::{
        checkpoint, green_terminal, pump, pump_for, pump_until, white_toplevel,
    };
    use crate::native_term::render::{Shared, Surface};
    use crate::native_term::screen::Frame;
    use gtk::prelude::*;
    use serde_json::json;
    use std::time::{Duration, Instant};

    /// The view id the rows' terms carry.
    const ID: u64 = 9;

    /// The host rectangle: inside the 640x480 page at scale 1 and at twice it.
    const RECT: Rect = Rect {
        x: 100,
        y: 80,
        w: 200,
        h: 150,
    };

    fn dropped(_: u64, _: ViewEvent) {}

    fn term_over(window: &gtk::Window, page: &gtk::DrawingArea) -> Term {
        Term::new(
            window,
            page.upcast_ref(),
            ID,
            dropped,
            Rc::new(Clip::private()),
        )
        .expect("a term")
    }

    /// Presents until the child has shown a frame, as the host's drain does, then lets the
    /// compositor show it. Inputs: the term, its renderer, surface and frame.
    fn show_frames(term: &mut Term, shared: &mut Shared, surface: &mut Surface, frame: &Frame) {
        for _ in 0..3 {
            pump_until("the term ready to present", || term.ready());
            term.presenting();
            shared.draw(surface, frame).expect("a present");
            term.presented();
            term.changed();
        }
        pump_for(Duration::from_millis(200));
    }

    /// Writes a step for the farm's screenshot: the child's device rectangle and, on X11,
    /// its root origin. Inputs: the step and the term.
    fn step(name: &str, term: &Term) {
        let child = term.child.as_ref().expect("a child after a shown place");
        let (x, y, w, h) = child.device_rect();
        let line = json!({
            "step": name,
            "device": [x, y, w, h],
            "root": child.root_origin().map(|(rx, ry)| [rx, ry]),
            "scale": child.scale(),
        });
        checkpoint(name, &line);
    }

    /// Owes a frame and runs GTK's events for 300 ms, failing the row if the term would
    /// present. Inputs: the term and the failure message.
    fn never_presents(term: &mut Term, message: &str) {
        term.changed();
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(300) {
            pump();
            assert!(!term.ready(), "{message}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn close(window: gtk::Window, term: Term) {
        drop(term);
        unsafe {
            window.destroy();
        }
        pump_for(Duration::from_millis(100));
    }

    /// The page asks a new term for focus before the host's first place, which the view
    /// refuses unplaced; the term owes it and applies it right after that place, once. A
    /// focus asked once the term shows applies at once.
    #[test]
    #[ignore = "needs a display: the farm runs it under Xvfb and sway"]
    fn focus_asked_before_the_first_place_is_applied_right_after_it() {
        let (window, page) = white_toplevel();
        let mut term = term_over(&window, &page);
        term.view.toplevel_focused();
        term.focus();
        assert!(
            !term.view.focus_state().is_native(),
            "an unplaced view holds no native focus"
        );
        let scale = window.scale_factor();
        assert_eq!(term.place(RECT, scale, true), Ok(Some(scale)));
        assert!(
            term.view.focus_state().active(),
            "focus asked before the first place is applied right after it"
        );
        assert_eq!(term.place(RECT, scale, false), Ok(None));
        assert!(
            !term.view.focus_state().is_native(),
            "a hidden term holds no native focus"
        );
        assert_eq!(term.place(RECT, scale, true), Ok(Some(scale)));
        assert!(
            !term.view.focus_state().is_native(),
            "owed focus applies once, not at every place"
        );
        term.focus();
        assert!(
            term.view.focus_state().active(),
            "focus asked after the first place applies at once"
        );
        close(window, term);
    }

    /// A shown term with native focus is covered: the keys go back to the page and its child
    /// leaves the screen, so the farm's screenshot reads the page's white over the rectangle,
    /// and it never presents while covered. Uncovered and placed again, it draws green again;
    /// a hidden place takes the child off screen as a cover does.
    #[test]
    #[ignore = "needs a display: the farm runs it under Xvfb, weston and sway"]
    fn a_cover_takes_the_child_off_screen_until_the_next_shown_place() {
        let (window, page) = white_toplevel();
        let mut term = term_over(&window, &page);
        let scale = window.scale_factor();
        assert_eq!(term.place(RECT, scale, true), Ok(Some(scale)));
        let child = term.child.as_ref().expect("a child after a shown place");
        let (mut shared, mut surface, frame) = green_terminal(child);
        show_frames(&mut term, &mut shared, &mut surface, &frame);
        step("shown", &term);
        term.view.toplevel_focused();
        term.focus();
        assert!(term.view.focus_state().active(), "a shown term takes focus");
        term.cover(true);
        assert!(
            !term.view.focus_state().is_native(),
            "a cover gives the keys back to the page"
        );
        never_presents(&mut term, "a covered term's child never presents");
        step("covered", &term);
        term.cover(false);
        assert_eq!(term.place(RECT, scale, true), Ok(Some(scale)));
        show_frames(&mut term, &mut shared, &mut surface, &frame);
        step("uncovered", &term);
        assert_eq!(term.place(RECT, scale, false), Ok(None));
        never_presents(&mut term, "a hidden term's child never presents");
        drop(surface);
        close(window, term);
        drop(shared);
    }

    /// A term placed again at twice the toplevel's scale reports the new scale, which has
    /// the host remake its renderer and surface at that key; the next present attaches a
    /// buffer at the new size and scale.
    #[test]
    #[ignore = "needs a display: the farm runs it under Xvfb, weston and sway"]
    fn a_rescale_reports_the_new_scale_and_the_next_present_carries_it() {
        let (window, page) = white_toplevel();
        let mut term = term_over(&window, &page);
        let scale = window.scale_factor();
        assert_eq!(term.place(RECT, scale, true), Ok(Some(scale)));
        let child = term.child.as_ref().expect("a child after a shown place");
        let size = child.size();
        let (mut shared, mut surface, frame) = green_terminal(child);
        show_frames(&mut term, &mut shared, &mut surface, &frame);
        assert_eq!(
            term.child.as_ref().and_then(Child::attached),
            Some((size, scale))
        );
        assert_eq!(
            term.place(RECT, scale * 2, true),
            Ok(Some(scale * 2)),
            "a place at a new scale reports it, so the host remakes the surface"
        );
        drop(surface);
        drop(shared);
        let child = term.child.as_ref().expect("the same child");
        let rescaled = child.size();
        assert_eq!(rescaled, (size.0 * 2, size.1 * 2));
        let (mut shared, mut surface, frame) = green_terminal(child);
        show_frames(&mut term, &mut shared, &mut surface, &frame);
        assert_eq!(
            term.child.as_ref().and_then(Child::attached),
            Some((rescaled, scale * 2)),
            "the next present attaches a buffer at the new size and scale"
        );
        step("rescaled", &term);
        drop(surface);
        close(window, term);
        drop(shared);
    }
}
