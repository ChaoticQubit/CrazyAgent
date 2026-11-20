use super::{NativeEvent, SKIPPED};
use super::{schedule, sink};
use crate::native_term::a11y::Geometry;
use crate::native_term::linux::{Boards, Rect, SCALE_MAX, Term, rect};
use crate::native_term::render::{GPU_FAULT, Shared, Surface};
use crate::native_term::screen::Frame;
use crate::native_term::style::Style;
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::term::Term as Grid;
use gtk::glib::ControlFlow;
use gtk::prelude::*;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use tauri::ipc::Channel;

pub(super) use crate::native_term::linux::Boards as Clip;

/// The proof that code runs on GTK's main thread, which every GTK call needs; never sent
/// to another thread.
pub struct Main(PhantomData<*const ()>);

/// The page's webview as the host takes it: its GTK widget.
pub type Webview = gtk::Widget;

/// The main thread's marker, or none on another thread or before GTK starts. Output: the
/// marker, if any.
pub fn main() -> Option<Main> {
    gtk::is_initialized_main_thread().then_some(Main(PhantomData))
}

/// A view's GTK and drawing parts: its wgpu surface, dropped before the term whose child
/// it draws on, the term, the tick callback a frame callback wait has armed, the toplevel
/// and the page widget its place is measured in, its scale key, events channel and the
/// wall window's project.
pub struct View {
    surface: Option<Surface>,
    term: Term,
    tick: Rc<RefCell<Option<gtk::TickCallbackId>>>,
    toplevel: gtk::Window,
    page: gtk::Widget,
    pub(super) scale: u32,
    pub(super) events: Channel<NativeEvent>,
    pub(super) project: Option<String>,
}

/// The page widget a view opens over and its realized toplevel.
pub struct Parent {
    toplevel: gtk::Window,
    page: gtk::Widget,
}

/// The page widget and its toplevel for one open: refused while the page is in no window
/// or its toplevel has no GDK window, so the term's view is always made. Input: the
/// webview's widget. Output: both, or `native_terminal_surface_unavailable`.
pub fn parent(pointer: Webview) -> Result<Parent, &'static str> {
    let toplevel = pointer
        .toplevel()
        .and_then(|top| top.downcast::<gtk::Window>().ok())
        .ok_or("native_terminal_surface_unavailable")?;
    if toplevel.window().is_none() {
        return Err("native_terminal_surface_unavailable");
    }
    assert_eq!(
        pointer.toplevel().as_ref(),
        Some(toplevel.upcast_ref::<gtk::Widget>())
    );
    Ok(Parent {
        toplevel,
        page: pointer,
    })
}

/// The scale of the toplevel the page shows in. Input: the parent. Output: the scale.
pub fn parent_scale(parent: &Parent) -> f64 {
    f64::from(window_scale(&parent.toplevel))
}

/// Makes a view's term over its open place, its child hidden until the first shown place;
/// the label waits for the Linux screen reader slice. Inputs: the marker, the parent, the
/// page id, the scale key, the open rect in CSS pixels, the label, the events channel and
/// the wall window's project. Output: the view, `native_terminal_rect_invalid`, or
/// `native_terminal_surface_unavailable` when no child can be made.
#[allow(clippy::too_many_arguments)]
pub fn open_view(
    _mtm: Main,
    parent: Parent,
    page: u64,
    key: u32,
    rect: [f64; 4],
    _label: &str,
    _style: &Style,
    events: Channel<NativeEvent>,
    project: Option<String>,
) -> Result<View, &'static str> {
    let Parent {
        toplevel,
        page: widget,
    } = parent;
    let [x, y, width, height] = rect;
    let opened = [x, y, width.max(1.0), height.max(1.0)];
    let at = host_rect(&widget, &toplevel, opened).ok_or("native_terminal_rect_invalid")?;
    let term = Term::new(&toplevel, &widget, at, page, sink, Boards::shared())?;
    assert!(!term.shown());
    Ok(View {
        surface: None,
        term,
        tick: Rc::new(RefCell::new(None)),
        toplevel,
        page: widget,
        scale: key,
        events,
        project,
    })
}

/// Gives a view native focus, which it owes until its first shown place; the toplevel and
/// the app stay as they are. Input: the view.
pub fn focus_view(view: &View) {
    view.term.focus();
}

/// Puts a view over its page place and shows it, or hides it. The page fills its widget on
/// Linux, so the viewport is not read. Inputs: the view, the place in CSS pixels, the page's
/// viewport height and whether it shows. Output: the scale of a shown view, or none for a
/// hidden one, also when the place is out of bounds or the page is not realized.
pub fn place_view(view: &mut View, rect: [f64; 4], _viewport: f64, shown: bool) -> Option<f64> {
    let scale = window_scale(&view.toplevel);
    let at = shown
        .then(|| host_rect(&view.page, &view.toplevel, rect))
        .flatten();
    let drawn = view.term.place(at, scale);
    assert_eq!(drawn.is_some(), view.term.shown());
    drawn.map(f64::from)
}

/// Gives the view a new scale key: its surface, made on the old scale's renderer, goes,
/// and `fit` makes the next one. Inputs: the view and the key.
pub fn rescale(view: &mut View, key: u32) {
    view.surface = None;
    view.scale = key;
}

/// Sizes the view's surface to its child, making it on the renderer when the view has none.
/// The child holds whole logical pixels times its scale, which the host's rounded pixels
/// can miss by one, so the child's size wins. Inputs: the view, the renderer at its scale
/// and the host's size in device pixels. Output: whether the view has a surface.
pub fn fit(view: &mut View, shared: &mut Shared, _pixels: (u32, u32)) -> bool {
    let (width, height) = view.term.size();
    let scale = view.term.scale();
    let (width, height) = match shared.side_max() {
        Some(side_max) => (
            surface_side(width, scale, side_max),
            surface_side(height, scale, side_max),
        ),
        None => (width, height),
    };
    match view.surface.as_mut() {
        Some(surface) => shared.resize(surface, width, height).is_ok(),
        None => {
            let (display, window) = view.term.handles();
            let made = unsafe { shared.surface_raw(display, window, width, height) };
            view.surface = made.ok();
            view.surface.is_some()
        }
    }
}

/// One side of the view's wgpu surface: the child's side in device pixels, held to the
/// device's largest texture side rounded down to a whole number of the scale's pixels, as
/// a Wayland buffer at that scale must be. Inputs: the side, the scale it is drawn at, 1
/// to 8, and the largest side. Output: the side.
fn surface_side(side: u32, scale: i32, side_max: u32) -> u32 {
    assert!((1..=SCALE_MAX).contains(&scale));
    let scale = u32::try_from(scale).expect("a positive scale");
    let most = side_max - side_max % scale;
    assert!(
        most >= scale,
        "a largest texture side under one scaled pixel"
    );
    let fitted = side.min(most);
    assert!(fitted <= side_max && (fitted == side || fitted % scale == 0));
    fitted
}

/// Draws a frame into the view's surface and presents it on the child's frame pacing; the
/// host calls it only after `ready`. Inputs: the view, the renderer at its scale and the
/// frame. Output: ok, `native_terminal_gpu` when the GPU refused the frame, or the skipped
/// code when there is no surface, the view is hidden or the draw skipped.
pub fn paint(view: &mut View, shared: &mut Shared, frame: Frame) -> Result<(), &'static str> {
    let Some(surface) = view.surface.as_mut() else {
        return Err(SKIPPED);
    };
    if !view.term.shown() {
        return Err(SKIPPED);
    }
    view.term.presenting(surface.size());
    match shared.draw(surface, &frame) {
        Ok(()) => {
            view.term.presented();
            Ok(())
        }
        Err(code) => {
            view.term.not_presented();
            if code == GPU_FAULT {
                Err(GPU_FAULT)
            } else {
                Err(SKIPPED)
            }
        }
    }
}

/// Whether the view can present a dirty frame now: its child owes one that the last
/// present's frame callback no longer holds back. While held back it arms one GTK tick on
/// the page, which asks the host for a drain on the next frame clock tick, never while one
/// is armed or once the display connection broke. Input: the view. Output: the answer.
pub fn ready(view: &mut View) -> bool {
    view.term.changed();
    if view.term.ready() {
        return true;
    }
    if !view.term.gone() && view.tick.borrow().is_none() {
        let armed = Rc::clone(&view.tick);
        let id = view.page.add_tick_callback(move |_, _| {
            armed.borrow_mut().take();
            schedule();
            ControlFlow::Break
        });
        *view.tick.borrow_mut() = Some(id);
    }
    assert!(view.term.gone() || view.tick.borrow().is_some());
    false
}

/// Whether the view's last place showed it. Input: the view. Output: the answer.
pub fn shown(view: &View) -> bool {
    view.term.shown()
}

/// The scale of the toplevel the view shows in now. Input: the view. Output: the scale.
pub fn scale(view: &View) -> f64 {
    f64::from(window_scale(&view.toplevel))
}

/// The input method's marked text the view holds: none until the Linux preedit slice.
/// Input: the view. Output: none.
pub fn marked(_view: &View) -> Option<String> {
    None
}

/// Hands assistive tools what a frame showed: nothing until the Linux screen reader slice.
/// Inputs: the view, the geometry and the terminal.
pub fn drawn(_view: &View, _geometry: Geometry, _term: &Grid<VoidListener>) {}

/// Takes a view down in the order nothing can reach it after: its armed tick, its surface,
/// then the term with its child and input view. Input: the view. Output: its events channel.
pub fn dismiss_view(view: View) -> Channel<NativeEvent> {
    let View {
        surface,
        term,
        tick,
        events,
        ..
    } = view;
    if let Some(id) = tick.borrow_mut().take() {
        id.remove();
    }
    drop(surface);
    drop(term);
    assert!(tick.borrow().is_none());
    events
}

/// The toplevel's scale, held to 1 to 8. Input: the toplevel. Output: the scale.
fn window_scale(toplevel: &gtk::Window) -> i32 {
    toplevel.scale_factor().clamp(1, SCALE_MAX)
}

/// A CSS place in the page as a host rectangle in the toplevel's logical pixels. Inputs:
/// the page widget, its toplevel and the place. Output: the rectangle, or none while the
/// page is not in the toplevel or the place is not finite, out of bounds or under a pixel.
fn host_rect(page: &gtk::Widget, toplevel: &gtk::Window, css: [f64; 4]) -> Option<Rect> {
    let offset = page.translate_coordinates(toplevel, 0, 0)?;
    let [x, y, width, height] = css;
    rect((x, y, width, height), offset).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_term::linux::rows::{green_shared, pump_for, pump_until, white_toplevel};
    use crate::native_term::style::{FontSource, Theme};
    use std::time::Duration;

    /// A child wider than the device's largest texture side gets a surface side that is
    /// a whole number of its scale's pixels, as a Wayland buffer at that scale must be, or
    /// the compositor ends the connection with invalid_size (CR-1630).
    #[test]
    fn a_surface_side_past_the_texture_side_stays_a_whole_number_of_scaled_pixels() {
        assert_eq!(surface_side(300, 3, 8192), 300);
        for scale in 1..=SCALE_MAX {
            let scale_px = u32::try_from(scale).expect("a positive scale");
            let side = surface_side(16_384 * scale_px, scale, 8192);
            assert!(side <= 8192, "scale {scale}: side {side}");
            assert_eq!(side % scale_px, 0, "scale {scale}: side {side}");
        }
    }

    /// The style a row view opens with; the Linux host draws with its renderer's own
    /// style, so only the shape matters. Output: the style.
    fn row_style() -> Style {
        Style {
            family: "row",
            source: FontSource::Built(&[]),
            font_px: 13.0,
            blink: false,
            theme: Theme::default(),
        }
    }

    /// The page place the rows open and show at, inside the 640x480 page.
    const PLACE: [f64; 4] = [100.0, 80.0, 200.0, 150.0];

    /// The page's viewport height the host passes; Linux does not read it.
    const VIEWPORT: f64 = 480.0;

    /// The presents the tick row makes: a compositor may answer one present's frame
    /// callback before the next read, so the row needs one present held back among them.
    const PRESENTS: usize = 20;

    fn opened(page: &gtk::DrawingArea) -> View {
        let _ = Clip::new();
        let parent = parent(page.clone().upcast()).expect("a parent");
        let events = Channel::new(|_| Ok(()));
        open_view(
            main().expect("the main thread"),
            parent,
            9,
            100,
            PLACE,
            "row",
            &row_style(),
            events,
            None,
        )
        .expect("a view")
    }

    fn close(window: gtk::Window, view: View) {
        let _ = dismiss_view(view);
        unsafe {
            window.destroy();
        }
        pump_for(Duration::from_millis(100));
    }

    /// The page asks a view for focus at open, before the host's first place: the view
    /// owes it, and the first shown place applies it.
    #[test]
    #[ignore = "needs a display: the farm runs it under Xvfb and sway"]
    fn a_focus_asked_at_open_applies_at_the_first_shown_place() {
        let (window, page) = white_toplevel();
        let mut view = opened(&page);
        view.term.toplevel_focused();
        focus_view(&view);
        assert!(
            !view.term.focus_native(),
            "an unplaced view holds no native focus"
        );
        let scale = place_view(&mut view, PLACE, VIEWPORT, true);
        assert_eq!(scale, Some(f64::from(window.scale_factor())));
        assert!(
            view.term.focus_active(),
            "a focus asked at open applies at the first shown place"
        );
        close(window, view);
    }

    /// A shown view the page covers is placed hidden, as the host folds the cover into
    /// shown: the child leaves the screen, the keys go back to the page, nothing presents,
    /// and the next shown place brings it back (rv-code #653).
    #[test]
    #[ignore = "needs a display: the farm runs it under Xvfb and sway"]
    fn a_covered_place_hides_the_view_and_gives_the_keys_back() {
        let (window, page) = white_toplevel();
        let mut view = opened(&page);
        view.term.toplevel_focused();
        assert!(place_view(&mut view, PLACE, VIEWPORT, true).is_some());
        focus_view(&view);
        assert!(view.term.focus_active(), "a shown view takes focus");
        assert_eq!(
            place_view(&mut view, PLACE, VIEWPORT, false),
            None,
            "a covered place hides the view"
        );
        assert!(!shown(&view), "a covered place leaves the view hidden");
        assert!(
            !view.term.focus_native(),
            "a covered place gives the keys back to the page"
        );
        assert!(!ready(&mut view), "a covered view never presents");
        assert!(place_view(&mut view, PLACE, VIEWPORT, true).is_some());
        assert!(shown(&view), "the next shown place brings it back");
        close(window, view);
    }

    /// A dirty view whose child still waits on its last present's frame callback is not
    /// ready, and arms one tick that asks for a drain, which clears itself once it ran.
    /// Across repeated presents at least one is held back, and each held one arms a tick.
    #[test]
    #[ignore = "needs a display: the farm runs it under Xvfb, weston and sway"]
    fn a_view_held_back_by_its_frame_callback_arms_one_tick() {
        let (window, page) = white_toplevel();
        let mut view = opened(&page);
        let scale = place_view(&mut view, PLACE, VIEWPORT, true).expect("shown");
        let key = (scale * 100.0) as u32;
        let (mut shared, _) = green_shared(scale as i32);
        assert!(
            fit(&mut view, &mut shared, (0, 0)),
            "a surface on the child"
        );
        assert_eq!(view.scale, key);
        let mut held = 0;
        for _ in 0..PRESENTS {
            pump_until("the next frame ready", || ready(&mut view));
            let frame = Frame {
                rows: Vec::new(),
                keys: Vec::new(),
                fills: Vec::new(),
                cursor: None,
                preedit: None,
            };
            assert_eq!(paint(&mut view, &mut shared, frame), Ok(()));
            if ready(&mut view) {
                continue;
            }
            held += 1;
            assert!(
                view.tick.borrow().is_some(),
                "a view held back by its frame callback arms one tick"
            );
            pump_until("the tick ran", || view.tick.borrow().is_none());
        }
        assert!(
            held >= 1,
            "a view waiting on its frame callback is not ready after at least one of {PRESENTS} presents"
        );
        drop(shared);
        close(window, view);
    }

    /// A rescale drops the surface made on the old scale's renderer, and the next fit makes
    /// a fresh one at the child's size.
    #[test]
    #[ignore = "needs a display: the farm runs it under Xvfb, weston and sway"]
    fn a_rescale_drops_the_surface_and_the_next_fit_remakes_it() {
        let (window, page) = white_toplevel();
        let mut view = opened(&page);
        let scale = place_view(&mut view, PLACE, VIEWPORT, true).expect("shown");
        let (mut shared, _frame) = green_shared(scale as i32);
        assert!(fit(&mut view, &mut shared, (0, 0)));
        rescale(&mut view, 200);
        assert!(view.surface.is_none(), "a rescale drops the surface");
        assert_eq!(view.scale, 200);
        assert!(
            fit(&mut view, &mut shared, (1, 1)),
            "the next fit remakes it"
        );
        assert!(view.surface.is_some());
        let _ = view.surface.take();
        drop(shared);
        close(window, view);
    }

    /// An open place that is not finite is refused before any GTK part is made.
    #[test]
    #[ignore = "needs a display: the farm runs it under Xvfb, weston and sway"]
    fn an_open_place_that_is_not_finite_is_refused() {
        let (window, page) = white_toplevel();
        let _ = Clip::new();
        let parent = parent(page.clone().upcast()).expect("a parent");
        let events = Channel::new(|_| Ok(()));
        let bad = [f64::NAN, 0.0, 10.0, 10.0];
        let opened = open_view(
            main().expect("main"),
            parent,
            9,
            100,
            bad,
            "row",
            &row_style(),
            events,
            None,
        );
        assert_eq!(opened.err(), Some("native_terminal_rect_invalid"));
        unsafe {
            window.destroy();
        }
        pump_for(Duration::from_millis(100));
    }
}
