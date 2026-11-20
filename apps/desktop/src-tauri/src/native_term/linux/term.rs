use std::cell::Cell;
use std::rc::Rc;

use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

use super::clip::Clip;
use super::surface::{Child, Rect, SCALE_MAX};
use super::view::View;
use crate::native_term::event::Sink;

/// One native terminal's Linux parts: its input view and the child surface it draws into,
/// both made at open and hidden until the host's first shown place. Focus asked for before
/// that place is owed and applied right after it. The host drops its wgpu surface on the
/// child before the term.
pub struct Term {
    view: View,
    child: Child,
    placed: bool,
    shown: bool,
    focus_owed: Cell<bool>,
}

impl Term {
    /// Wires a term's view and child to a mapped toplevel and the page widget under it,
    /// the child hidden. Inputs: the toplevel, the page widget, the open rectangle in the
    /// toplevel's logical pixels, the view id, the sink and the host's boards. Output: the
    /// term, or `native_terminal_surface_unavailable` when either part cannot be made.
    pub fn new(
        toplevel: &gtk::Window,
        page: &gtk::Widget,
        rect: Rect,
        id: u64,
        sink: Sink,
        clip: Rc<Clip>,
    ) -> Result<Term, &'static str> {
        let view = View::new(toplevel, page, id, sink, clip)?;
        let mut child = Child::new(toplevel, page, rect)?;
        child.hide();
        Ok(Term {
            view,
            child,
            placed: false,
            shown: false,
            focus_owed: Cell::new(false),
        })
    }

    /// Gives the view native focus, as the page asks. The view refuses it before the first
    /// shown place, so until then it is owed and applied right after that place. Output:
    /// none.
    pub fn focus(&self) {
        if self.placed {
            self.view.focus(true);
        } else {
            self.focus_owed.set(true);
        }
    }

    /// Puts the term over its host rectangle at a scale and shows it, or takes it off
    /// screen with none. The first shown place applies owed focus. Inputs: the rectangle in
    /// the toplevel's logical pixels, or none, and the scale, 1 to 8. Output: the scale the
    /// child now draws at, or none while hidden.
    pub fn place(&mut self, rect: Option<Rect>, scale: i32) -> Option<i32> {
        assert!((1..=SCALE_MAX).contains(&scale));
        let Some(rect) = rect else {
            if self.shown {
                self.child.hide();
            }
            self.view.place(None);
            self.shown = false;
            return None;
        };
        self.child.place_scaled(rect, scale);
        self.child.show();
        self.view.place(Some(rect));
        self.placed = true;
        self.shown = true;
        if self.focus_owed.take() {
            self.view.focus(true);
        }
        assert!(!self.focus_owed.get());
        Some(self.child.scale())
    }

    /// Covers or uncovers the term as the page reports an element over it. A cover gives
    /// keys back to the page and takes the child off screen; the host's next shown place
    /// puts the child back. Input: covered or not. Output: none.
    pub fn cover(&mut self, covered: bool) {
        self.view.cover(covered);
        if covered && self.shown {
            self.child.hide();
            self.shown = false;
        }
    }

    /// Whether the last place showed the term. Output: the answer.
    pub fn shown(&self) -> bool {
        self.shown
    }

    /// Whether the host may present now: the term shows and its child owes a frame that its
    /// last present's callback no longer holds back. Output: the answer.
    pub fn ready(&mut self) -> bool {
        self.shown && self.child.ready()
    }

    /// Whether the child's display connection broke, so it never presents again. Output:
    /// the answer.
    pub fn gone(&self) -> bool {
        self.child.gone()
    }

    /// The size the host's wgpu surface on the child takes. Output: width and height in
    /// device pixels.
    pub fn size(&self) -> (u32, u32) {
        self.child.size()
    }

    /// The scale the child is placed at, 1 to 8. Output: it.
    pub fn scale(&self) -> i32 {
        self.child.scale()
    }

    /// The child's display and window handles a wgpu surface is made on, live as long as
    /// the term. Output: both.
    pub fn handles(&self) -> (RawDisplayHandle, RawWindowHandle) {
        self.child.handles()
    }

    /// Notes a present about to happen at the size the host's wgpu surface is configured
    /// to, which may be held under the child's own size, and at the child's scale. Input:
    /// the configured size in device pixels. Output: none.
    pub fn presenting(&mut self, size: (u32, u32)) {
        assert!(self.shown, "a present only while shown");
        assert!(size.0 >= 1 && size.1 >= 1, "a configured surface size");
        self.child.presenting(size, self.child.scale());
    }

    /// Notes that the present went out. Output: none.
    pub fn presented(&mut self) {
        self.child.presented();
    }

    /// Notes that the draw failed before presenting, so the frame is owed again. Output:
    /// none.
    pub fn not_presented(&mut self) {
        self.child.not_presented();
    }

    /// Notes a change to draw. Output: none.
    pub fn changed(&mut self) {
        self.child.changed();
    }
}

#[cfg(test)]
impl Term {
    /// Whether the view holds native focus, for rows outside this module. Output: the answer.
    pub fn focus_native(&self) -> bool {
        self.view.focus_state().is_native()
    }

    /// Whether the view holds active native focus, for rows outside this module. Output:
    /// the answer.
    pub fn focus_active(&self) -> bool {
        self.view.focus_state().active()
    }

    /// Stands in for the toplevel's focus coming in, for rows outside this module. Output:
    /// none.
    pub fn toplevel_focused(&self) {
        self.view.toplevel_focused();
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
            RECT,
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
            term.presenting(surface.size());
            shared.draw(surface, frame).expect("a present");
            term.presented();
            term.changed();
        }
        pump_for(Duration::from_millis(200));
    }

    /// Writes a step for the farm's screenshot: the child's device rectangle and, on X11,
    /// its root origin. Inputs: the step and the term.
    fn step(name: &str, term: &Term) {
        let child = &term.child;
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

    /// A term made at open has its child already, hidden: the farm's screenshot reads the
    /// page's white over its rectangle, with no unpainted window, and it owes no frame
    /// until the host's first shown place, which shows the child green.
    #[test]
    #[ignore = "needs a display: the farm runs it under Xvfb, weston and sway"]
    fn a_term_made_at_open_stays_hidden_until_its_first_shown_place() {
        let (window, page) = white_toplevel();
        let mut term = term_over(&window, &page);
        never_presents(
            &mut term,
            "a term made at open never presents before its first place",
        );
        assert!(!term.shown(), "a term made at open is hidden");
        step("hidden", &term);
        let scale = window.scale_factor();
        assert_eq!(term.place(Some(RECT), scale), Some(scale));
        let (mut shared, mut surface, frame) = green_terminal(&term.child);
        show_frames(&mut term, &mut shared, &mut surface, &frame);
        step("placed", &term);
        drop(surface);
        close(window, term);
        drop(shared);
    }

    /// A toplevel realized but not mapped has a GDK window, so the view could be wired,
    /// but no child can be made over it: the open fails with the plain code, which the
    /// host answers the page with.
    #[test]
    #[ignore = "needs a display: the farm runs it under Xvfb, weston and sway"]
    fn an_open_over_an_unmapped_toplevel_fails() {
        gtk::init().expect("a display");
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        let page = gtk::DrawingArea::new();
        window.add(&page);
        window.realize();
        page.realize();
        assert!(
            window.window().is_some(),
            "a realized toplevel has a GDK window"
        );
        let opened = Term::new(
            &window,
            page.upcast_ref(),
            RECT,
            ID,
            dropped,
            Rc::new(Clip::private()),
        );
        assert_eq!(
            opened.err(),
            Some("native_terminal_surface_unavailable"),
            "an open over an unmapped toplevel fails"
        );
        unsafe {
            window.destroy();
        }
        pump_for(Duration::from_millis(100));
    }

    /// The page asks a new term for focus before the host's first place, which the view
    /// refuses unplaced; the term owes it and applies it right after that place, once. A
    /// focus asked once the term shows applies at once, and a cover takes it back.
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
        assert_eq!(term.place(Some(RECT), scale), Some(scale));
        assert!(
            term.view.focus_state().active(),
            "focus asked before the first place is applied right after it"
        );
        assert_eq!(term.place(None, scale), None);
        assert!(
            !term.view.focus_state().is_native(),
            "a hidden term holds no native focus"
        );
        assert_eq!(term.place(Some(RECT), scale), Some(scale));
        assert!(
            !term.view.focus_state().is_native(),
            "owed focus applies once, not at every place"
        );
        term.focus();
        assert!(
            term.view.focus_state().active(),
            "focus asked after the first place applies at once"
        );
        term.cover(true);
        assert!(
            !term.view.focus_state().is_native(),
            "a cover gives the keys back to the page"
        );
        close(window, term);
    }

    /// A shown term is covered: its child leaves the screen, so the farm's screenshot reads
    /// the page's white over the rectangle, and it never presents while covered; uncovered
    /// and placed again, it draws green again. A hidden place takes the child off screen too.
    #[test]
    #[ignore = "needs a display: the farm runs it under Xvfb, weston and sway"]
    fn a_cover_takes_the_child_off_screen_until_the_next_shown_place() {
        let (window, page) = white_toplevel();
        let mut term = term_over(&window, &page);
        let scale = window.scale_factor();
        assert_eq!(term.place(Some(RECT), scale), Some(scale));
        let child = &term.child;
        let (mut shared, mut surface, frame) = green_terminal(child);
        show_frames(&mut term, &mut shared, &mut surface, &frame);
        step("shown", &term);
        term.cover(true);
        never_presents(&mut term, "a covered term's child never presents");
        step("covered", &term);
        term.cover(false);
        assert_eq!(term.place(Some(RECT), scale), Some(scale));
        show_frames(&mut term, &mut shared, &mut surface, &frame);
        step("uncovered", &term);
        assert_eq!(term.place(None, scale), None);
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
        assert_eq!(term.place(Some(RECT), scale), Some(scale));
        let child = &term.child;
        let size = child.size();
        let (mut shared, mut surface, frame) = green_terminal(child);
        show_frames(&mut term, &mut shared, &mut surface, &frame);
        assert_eq!(term.child.attached(), Some((size, scale)));
        assert_eq!(
            term.place(Some(RECT), scale * 2),
            Some(scale * 2),
            "a place at a new scale reports it, so the host remakes the surface"
        );
        drop(surface);
        drop(shared);
        let child = &term.child;
        let rescaled = child.size();
        assert_eq!(rescaled, (size.0 * 2, size.1 * 2));
        let (mut shared, mut surface, frame) = green_terminal(child);
        show_frames(&mut term, &mut shared, &mut surface, &frame);
        assert_eq!(
            term.child.attached(),
            Some((rescaled, scale * 2)),
            "the next present attaches a buffer at the new size and scale"
        );
        step("rescaled", &term);
        drop(surface);
        close(window, term);
        drop(shared);
    }
}
