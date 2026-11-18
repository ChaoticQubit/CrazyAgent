use std::cell::Cell;
use std::ffi::c_void;
use std::io::ErrorKind;
use std::ops::RangeInclusive;
use std::ptr::NonNull;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::gdk::{Display, Window};
use gtk::glib::SignalHandlerId;
use gtk::glib::translate::ToGlibPtr;
use gtk::prelude::*;
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
    XlibDisplayHandle, XlibWindowHandle,
};
use wayland_backend::client::{Backend, ObjectId, WaylandError};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_subcompositor::WlSubcompositor;
use wayland_client::protocol::wl_subsurface::WlSubsurface;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};

/// The plain code for a toplevel no child surface can be made in: not mapped, a display
/// that is neither X11 nor Wayland, or a server without the parts a child needs.
const UNAVAILABLE: &str = "native_terminal_surface_unavailable";

/// The largest host rectangle edge, origin or webview offset, in logical pixels.
pub const RECT_MAX: i32 = 16_384;

/// The largest display scale a child is made at.
const SCALE_MAX: i32 = 8;

/// The X11 Shape extension's input shape kind, which XFixes empties so clicks reach the
/// toplevel.
const SHAPE_INPUT: i32 = 2;

/// The refresh rate an X11 child paces to when GDK reports none, in millihertz.
const REFRESH_MHZ_DEFAULT: i32 = 60_000;

/// The refresh rates an X11 child paces to, in millihertz: 1 Hz to 1 kHz.
const REFRESH_MHZ: RangeInclusive<i32> = 1_000..=1_000_000;

/// A host rectangle in the toplevel's logical pixels: where the child sits and its size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    /// The rectangle in device pixels. Input: the display scale, 1 to 8. Output: x, y,
    /// width and height.
    pub fn device(self, scale: i32) -> (i32, i32, u32, u32) {
        assert!(
            (1..=SCALE_MAX).contains(&scale),
            "a display scale of 1 to {SCALE_MAX}, got {scale}"
        );
        assert!(self.w >= 1 && self.h >= 1);
        let width = u32::try_from(self.w * scale).expect("a positive width");
        let height = u32::try_from(self.h * scale).expect("a positive height");
        (self.x * scale, self.y * scale, width, height)
    }
}

/// The host rectangle from the page's CSS pixels, which are GTK logical pixels at zoom 1,
/// and the webview's offset in its toplevel. Inputs: x, y, width and height, and the
/// offset. Output: the rectangle, or `native_terminal_rect_invalid` for a value that is not
/// finite, is out of bounds, or a size under one pixel.
pub fn rect(page: (f64, f64, f64, f64), offset: (i32, i32)) -> Result<Rect, &'static str> {
    let (x, y, w, h) = page;
    let max = f64::from(RECT_MAX);
    let bounded = [x, y, w, h]
        .iter()
        .all(|value| value.is_finite() && value.abs() <= max);
    let offset_bounded = offset.0.abs() <= RECT_MAX && offset.1.abs() <= RECT_MAX;
    if !bounded || !offset_bounded || w < 1.0 || h < 1.0 {
        return Err("native_terminal_rect_invalid");
    }
    let rect = Rect {
        x: x.round() as i32 + offset.0,
        y: y.round() as i32 + offset.1,
        w: w.round() as i32,
        h: h.round() as i32,
    };
    assert!((1..=RECT_MAX).contains(&rect.w) && (1..=RECT_MAX).contains(&rect.h));
    Ok(rect)
}

/// When a child may present: only while shown, only with a change to draw, never before
/// the last present's frame callback, and never after its toplevel unmapped. A FIFO present
/// waits for the last present's callback, which a hidden or unmapped surface may never get
/// (spike run 3: a present after a null-buffer hide blocked the main thread 40 s).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pacing {
    shown: bool,
    waiting: bool,
    dirty: bool,
    gone: bool,
}

impl Pacing {
    /// A new child's pacing: shown, with its first frame owed. Output: it.
    const fn new() -> Pacing {
        Pacing {
            shown: true,
            waiting: false,
            dirty: true,
            gone: false,
        }
    }

    /// Whether a present may happen now. Output: the answer.
    fn ready(&self) -> bool {
        self.shown && self.dirty && !self.waiting && !self.gone
    }

    /// Notes a change to draw: output, a size or a style.
    fn changed(&mut self) {
        self.dirty = true;
    }

    /// Notes a present about to happen; the next waits for its frame callback.
    fn presenting(&mut self) {
        assert!(self.ready(), "a present only when ready: {self:?}");
        self.dirty = false;
        self.waiting = true;
    }

    /// Notes a present that did not happen, so its frame is owed again.
    fn not_presented(&mut self) {
        self.waiting = false;
        self.dirty = true;
    }

    /// Notes the last present's frame callback.
    fn frame_done(&mut self) {
        self.waiting = false;
    }

    /// Notes the child leaving the screen.
    fn hide(&mut self) {
        self.shown = false;
    }

    /// Notes the child back on screen; an X11 window lost its pixels, so it is redrawn.
    fn show(&mut self) {
        self.shown = true;
        self.dirty = true;
    }

    /// Notes the toplevel unmapping, after which the child never presents again.
    fn gone(&mut self) {
        self.gone = true;
    }

    /// Whether the toplevel unmapped. Output: the answer.
    fn is_gone(&self) -> bool {
        self.gone
    }
}

/// Marks a child gone from its toplevel's unmap or unrealize handler. Input: the shared
/// pacing. Output: none.
fn mark_gone(pacing: &Cell<Pacing>) {
    let mut next = pacing.get();
    next.gone();
    pacing.set(next);
}

/// The Wayland queue state: whether the last present's frame callback has come.
#[derive(Default)]
struct Frames {
    done: bool,
}

delegate_noop!(Frames: WlCompositor);
delegate_noop!(Frames: WlSubcompositor);
delegate_noop!(Frames: WlSubsurface);
delegate_noop!(Frames: WlRegion);
delegate_noop!(Frames: ignore WlSurface);

impl Dispatch<WlRegistry, GlobalListContents> for Frames {
    /// Ignores registry changes after the first roundtrip. Input: the event. Output: none.
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlCallback, ()> for Frames {
    /// Notes a frame callback's done event. Input: the event. Output: none.
    fn event(
        state: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.done = true;
        }
    }
}

/// An X11 child window on GDK's own Xlib display.
struct X11 {
    xlib: x11_dl::xlib::Xlib,
    gdk: gdkx11::X11Display,
    display: *mut x11_dl::xlib::Display,
    screen: i32,
    window: u64,
}

impl X11 {
    /// Runs Xlib calls on the child window under a GDK error trap, so a window the server
    /// already destroyed never reaches GDK's fatal X error handler. Input: the calls, given
    /// the library, display and window. Output: none.
    fn trapped(&self, calls: impl FnOnce(&x11_dl::xlib::Xlib, *mut x11_dl::xlib::Display, u64)) {
        self.gdk.error_trap_push();
        calls(&self.xlib, self.display, self.window);
        unsafe { (self.xlib.XFlush)(self.display) };
        self.gdk.error_trap_pop_ignored();
    }

    /// Moves the child to the root, unmapped, before GDK destroys the toplevel, so the
    /// server keeps the window wgpu still draws into until the host drops both. Output:
    /// none.
    fn to_root(&self) {
        self.trapped(|xlib, display, window| unsafe {
            let root = (xlib.XDefaultRootWindow)(display);
            (xlib.XUnmapWindow)(display, window);
            (xlib.XReparentWindow)(display, window, root, 0, 0);
        });
    }
}

/// A desynced Wayland subsurface of the toplevel's surface, made on GTK's own connection
/// with its own event queue for the frame callbacks of its presents.
struct Wayland {
    _gdk: Display,
    display: NonNull<c_void>,
    conn: Connection,
    queue: EventQueue<Frames>,
    frames: Frames,
    surface: WlSurface,
    subsurface: WlSubsurface,
    parent: WlSurface,
}

impl Wayland {
    /// Reads this queue's events without blocking and dispatches them. Output: whether the
    /// last present's frame callback has come, or none when the connection broke.
    fn frame_done(&mut self) -> Option<bool> {
        self.queue.dispatch_pending(&mut self.frames).ok()?;
        if let Some(guard) = self.queue.prepare_read() {
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(error)) if error.kind() == ErrorKind::WouldBlock => {}
                Err(_) => return None,
            }
        }
        self.queue.dispatch_pending(&mut self.frames).ok()?;
        Some(self.frames.done)
    }
}

/// Sends queued Wayland requests; a broken connection leaves the child gone. Inputs: the
/// connection and the pacing. Output: none.
fn flush(conn: &Connection, pacing: &Cell<Pacing>) {
    match conn.flush() {
        Ok(()) => {}
        Err(WaylandError::Io(error)) if error.kind() == ErrorKind::WouldBlock => {}
        Err(_) => mark_gone(pacing),
    }
}

/// The platform window of a child.
enum Native {
    X11 {
        x11: Rc<X11>,
        interval: Duration,
        presented: Option<Instant>,
    },
    Wayland(Box<Wayland>),
}

/// A child surface of a GTK toplevel that wgpu draws a terminal into: an X11 child window
/// with an empty input shape, or a desynced Wayland subsurface with an empty input region,
/// on GTK's own display connection so it stacks with the toplevel. Clicks pass through to
/// the toplevel. Once the toplevel unmaps the child never presents again; the host makes a
/// new one. The host drops the wgpu surface before the child.
pub struct Child {
    native: Native,
    pacing: Rc<Cell<Pacing>>,
    toplevel: gtk::Window,
    handlers: Vec<SignalHandlerId>,
    rect: Rect,
    scale: i32,
}

impl Child {
    /// Makes a child over a host rectangle of a mapped toplevel, shown and owing its first
    /// frame. Inputs: the toplevel and the rectangle. Output: the child, or
    /// `native_terminal_surface_unavailable`.
    pub fn new(toplevel: &gtk::Window, rect: Rect) -> Result<Child, &'static str> {
        let gdk = toplevel.window().ok_or(UNAVAILABLE)?;
        let scale = toplevel.scale_factor();
        if !gdk.is_viewable() || !(1..=SCALE_MAX).contains(&scale) {
            return Err(UNAVAILABLE);
        }
        let display = gdk.display();
        let native = if let Some(x11) = display.downcast_ref::<gdkx11::X11Display>() {
            x11_child(x11, &gdk, rect.device(scale))?
        } else if display.type_().name() == "GdkWaylandDisplay" {
            Native::Wayland(Box::new(wayland_child(&display, &gdk, rect, scale)?))
        } else {
            return Err(UNAVAILABLE);
        };
        let pacing = Rc::new(Cell::new(Pacing::new()));
        let unmap = {
            let pacing = Rc::clone(&pacing);
            toplevel.connect_unmap(move |_| mark_gone(&pacing))
        };
        let root = match &native {
            Native::X11 { x11, .. } => Some(Rc::clone(x11)),
            Native::Wayland(_) => None,
        };
        let unrealize = {
            let pacing = Rc::clone(&pacing);
            toplevel.connect_unrealize(move |_| {
                mark_gone(&pacing);
                if let Some(x11) = &root {
                    x11.to_root();
                }
            })
        };
        let child = Child {
            native,
            pacing,
            toplevel: toplevel.clone(),
            handlers: vec![unmap, unrealize],
            rect,
            scale,
        };
        assert!(child.pacing.get().ready());
        Ok(child)
    }

    /// The raw handles wgpu draws through, for `Shared::surface_raw`; both stay valid until
    /// the child drops. Output: the display and window handles.
    pub fn handles(&self) -> (RawDisplayHandle, RawWindowHandle) {
        match &self.native {
            Native::X11 { x11, .. } => (
                RawDisplayHandle::Xlib(XlibDisplayHandle::new(
                    NonNull::new(x11.display.cast::<c_void>()),
                    x11.screen,
                )),
                RawWindowHandle::Xlib(XlibWindowHandle::new(x11.window)),
            ),
            Native::Wayland(wayland) => {
                let surface = NonNull::new(wayland.surface.id().as_ptr().cast::<c_void>())
                    .expect("a live surface until the child drops");
                (
                    RawDisplayHandle::Wayland(WaylandDisplayHandle::new(wayland.display)),
                    RawWindowHandle::Wayland(WaylandWindowHandle::new(surface)),
                )
            }
        }
    }

    /// The display scale the child draws at. Output: 1 to 8.
    pub fn scale(&self) -> i32 {
        self.scale
    }

    /// The child's place in the toplevel, in device pixels. Output: x, y, width, height.
    pub fn device_rect(&self) -> (i32, i32, u32, u32) {
        self.rect.device(self.scale)
    }

    /// The size a wgpu surface on the child takes. Output: width and height in device
    /// pixels.
    pub fn size(&self) -> (u32, u32) {
        let (_, _, width, height) = self.device_rect();
        (width, height)
    }

    /// Moves the child to a new host rectangle at the toplevel's current scale and owes a
    /// frame; the host resizes the wgpu surface when `size` changed. Input: the rectangle.
    /// Output: none.
    pub fn place(&mut self, rect: Rect) {
        let scale = self.toplevel.scale_factor();
        if self.pacing.get().is_gone() || !(1..=SCALE_MAX).contains(&scale) {
            return;
        }
        let rescaled = scale != self.scale;
        self.rect = rect;
        self.scale = scale;
        match &self.native {
            Native::X11 { x11, .. } => {
                let (x, y, width, height) = rect.device(scale);
                x11.trapped(|xlib, display, window| unsafe {
                    (xlib.XMoveResizeWindow)(display, window, x, y, width, height);
                });
            }
            Native::Wayland(wayland) => {
                wayland.subsurface.set_position(rect.x, rect.y);
                if rescaled {
                    wayland.surface.set_buffer_scale(scale);
                }
                wayland.parent.commit();
                flush(&wayland.conn, &self.pacing);
            }
        }
        self.changed();
    }

    /// Takes the child off screen: X11 unmaps it, Wayland places it below the toplevel's
    /// surface, whose opaque page then covers it (spike run 3: passes on weston and sway).
    /// Output: none.
    pub fn hide(&mut self) {
        let mut pacing = self.pacing.get();
        if pacing.is_gone() {
            return;
        }
        pacing.hide();
        self.pacing.set(pacing);
        match &self.native {
            Native::X11 { x11, .. } => x11.trapped(|xlib, display, window| unsafe {
                (xlib.XUnmapWindow)(display, window);
            }),
            Native::Wayland(wayland) => {
                wayland.subsurface.place_below(&wayland.parent);
                wayland.parent.commit();
                flush(&wayland.conn, &self.pacing);
            }
        }
    }

    /// Puts the child back on screen and owes a frame. Output: none.
    pub fn show(&mut self) {
        let mut pacing = self.pacing.get();
        if pacing.is_gone() {
            return;
        }
        pacing.show();
        self.pacing.set(pacing);
        match &self.native {
            Native::X11 { x11, .. } => x11.trapped(|xlib, display, window| unsafe {
                (xlib.XMapRaised)(display, window);
            }),
            Native::Wayland(wayland) => {
                wayland.subsurface.place_above(&wayland.parent);
                wayland.parent.commit();
                flush(&wayland.conn, &self.pacing);
            }
        }
    }

    /// Notes a change to draw. Output: none.
    pub fn changed(&mut self) {
        let mut pacing = self.pacing.get();
        pacing.changed();
        self.pacing.set(pacing);
    }

    /// Whether the host may present now, after reading the last present's frame callback
    /// without blocking: on Wayland from this queue, on X11 one refresh after the present.
    /// Output: the answer.
    pub fn ready(&mut self) -> bool {
        let mut pacing = self.pacing.get();
        if pacing.waiting && !pacing.is_gone() {
            let done = match &mut self.native {
                Native::X11 {
                    interval,
                    presented,
                    ..
                } => Some(presented.is_some_and(|at| at.elapsed() >= *interval)),
                Native::Wayland(wayland) => wayland.frame_done(),
            };
            match done {
                Some(true) => pacing.frame_done(),
                Some(false) => {}
                None => pacing.gone(),
            }
            self.pacing.set(pacing);
        }
        pacing.ready()
    }

    /// Notes a present about to happen, right before the host draws; on Wayland it asks
    /// for the frame callback that the present's commit carries. Output: none.
    pub fn presenting(&mut self) {
        let mut pacing = self.pacing.get();
        pacing.presenting();
        self.pacing.set(pacing);
        match &mut self.native {
            Native::X11 { presented, .. } => *presented = Some(Instant::now()),
            Native::Wayland(wayland) => {
                wayland.frames.done = false;
                let _callback = wayland.surface.frame(&wayland.queue.handle(), ());
            }
        }
    }

    /// Notes that the host's draw failed before presenting, so the frame is owed again.
    /// Output: none.
    pub fn not_presented(&mut self) {
        let mut pacing = self.pacing.get();
        pacing.not_presented();
        self.pacing.set(pacing);
    }
}

#[cfg(test)]
impl Child {
    /// Where the child sits on the X11 root, for the farm's pixel checks. Output: the root
    /// origin, or none on Wayland, where the toplevel sits at the output's origin.
    fn root_origin(&self) -> Option<(i32, i32)> {
        let Native::X11 { x11, .. } = &self.native else {
            return None;
        };
        let (mut x, mut y, mut over) = (0, 0, 0);
        let found = unsafe {
            let root = (x11.xlib.XDefaultRootWindow)(x11.display);
            (x11.xlib.XTranslateCoordinates)(
                x11.display,
                x11.window,
                root,
                0,
                0,
                &mut x,
                &mut y,
                &mut over,
            )
        };
        (found != 0).then_some((x, y))
    }
}

impl Drop for Child {
    /// Disconnects the toplevel handlers and destroys the window or subsurface; a gone
    /// Wayland toplevel's surface is not committed, since GDK destroyed it. Output: none.
    fn drop(&mut self) {
        for handler in self.handlers.drain(..) {
            self.toplevel.disconnect(handler);
        }
        let gone = self.pacing.get().is_gone();
        match &self.native {
            Native::X11 { x11, .. } => x11.trapped(|xlib, display, window| unsafe {
                (xlib.XDestroyWindow)(display, window);
            }),
            Native::Wayland(wayland) => {
                wayland.subsurface.destroy();
                wayland.surface.destroy();
                if !gone {
                    wayland.parent.commit();
                }
                flush(&wayland.conn, &self.pacing);
            }
        }
    }
}

/// Makes an X11 child window of the toplevel with an empty input shape, mapped on top, and
/// paced to the toplevel's monitor refresh. Inputs: GDK's X11 display, the toplevel's GDK
/// window and the device rectangle. Output: the child, or the plain code.
fn x11_child(
    gdk: &gdkx11::X11Display,
    toplevel: &Window,
    device: (i32, i32, u32, u32),
) -> Result<Native, &'static str> {
    let parent = toplevel
        .downcast_ref::<gdkx11::X11Window>()
        .ok_or(UNAVAILABLE)?
        .xid();
    let raw: *mut gdkx11::ffi::GdkX11Display = gdk.to_glib_none().0;
    let display =
        unsafe { gdkx11::ffi::gdk_x11_display_get_xdisplay(raw) }.cast::<x11_dl::xlib::Display>();
    if display.is_null() {
        return Err(UNAVAILABLE);
    }
    let xlib = x11_dl::xlib::Xlib::open().map_err(|_| UNAVAILABLE)?;
    let xfixes = x11_dl::xfixes::Xlib::open().map_err(|_| UNAVAILABLE)?;
    gdk.error_trap_push();
    let (screen, window) = unsafe {
        let screen = (xlib.XDefaultScreen)(display);
        let (x, y, width, height) = device;
        let window = (xlib.XCreateSimpleWindow)(display, parent, x, y, width, height, 0, 0, 0);
        if window != 0 {
            let region = (xfixes.XFixesCreateRegion)(display, std::ptr::null_mut(), 0);
            (xfixes.XFixesSetWindowShapeRegion)(display, window, SHAPE_INPUT, 0, 0, region);
            (xfixes.XFixesDestroyRegion)(display, region);
            (xlib.XMapRaised)(display, window);
        }
        (screen, window)
    };
    let failed = gdk.error_trap_pop() != 0;
    let x11 = Rc::new(X11 {
        xlib,
        gdk: gdk.clone(),
        display,
        screen,
        window,
    });
    if window == 0 || failed {
        if window != 0 {
            x11.trapped(|xlib, display, window| unsafe {
                (xlib.XDestroyWindow)(display, window);
            });
        }
        return Err(UNAVAILABLE);
    }
    let refresh = gdk
        .upcast_ref::<Display>()
        .monitor_at_window(toplevel)
        .map_or(REFRESH_MHZ_DEFAULT, |monitor| monitor.refresh_rate());
    let refresh = if REFRESH_MHZ.contains(&refresh) {
        refresh
    } else {
        REFRESH_MHZ_DEFAULT
    };
    let interval =
        Duration::from_micros(1_000_000_000 / u64::try_from(refresh).expect("a positive rate"));
    assert!(interval >= Duration::from_millis(1) && interval <= Duration::from_secs(1));
    Ok(Native::X11 {
        x11,
        interval,
        presented: None,
    })
}

/// Makes a desynced subsurface of the toplevel's surface on GDK's own Wayland connection,
/// with its own event queue, an empty input region and the toplevel's buffer scale. The
/// toplevel's surface pointer is borrowed from GDK, which destroys it on unmap; the child's
/// unmap handler marks it gone before any later use. Inputs: GDK's display, the toplevel's
/// GDK window, the logical rectangle and the scale. Output: the subsurface, or the code.
fn wayland_child(
    gdk: &Display,
    toplevel: &Window,
    rect: Rect,
    scale: i32,
) -> Result<Wayland, &'static str> {
    let raw_display: *mut gtk::gdk::ffi::GdkDisplay = gdk.to_glib_none().0;
    let raw_window: *mut gtk::gdk::ffi::GdkWindow = toplevel.to_glib_none().0;
    let display =
        unsafe { gdk_wayland_sys::gdk_wayland_display_get_wl_display(raw_display.cast()) };
    let parent = unsafe { gdk_wayland_sys::gdk_wayland_window_get_wl_surface(raw_window.cast()) };
    let display = NonNull::new(display.cast::<c_void>()).ok_or(UNAVAILABLE)?;
    if parent.is_null() {
        return Err(UNAVAILABLE);
    }
    let backend = unsafe { Backend::from_foreign_display(display.as_ptr().cast()) };
    let conn = Connection::from_backend(backend);
    let (globals, mut queue) = registry_queue_init::<Frames>(&conn).map_err(|_| UNAVAILABLE)?;
    let handle = queue.handle();
    let compositor: WlCompositor = globals.bind(&handle, 3..=4, ()).map_err(|_| UNAVAILABLE)?;
    let subcompositor: WlSubcompositor =
        globals.bind(&handle, 1..=1, ()).map_err(|_| UNAVAILABLE)?;
    let parent = unsafe { ObjectId::from_ptr(WlSurface::interface(), parent.cast()) }
        .map_err(|_| UNAVAILABLE)?;
    let parent = WlSurface::from_id(&conn, parent).map_err(|_| UNAVAILABLE)?;
    let surface = compositor.create_surface(&handle, ());
    let subsurface = subcompositor.get_subsurface(&surface, &parent, &handle, ());
    subcompositor.destroy();
    subsurface.set_position(rect.x, rect.y);
    subsurface.set_desync();
    let region = compositor.create_region(&handle, ());
    surface.set_input_region(Some(&region));
    region.destroy();
    surface.set_buffer_scale(scale);
    surface.commit();
    parent.commit();
    let mut frames = Frames::default();
    queue.roundtrip(&mut frames).map_err(|_| UNAVAILABLE)?;
    Ok(Wayland {
        _gdk: gdk.clone(),
        display,
        conn,
        queue,
        frames,
        surface,
        subsurface,
        parent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_term::render::{Shared, Surface};
    use crate::native_term::screen::Frame;
    use crate::native_term::style::{Style, Theme, font_for};
    use alacritty_terminal::vte::ansi::Rgb;
    use serde_json::{Value, json};
    use std::path::PathBuf;

    /// How long the display row waits for one step: the window to map or a frame callback.
    const STEP_WAIT: Duration = Duration::from_secs(10);

    /// How long the display row waits for the farm to take a step's screenshot.
    const SHOT_WAIT: Duration = Duration::from_secs(60);

    /// The longest one present may take: under the 16.7 ms of a 60 Hz output, so a present
    /// that waited on a frame callback fails the row.
    const PRESENT_MS_MAX: f64 = 12.0;

    /// How many presents the display row paces back to back.
    const FRAMES: usize = 60;

    /// The child's colour, which the farm's pixel checks look for.
    const GREEN: Rgb = Rgb { r: 0, g: 255, b: 0 };

    #[test]
    fn a_host_rect_is_finite_bounded_and_at_least_one_pixel() {
        assert_eq!(
            rect((10.4, 20.6, 400.0, 240.0), (0, 30)),
            Ok(Rect {
                x: 10,
                y: 51,
                w: 400,
                h: 240
            })
        );
        assert_eq!(
            rect((-3.0, 0.0, 1.0, 1.0), (0, 0)),
            Ok(Rect {
                x: -3,
                y: 0,
                w: 1,
                h: 1
            })
        );
        let max = f64::from(RECT_MAX);
        for bad in [
            (f64::NAN, 0.0, 10.0, 10.0),
            (0.0, f64::INFINITY, 10.0, 10.0),
            (0.0, 0.0, 0.5, 10.0),
            (0.0, 0.0, 10.0, 0.0),
            (max + 1.0, 0.0, 10.0, 10.0),
            (0.0, 0.0, max + 1.0, 10.0),
        ] {
            assert_eq!(
                rect(bad, (0, 0)),
                Err("native_terminal_rect_invalid"),
                "{bad:?}"
            );
        }
        assert_eq!(
            rect((0.0, 0.0, 10.0, 10.0), (RECT_MAX + 1, 0)),
            Err("native_terminal_rect_invalid")
        );
    }

    #[test]
    fn device_pixels_scale_the_logical_rect() {
        let logical = Rect {
            x: -5,
            y: 7,
            w: 400,
            h: 240,
        };
        assert_eq!(logical.device(1), (-5, 7, 400, 240));
        assert_eq!(logical.device(2), (-10, 14, 800, 480));
    }

    #[test]
    #[should_panic(expected = "a display scale")]
    fn a_scale_out_of_range_is_a_bug() {
        let one = Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        };
        let _ = one.device(0);
    }

    #[test]
    fn a_child_presents_only_with_a_change_and_no_frame_callback_owed() {
        let mut pacing = Pacing::new();
        assert!(pacing.ready());
        pacing.presenting();
        assert!(!pacing.ready());
        pacing.changed();
        assert!(!pacing.ready());
        pacing.frame_done();
        assert!(pacing.ready());
        pacing.presenting();
        pacing.frame_done();
        assert!(!pacing.ready());
        pacing.changed();
        assert!(pacing.ready());
    }

    #[test]
    fn a_hidden_child_never_presents_and_showing_redraws_it() {
        let mut pacing = Pacing::new();
        pacing.presenting();
        pacing.hide();
        pacing.changed();
        pacing.frame_done();
        assert!(!pacing.ready());
        pacing.show();
        assert!(pacing.ready());
        pacing.presenting();
        pacing.frame_done();
        pacing.hide();
        pacing.show();
        assert!(pacing.ready());
    }

    #[test]
    fn a_present_that_did_not_happen_is_owed_again() {
        let mut pacing = Pacing::new();
        pacing.presenting();
        pacing.not_presented();
        assert!(pacing.ready());
    }

    #[test]
    fn a_child_whose_toplevel_unmapped_never_presents_again() {
        let mut pacing = Pacing::new();
        pacing.gone();
        pacing.show();
        pacing.changed();
        pacing.frame_done();
        assert!(!pacing.ready());
        assert!(pacing.is_gone());
    }

    #[test]
    #[should_panic(expected = "a present only when ready")]
    fn presenting_when_not_ready_is_a_bug() {
        let mut pacing = Pacing::new();
        pacing.hide();
        pacing.presenting();
    }

    fn pump() {
        while gtk::events_pending() {
            gtk::main_iteration_do(false);
        }
    }

    fn pump_for(wait: Duration) {
        let end = Instant::now() + wait;
        while Instant::now() < end {
            pump();
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn ms(since: Instant) -> f64 {
        since.elapsed().as_secs_f64() * 1000.0
    }

    fn stats(samples: &[f64]) -> Value {
        assert!(!samples.is_empty());
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
        json!({"n": sorted.len(), "p50": at(0.5), "p95": at(0.95), "max": at(1.0)})
    }

    fn row_dir() -> Option<PathBuf> {
        std::env::var_os("NATIVE_ROW_DIR").map(PathBuf::from)
    }

    fn wait_ready(child: &mut Child) -> f64 {
        let started = Instant::now();
        loop {
            pump();
            if child.ready() {
                return ms(started);
            }
            assert!(
                started.elapsed() < STEP_WAIT,
                "no frame callback within the step wait"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn checkpoint(step: &str, child: &Child) {
        let Some(dir) = row_dir() else {
            return;
        };
        let (x, y, w, h) = child.device_rect();
        let line = json!({
            "step": step,
            "device": [x, y, w, h],
            "root": child.root_origin().map(|(rx, ry)| [rx, ry]),
            "scale": child.scale(),
        });
        std::fs::write(dir.join(format!("{step}.json")), format!("{line}\n")).expect("step file");
        let go = dir.join(format!("go-{step}"));
        let started = Instant::now();
        while !go.exists() {
            assert!(
                started.elapsed() < SHOT_WAIT,
                "the farm took no {step} shot"
            );
            pump();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn present(
        child: &mut Child,
        shared: &mut Shared,
        surface: &mut Surface,
        frame: &Frame,
    ) -> f64 {
        child.presenting();
        let started = Instant::now();
        shared.draw(surface, frame).expect("a present");
        ms(started)
    }

    fn white_toplevel() -> gtk::Window {
        gtk::init().expect("a display");
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_decorated(false);
        window.set_default_size(640, 480);
        let css = gtk::CssProvider::new();
        css.load_from_data(b"window { background-color: #ffffff; }")
            .expect("page css");
        let screen = WidgetExt::screen(&window).expect("a screen");
        gtk::StyleContext::add_provider_for_screen(
            &screen,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        window.show_all();
        let started = Instant::now();
        while !window.window().is_some_and(|gdk| gdk.is_viewable()) {
            assert!(started.elapsed() < STEP_WAIT, "the toplevel did not map");
            pump_for(Duration::from_millis(10));
        }
        pump_for(Duration::from_millis(500));
        window
    }

    /// Places a child over a white toplevel, paces presents on its frame callbacks, hides
    /// it, sends output while hidden, shows it, then unmaps and destroys the toplevel with
    /// the wgpu surface still alive. Under NATIVE_ROW_DIR the farm takes a screenshot at each
    /// step; the row checks pacing and present times. Run 3 rule: never present while
    /// hidden, and never present before the last present's frame callback.
    #[test]
    #[ignore = "needs a display: the farm runs it under Xvfb, weston and sway"]
    fn a_child_places_hides_shows_and_presents_only_on_its_frame_callback() {
        let window = white_toplevel();
        let placed = Rect {
            x: 100,
            y: 120,
            w: 400,
            h: 240,
        };
        let mut child = Child::new(&window, placed).expect("a child surface");
        let (width, height) = child.size();
        let (family, source) = font_for("JetBrains Mono").expect("built in");
        let style = Style {
            family,
            source,
            font_px: 13.0,
            blink: false,
            theme: Theme {
                background: GREEN,
                ..Theme::default()
            },
        };
        let mut shared = Shared::new(child.scale() as f32, &style).expect("a GPU");
        let (display, handle) = child.handles();
        let mut surface =
            unsafe { shared.surface_raw(display, handle, width, height) }.expect("a wgpu surface");
        let frame = Frame {
            rows: Vec::new(),
            keys: Vec::new(),
            fills: Vec::new(),
            cursor: None,
        };

        let mut presents = Vec::with_capacity(FRAMES);
        let mut waits = Vec::with_capacity(FRAMES);
        while presents.len() < FRAMES {
            waits.push(wait_ready(&mut child));
            presents.push(present(&mut child, &mut shared, &mut surface, &frame));
            child.changed();
        }
        pump_for(Duration::from_millis(200));
        checkpoint("placed", &child);

        child.hide();
        pump_for(Duration::from_millis(100));
        child.changed();
        let hidden = Instant::now();
        while hidden.elapsed() < Duration::from_millis(300) {
            pump();
            assert!(!child.ready(), "a hidden child never presents");
            std::thread::sleep(Duration::from_millis(1));
        }
        checkpoint("hidden", &child);

        child.show();
        let shown_wait = wait_ready(&mut child);
        let shown_present = present(&mut child, &mut shared, &mut surface, &frame);
        pump_for(Duration::from_millis(200));
        checkpoint("shown", &child);

        window.hide();
        pump_for(Duration::from_millis(200));
        child.changed();
        assert!(
            !child.ready(),
            "a child whose toplevel unmapped never presents"
        );
        unsafe { window.destroy() };
        pump_for(Duration::from_millis(200));
        drop(surface);
        drop(child);
        drop(shared);
        pump_for(Duration::from_millis(100));

        let worst = presents.iter().copied().fold(shown_present, f64::max);
        let report = json!({
            "present_ms": stats(&presents),
            "wait_ms": stats(&waits),
            "shown_wait_ms": shown_wait,
            "shown_present_ms": shown_present,
        });
        if let Some(dir) = row_dir() {
            std::fs::write(dir.join("report.json"), format!("{report}\n")).expect("report file");
        }
        assert!(
            worst < PRESENT_MS_MAX,
            "a present took {worst:.2} ms: {report}"
        );
    }
}
