use std::cell::Cell;
use std::ffi::c_void;
use std::io::ErrorKind;
use std::ops::RangeInclusive;
use std::ptr::NonNull;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::gdk::{Display, Window};
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
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

use super::signals::Handlers;

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

/// The X11 Shape extension's bounding shape kind, which XFixes sets to the part of the
/// child inside the webview.
const SHAPE_BOUNDING: i32 = 0;

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

    /// The part of the rectangle inside another, such as the webview's area: a Wayland
    /// subsurface is not clipped to its toplevel, so a child shows only this part. Input:
    /// the other rectangle, in the same pixels. Output: the part, or none when no pixel of
    /// the rectangle is inside.
    pub fn visible(self, bounds: Rect) -> Option<Rect> {
        let end = |start: i32, size: i32| i64::from(start) + i64::from(size);
        let x = self.x.max(bounds.x);
        let y = self.y.max(bounds.y);
        let right = end(self.x, self.w).min(end(bounds.x, bounds.w));
        let bottom = end(self.y, self.h).min(end(bounds.y, bounds.h));
        if right <= i64::from(x) || bottom <= i64::from(y) {
            return None;
        }
        let part = Rect {
            x,
            y,
            w: i32::try_from(right - i64::from(x)).expect("no wider than the rectangle"),
            h: i32::try_from(bottom - i64::from(y)).expect("no taller than the rectangle"),
        };
        assert!(part.x >= self.x && part.y >= self.y && part.w <= self.w && part.h <= self.h);
        assert!(part.w >= 1 && part.h >= 1);
        Some(part)
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

/// When a child may present: only while shown with a part inside the webview, only with a
/// change to draw, never before the last present's frame callback, and never after its
/// toplevel unmapped. A FIFO present
/// waits for the last present's callback, which a hidden or unmapped surface may never get
/// (spike run 3: a present after a null-buffer hide blocked the main thread 40 s).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pacing {
    shown: bool,
    clipped: bool,
    waiting: bool,
    dirty: bool,
    gone: bool,
}

impl Pacing {
    /// A new child's pacing: shown, with its first frame owed. Output: it.
    const fn new() -> Pacing {
        Pacing {
            shown: true,
            clipped: false,
            waiting: false,
            dirty: true,
            gone: false,
        }
    }

    /// Whether a present may happen now. Output: the answer.
    fn ready(&self) -> bool {
        self.on_screen() && self.dirty && !self.waiting
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

    /// Notes the child's rectangle having no part inside the webview.
    fn clip(&mut self) {
        self.clipped = true;
    }

    /// Notes a part of the rectangle inside the webview; one coming back is redrawn, since
    /// an X11 window off screen lost its pixels.
    fn unclip(&mut self) {
        if self.clipped {
            self.clipped = false;
            self.dirty = true;
        }
    }

    /// Whether the child is on screen: shown, with a part inside the webview, and its
    /// toplevel mapped. Output: the answer.
    fn on_screen(&self) -> bool {
        self.shown && !self.clipped && !self.gone
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
delegate_noop!(Frames: WpViewporter);
delegate_noop!(Frames: WpViewport);

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
    xfixes: x11_dl::xfixes::Xlib,
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

    /// Shows the part of the child on screen: shapes the window to it and maps it, or
    /// unmaps the window when no part is on screen. Inputs: the rectangle and scale, and
    /// the part on screen before and now. Output: none.
    fn show(&self, rect: Rect, scale: i32, was: Option<Rect>, part: Option<Rect>) {
        let Some(part) = part else {
            if was.is_some() {
                self.trapped(|xlib, display, window| unsafe {
                    (xlib.XUnmapWindow)(display, window);
                });
            }
            return;
        };
        assert!(part.x >= rect.x && part.y >= rect.y);
        let offset = |logical: i32| {
            let device = logical.saturating_mul(scale).clamp(0, i32::from(i16::MAX));
            i16::try_from(device).expect("clamped to a short")
        };
        let size = |logical: i32| {
            let device = logical.saturating_mul(scale).clamp(1, i32::from(u16::MAX));
            u16::try_from(device).expect("clamped to an unsigned short")
        };
        let mut shape = x11_dl::xlib::XRectangle {
            x: offset(part.x - rect.x),
            y: offset(part.y - rect.y),
            width: size(part.w),
            height: size(part.h),
        };
        self.trapped(|xlib, display, window| unsafe {
            let region = (self.xfixes.XFixesCreateRegion)(display, &mut shape, 1);
            (self.xfixes.XFixesSetWindowShapeRegion)(display, window, SHAPE_BOUNDING, 0, 0, region);
            (self.xfixes.XFixesDestroyRegion)(display, region);
            if was.is_none() {
                (xlib.XMapRaised)(display, window);
            }
        });
    }
}

/// The Wayland objects that place a child, on GTK's own connection: its surface, a desynced
/// subsurface of the toplevel's surface, the surface's viewport, which crops it to the part
/// inside the webview, and the toplevel's surface.
struct Surfaces {
    conn: Connection,
    surface: WlSurface,
    subsurface: WlSubsurface,
    viewport: WpViewport,
    parent: WlSurface,
}

impl Surfaces {
    /// Crops the surface to the part on screen inside the buffer its next commit carries and
    /// places the subsurface there, all pending until the next commits. With no such part
    /// the surface is one pixel under the toplevel's surface at the page's top left, where
    /// the opaque page covers it. The crop never reaches past that buffer, since
    /// wp_viewport's out_of_buffer error would end GTK's connection, and before the first
    /// buffer only the place is sent. Inputs: the placement, that buffer, none before the
    /// first present, and the page's area. Output: none.
    fn crop(&self, placement: &Placement, buffer: Option<Buffer>, bounds: Option<Rect>) {
        let rect = placement.rect;
        let shown = placement.part.and_then(|part| match buffer {
            Some(buffer) => part.visible(buffer.at(rect.x, rect.y)),
            None => Some(part),
        });
        if let Some(buffer) = buffer {
            let (source, size) = shown.map_or(((0, 0), (1, 1)), |shown| {
                assert!(shown.x >= rect.x && shown.y >= rect.y);
                ((shown.x - rect.x, shown.y - rect.y), (shown.w, shown.h))
            });
            self.surface.set_buffer_scale(buffer.scale);
            self.viewport.set_source(
                f64::from(source.0),
                f64::from(source.1),
                f64::from(size.0),
                f64::from(size.1),
            );
            self.viewport.set_destination(size.0, size.1);
        }
        match shown {
            Some(shown) => {
                self.subsurface.set_position(shown.x, shown.y);
                self.subsurface.place_above(&self.parent);
            }
            None => {
                let (x, y) = bounds.map_or((0, 0), |bounds| (bounds.x, bounds.y));
                self.subsurface.set_position(x, y);
                self.subsurface.place_below(&self.parent);
            }
        }
    }
}

/// A desynced Wayland subsurface's event queue for the frame callbacks of its presents, on
/// GTK's own connection, its surfaces, and whether its first present has been followed by a
/// commit of the toplevel's surface.
struct Wayland {
    _gdk: Display,
    display: NonNull<c_void>,
    queue: EventQueue<Frames>,
    frames: Frames,
    surfaces: Rc<Surfaces>,
    mapped: bool,
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

/// The window a child's crop shapes: the X11 child window, or the Wayland surfaces.
enum Target {
    X11(Rc<X11>),
    Wayland(Rc<Surfaces>),
}

/// A buffer of a Wayland child's surface: its size in device pixels and the scale it was
/// drawn at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Buffer {
    width: u32,
    height: u32,
    scale: i32,
}

impl Buffer {
    /// The buffer's area in the toplevel's logical pixels with its top left at a point.
    /// Inputs: the point. Output: the area, at least one pixel.
    fn at(self, x: i32, y: i32) -> Rect {
        assert!((1..=SCALE_MAX).contains(&self.scale));
        let logical = |device: u32| i32::try_from(device).expect("a bounded buffer") / self.scale;
        let area = Rect {
            x,
            y,
            w: logical(self.width),
            h: logical(self.height),
        };
        assert!(
            area.w >= 1 && area.h >= 1,
            "a buffer of at least one logical pixel"
        );
        area
    }
}

/// Where a child is and at what scale, the buffer its surface holds and the one the host is
/// presenting, and the part of its rectangle on screen, none while off screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Placement {
    rect: Rect,
    scale: i32,
    attached: Option<Buffer>,
    drawing: Option<Buffer>,
    part: Option<Rect>,
}

/// What a child shows of its rectangle: only the part inside the webview's area, since a
/// Wayland subsurface is not clipped to its toplevel. The page's size-allocate handler
/// shares it and re-clips with no host call, as a window shrink sends no new page rect.
struct Crop {
    page: gtk::Widget,
    toplevel: gtk::Window,
    target: Target,
    pacing: Rc<Cell<Pacing>>,
    placement: Cell<Placement>,
}

impl Crop {
    /// The webview's area in the toplevel's logical pixels, inside the toplevel. Output: it,
    /// or none while the page is not realized or has no area.
    fn bounds(&self) -> Option<Rect> {
        let (x, y) = self.page.translate_coordinates(&self.toplevel, 0, 0)?;
        let page = Rect {
            x,
            y,
            w: self.page.allocated_width(),
            h: self.page.allocated_height(),
        };
        let toplevel = Rect {
            x: 0,
            y: 0,
            w: self.toplevel.allocated_width(),
            h: self.toplevel.allocated_height(),
        };
        page.visible(toplevel)
    }

    /// Applies what the child shows now: its part inside the webview while on screen, else
    /// nothing. X11 shapes the window to the part and maps it, or unmaps it. Wayland crops
    /// the surface's buffer through its viewport and commits the surface; the subsurface's
    /// place takes effect with the toplevel's next commit, made here, or, from the page's
    /// size-allocate, GTK's commit of the frame it is laying out, since a commit inside
    /// GTK's layout could carry GTK's acked size with its old buffer. Input: whether to
    /// commit the toplevel's surface. Output: none.
    fn apply(&self, commit_toplevel: bool) {
        let mut pacing = self.pacing.get();
        if pacing.is_gone() {
            return;
        }
        let mut placement = self.placement.get();
        let bounds = self.bounds();
        let inside = bounds.and_then(|bounds| placement.rect.visible(bounds));
        if inside.is_some() {
            pacing.unclip();
        } else {
            pacing.clip();
        }
        self.pacing.set(pacing);
        let was = placement.part;
        placement.part = inside.filter(|_| pacing.on_screen());
        self.placement.set(placement);
        match &self.target {
            Target::X11(x11) => x11.show(placement.rect, placement.scale, was, placement.part),
            Target::Wayland(surfaces) => {
                surfaces.crop(&placement, placement.attached, bounds);
                surfaces.surface.commit();
                if commit_toplevel {
                    surfaces.parent.commit();
                }
                if !commit_toplevel || (was.is_some() && placement.part.is_none()) {
                    self.toplevel.queue_draw();
                }
                flush(&surfaces.conn, &self.pacing);
            }
        }
    }

    /// Crops a Wayland surface for the buffer the host is about to present, so the
    /// present's commit carries both. Input: the buffer's size in device pixels, at least
    /// one logical pixel each way. Output: none.
    fn drawing(&self, size: (u32, u32)) {
        let mut placement = self.placement.get();
        let buffer = Buffer {
            width: size.0,
            height: size.1,
            scale: placement.scale,
        };
        placement.drawing = Some(buffer);
        self.placement.set(placement);
        if let Target::Wayland(surfaces) = &self.target {
            surfaces.crop(&placement, Some(buffer), self.bounds());
        }
    }

    /// Notes the host's present gone out or failed. Input: whether it went out. Output:
    /// whether the surface now holds a buffer of a new size or scale, whose crop may have
    /// moved the subsurface.
    fn presented(&self, out: bool) -> bool {
        let mut placement = self.placement.get();
        let drawn = placement.drawing.take();
        let resized = out && drawn.is_some() && drawn != placement.attached;
        if out && drawn.is_some() {
            placement.attached = drawn;
        }
        self.placement.set(placement);
        resized
    }
}

/// A child surface of a GTK toplevel that wgpu draws a terminal into: an X11 child window
/// with an empty input shape, or a desynced Wayland subsurface with an empty input region,
/// on GTK's own display connection so it stacks with the toplevel. Clicks pass through to
/// the toplevel. Only the part of its rectangle inside the webview shows. Once the toplevel
/// unmaps the child never presents again; the host makes a new one. The host drops the wgpu
/// surface before the child.
pub struct Child {
    native: Native,
    crop: Rc<Crop>,
    pacing: Rc<Cell<Pacing>>,
    handlers: Handlers,
    page_handlers: Handlers,
}

impl Child {
    /// Makes a child over a host rectangle of a mapped toplevel, shown and owing its first
    /// frame when a part of the rectangle is inside the page widget, the webview, which must
    /// be in that toplevel. Inputs: the toplevel, the page widget and the rectangle. Output:
    /// the child, or `native_terminal_surface_unavailable`, also when the connection broke.
    pub fn new(
        toplevel: &gtk::Window,
        page: &gtk::Widget,
        rect: Rect,
    ) -> Result<Child, &'static str> {
        if page.toplevel().as_ref() != Some(toplevel.upcast_ref::<gtk::Widget>()) {
            return Err(UNAVAILABLE);
        }
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
        let target = match &native {
            Native::X11 { x11, .. } => Target::X11(Rc::clone(x11)),
            Native::Wayland(wayland) => Target::Wayland(Rc::clone(&wayland.surfaces)),
        };
        let pacing = Rc::new(Cell::new(Pacing::new()));
        let crop = Rc::new(Crop {
            page: page.clone(),
            toplevel: toplevel.clone(),
            target,
            pacing: Rc::clone(&pacing),
            placement: Cell::new(Placement {
                rect,
                scale,
                attached: None,
                drawing: None,
                part: None,
            }),
        });
        let mut handlers = Handlers::new(toplevel);
        let unmap = {
            let pacing = Rc::clone(&pacing);
            toplevel.connect_unmap(move |_| mark_gone(&pacing))
        };
        handlers.push(unmap);
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
        handlers.push(unrealize);
        let mut page_handlers = Handlers::new(page);
        let allocated = {
            let crop = Rc::clone(&crop);
            page.connect_size_allocate(move |_, _| crop.apply(false))
        };
        page_handlers.push(allocated);
        crop.apply(true);
        let child = Child {
            native,
            crop,
            pacing,
            handlers,
            page_handlers,
        };
        if child.pacing.get().is_gone() {
            return Err(UNAVAILABLE);
        }
        assert_eq!(
            child.pacing.get().ready(),
            child.crop.placement.get().part.is_some()
        );
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
                let surface = NonNull::new(wayland.surfaces.surface.id().as_ptr().cast::<c_void>())
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
        self.crop.placement.get().scale
    }

    /// The child's place in the toplevel, in device pixels. Output: x, y, width, height.
    pub fn device_rect(&self) -> (i32, i32, u32, u32) {
        let placement = self.crop.placement.get();
        placement.rect.device(placement.scale)
    }

    /// The size a wgpu surface on the child takes: the whole rectangle, however little of
    /// it is on screen. Output: width and height in device pixels.
    pub fn size(&self) -> (u32, u32) {
        let (_, _, width, height) = self.device_rect();
        (width, height)
    }

    /// Moves the child to a new host rectangle at the toplevel's current scale and owes a
    /// frame; the host resizes the wgpu surface when `size` changed. Only the part inside
    /// the webview shows. Input: the rectangle. Output: none.
    pub fn place(&mut self, rect: Rect) {
        let scale = self.crop.toplevel.scale_factor();
        if self.pacing.get().is_gone() || !(1..=SCALE_MAX).contains(&scale) {
            return;
        }
        let mut placement = self.crop.placement.get();
        placement.rect = rect;
        placement.scale = scale;
        self.crop.placement.set(placement);
        if let Native::X11 { x11, .. } = &self.native {
            let (x, y, width, height) = rect.device(scale);
            x11.trapped(|xlib, display, window| unsafe {
                (xlib.XMoveResizeWindow)(display, window, x, y, width, height);
            });
        }
        self.crop.apply(true);
        self.changed();
    }

    /// Takes the child off screen: X11 unmaps it; Wayland crops it to one pixel under the
    /// toplevel's surface, whose opaque page covers it, and has GTK redraw the page: weston
    /// 13 applies damage only with a new buffer and does not repaint the area a restack
    /// below an opaque parent uncovers. Output: none.
    pub fn hide(&mut self) {
        let mut pacing = self.pacing.get();
        if pacing.is_gone() {
            return;
        }
        pacing.hide();
        self.pacing.set(pacing);
        self.crop.apply(true);
    }

    /// Puts the part of the child inside the webview back on screen and owes a frame.
    /// Output: none.
    pub fn show(&mut self) {
        let mut pacing = self.pacing.get();
        if pacing.is_gone() {
            return;
        }
        pacing.show();
        self.pacing.set(pacing);
        self.crop.apply(true);
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

    /// Notes a present about to happen, right before the host draws into its wgpu surface.
    /// On Wayland it asks for the frame callback that the present's commit carries, and
    /// crops the surface for that buffer in the same commit, so the crop holds even before
    /// the host resizes to `size`. Input: the wgpu surface's configured size in device
    /// pixels. Output: none.
    pub fn presenting(&mut self, size: (u32, u32)) {
        let mut pacing = self.pacing.get();
        pacing.presenting();
        self.pacing.set(pacing);
        match &mut self.native {
            Native::X11 { presented, .. } => *presented = Some(Instant::now()),
            Native::Wayland(wayland) => {
                wayland.frames.done = false;
                let _callback = wayland.surfaces.surface.frame(&wayland.queue.handle(), ());
            }
        }
        self.crop.drawing(size);
    }

    /// Notes that the host's present went out. On Wayland it then commits the toplevel's
    /// surface after the first present, since a compositor such as weston gives a desynced
    /// subsurface no place on screen, and so no frame callback, until its parent commits
    /// after the subsurface's first buffer, and after a present at a new size or scale,
    /// whose crop may have moved the subsurface. Output: none.
    pub fn presented(&mut self) {
        let resized = self.crop.presented(true);
        let Native::Wayland(wayland) = &mut self.native else {
            return;
        };
        if (wayland.mapped && !resized) || self.pacing.get().is_gone() {
            return;
        }
        wayland.surfaces.parent.commit();
        flush(&wayland.surfaces.conn, &self.pacing);
        wayland.mapped = true;
        assert!(self.pacing.get().waiting || self.pacing.get().is_gone());
    }

    /// Notes that the host's draw failed before presenting, so the frame is owed again and
    /// the surface keeps its buffer. Output: none.
    pub fn not_presented(&mut self) {
        let mut pacing = self.pacing.get();
        pacing.not_presented();
        self.pacing.set(pacing);
        self.crop.presented(false);
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

    /// The part of the child's rectangle on screen, in the toplevel's logical pixels.
    /// Output: the part, or none while the child is off screen.
    fn part(&self) -> Option<Rect> {
        self.crop.placement.get().part
    }
}

impl Drop for Child {
    /// Disconnects the toplevel's and the page's handlers and destroys the window, or the
    /// viewport, subsurface and surface; a gone Wayland toplevel's surface is not committed,
    /// since GDK destroyed it. Output: none.
    fn drop(&mut self) {
        self.handlers.disconnect();
        self.page_handlers.disconnect();
        let gone = self.pacing.get().is_gone();
        match &self.native {
            Native::X11 { x11, .. } => x11.trapped(|xlib, display, window| unsafe {
                (xlib.XDestroyWindow)(display, window);
            }),
            Native::Wayland(wayland) => {
                let surfaces = &wayland.surfaces;
                surfaces.viewport.destroy();
                surfaces.subsurface.destroy();
                surfaces.surface.destroy();
                if !gone {
                    surfaces.parent.commit();
                }
                flush(&surfaces.conn, &self.pacing);
            }
        }
    }
}

/// Makes an X11 child window of the toplevel with an empty input shape, unmapped until its
/// crop shows a part of it, and paced to the toplevel's monitor refresh. Inputs: GDK's X11 display, the toplevel's GDK
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
        }
        (screen, window)
    };
    let failed = gdk.error_trap_pop() != 0;
    let x11 = Rc::new(X11 {
        xlib,
        xfixes,
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
/// with its own event queue, an empty input region, the toplevel's buffer scale and a
/// viewport; a compositor without wp_viewporter gets the plain code. The
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
    let viewporter: WpViewporter = globals.bind(&handle, 1..=1, ()).map_err(|_| UNAVAILABLE)?;
    let parent = unsafe { ObjectId::from_ptr(WlSurface::interface(), parent.cast()) }
        .map_err(|_| UNAVAILABLE)?;
    let parent = WlSurface::from_id(&conn, parent).map_err(|_| UNAVAILABLE)?;
    let surface = compositor.create_surface(&handle, ());
    let subsurface = subcompositor.get_subsurface(&surface, &parent, &handle, ());
    subcompositor.destroy();
    let viewport = viewporter.get_viewport(&surface, &handle, ());
    viewporter.destroy();
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
        queue,
        frames,
        surfaces: Rc::new(Surfaces {
            conn,
            surface,
            subsurface,
            viewport,
            parent,
        }),
        mapped: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_term::linux::rows::{STEP_WAIT, ms, pump, pump_for, pump_until, row_dir};
    use crate::native_term::render::{Shared, Surface};
    use crate::native_term::screen::Frame;
    use crate::native_term::style::{Style, Theme, font_for};
    use alacritty_terminal::vte::ansi::Rgb;
    use serde_json::{Value, json};

    /// The 95th percentile one present may take: under the 16.7 ms of a 60 Hz output, so
    /// presents that wait on a frame callback fail the row.
    const PRESENT_MS_MAX: f64 = 12.0;

    /// The longest any one present may take, the loaded cells' CPU preemption included: a
    /// present blocked on a frame callback that never comes (run 3: 40 s) fails the row.
    const PRESENT_BLOCK_MS_MAX: f64 = 50.0;

    /// The GObject criticals the display row saw, such as a handler disconnected after its
    /// toplevel's dispose had already removed it.
    static GOBJECT_CRITICALS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

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

    /// A Wayland subsurface is not clipped to its toplevel, so a child shows only the part
    /// of its rectangle inside the webview: none past an edge, none outside it.
    #[test]
    fn the_visible_part_is_the_rect_inside_the_webview() {
        let page = Rect {
            x: 0,
            y: 40,
            w: 800,
            h: 560,
        };
        let at = |x, y, w, h| Rect { x, y, w, h };
        let inside = at(100, 120, 400, 240);
        assert_eq!(inside.visible(page), Some(inside));
        assert_eq!(
            at(600, 500, 400, 240).visible(page),
            Some(at(600, 500, 200, 100))
        );
        assert_eq!(
            at(-50, 0, 400, 240).visible(page),
            Some(at(0, 40, 350, 200))
        );
        assert_eq!(at(-100, 0, 1000, 700).visible(page), Some(page));
        assert_eq!(at(800, 120, 400, 240).visible(page), None);
        assert_eq!(at(100, -200, 400, 240).visible(page), None);
        assert_eq!(at(-16_384, -16_384, 16_384, 16_384).visible(page), None);
        assert_eq!(inside.visible(at(0, 40, 0, 0)), None);
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

    /// A child whose rectangle has no part inside the webview stays off screen and never
    /// presents, and owes a frame once a part comes back; a hidden child stays off screen.
    #[test]
    fn a_child_with_no_visible_part_never_presents_and_owes_a_frame_when_one_comes_back() {
        let mut pacing = Pacing::new();
        pacing.presenting();
        pacing.frame_done();
        pacing.clip();
        pacing.changed();
        assert!(!pacing.ready());
        assert!(!pacing.on_screen());
        pacing.unclip();
        assert!(pacing.on_screen());
        assert!(pacing.ready());
        pacing.presenting();
        pacing.frame_done();
        pacing.unclip();
        assert!(!pacing.ready(), "a part that stayed owes no frame");
        pacing.clip();
        pacing.hide();
        pacing.unclip();
        assert!(
            !pacing.on_screen(),
            "a hidden child stays off screen with a part"
        );
        pacing.show();
        assert!(pacing.on_screen() && pacing.ready());
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

    fn stats(samples: &[f64]) -> Value {
        assert!(!samples.is_empty());
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
        json!({"n": sorted.len(), "p50": at(0.5), "p95": at(0.95), "max": at(1.0)})
    }

    fn wait_ready(child: &mut Child) -> f64 {
        let started = Instant::now();
        pump_until("frame callback", || child.ready());
        ms(started)
    }

    fn checkpoint(step: &str, child: &Child) {
        let (x, y, w, h) = child.device_rect();
        let line = json!({
            "step": step,
            "device": [x, y, w, h],
            "root": child.root_origin().map(|(rx, ry)| [rx, ry]),
            "scale": child.scale(),
        });
        crate::native_term::linux::rows::checkpoint(step, &line);
    }

    fn present(
        child: &mut Child,
        shared: &mut Shared,
        surface: &mut Surface,
        frame: &Frame,
        size: (u32, u32),
    ) -> f64 {
        child.presenting(size);
        let started = Instant::now();
        shared.draw(surface, frame).expect("a present");
        let took = ms(started);
        child.presented();
        took
    }

    /// A white toplevel holding a page widget that fills it, mapped. Output: both.
    fn white_toplevel() -> (gtk::Window, gtk::DrawingArea) {
        gtk::init().expect("a display");
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_decorated(false);
        window.set_default_size(640, 480);
        add_css(&window, b"window { background-color: #ffffff; }");
        let page = gtk::DrawingArea::new();
        window.add(&page);
        window.show_all();
        wait_mapped(&window);
        pump_for(Duration::from_millis(500));
        (window, page)
    }

    /// Adds a stylesheet for the toplevel's screen. Inputs: the toplevel and the CSS.
    fn add_css(window: &gtk::Window, css: &[u8]) {
        let provider = gtk::CssProvider::new();
        provider.load_from_data(css).expect("page css");
        let screen = WidgetExt::screen(window).expect("a screen");
        gtk::StyleContext::add_provider_for_screen(
            &screen,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    /// Runs GTK's events until the toplevel is viewable. Input: the toplevel.
    fn wait_mapped(window: &gtk::Window) {
        let started = Instant::now();
        while !window.window().is_some_and(|gdk| gdk.is_viewable()) {
            assert!(started.elapsed() < STEP_WAIT, "the toplevel did not map");
            pump_for(Duration::from_millis(10));
        }
    }

    /// A GPU, a wgpu surface on the child at its size and an empty frame, drawn in a green
    /// background the farm's screenshots look for. Input: the child. Output: them.
    fn green_terminal(child: &Child) -> (Shared, Surface, Frame) {
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
        let surface =
            unsafe { shared.surface_raw(display, handle, width, height) }.expect("a wgpu surface");
        let frame = Frame {
            rows: Vec::new(),
            keys: Vec::new(),
            fills: Vec::new(),
            cursor: None,
        };
        (shared, surface, frame)
    }

    /// A blue toplevel the size of the farm's output, and a white 640x480 one transient for it,
    /// so weston's kiosk shell centres it and sway floats it; X11 runs no window manager, so
    /// it is moved. The white one holds a page widget at its top left. Input: the page size.
    /// Output: the blue toplevel, the white one and the page.
    fn toplevel_over_back(size: (i32, i32)) -> (gtk::Window, gtk::Window, gtk::DrawingArea) {
        gtk::init().expect("a display");
        let back = gtk::Window::new(gtk::WindowType::Toplevel);
        back.set_widget_name("native-back");
        back.set_decorated(false);
        back.set_default_size(1280, 800);
        add_css(
            &back,
            b"#native-back { background-color: #0000ff; } #native-row { background-color: #ffffff; }",
        );
        back.show_all();
        wait_mapped(&back);
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_widget_name("native-row");
        window.set_decorated(false);
        window.set_default_size(640, 480);
        window.set_transient_for(Some(&back));
        window.move_(320, 160);
        let fixed = gtk::Fixed::new();
        let page = gtk::DrawingArea::new();
        page.set_size_request(size.0, size.1);
        fixed.put(&page, 0, 0);
        window.add(&fixed);
        window.show_all();
        wait_mapped(&window);
        pump_for(Duration::from_millis(500));
        (back, window, page)
    }

    /// Resizes the page widget and runs GTK's layout, so its size-allocate has run. Inputs:
    /// the page and the new size.
    fn resize_page(page: &gtk::DrawingArea, size: (i32, i32)) {
        page.set_size_request(size.0, size.1);
        pump_until("the page's new allocation", || {
            (page.allocated_width(), page.allocated_height()) == size
        });
        pump_for(Duration::from_millis(200));
    }

    /// Presents until the child has shown a frame, then lets the compositor show it.
    /// Inputs: the child, its terminal and the wgpu surface's size.
    fn show_frame(
        child: &mut Child,
        shared: &mut Shared,
        surface: &mut Surface,
        frame: &Frame,
        size: (u32, u32),
    ) {
        for _ in 0..3 {
            wait_ready(child);
            present(child, shared, surface, frame, size);
            child.changed();
        }
        pump_for(Duration::from_millis(200));
    }

    /// Presents once at the wgpu surface's old size after a place, as a host that has not
    /// resized yet does. X11 may refuse the present; on Wayland the crop stays inside the
    /// old buffer, so GTK's connection and the child live on. Inputs: the child, its
    /// terminal and the old size.
    fn present_unresized(
        child: &mut Child,
        shared: &mut Shared,
        surface: &mut Surface,
        frame: &Frame,
        size: (u32, u32),
    ) {
        wait_ready(child);
        child.presenting(size);
        match shared.draw(surface, frame) {
            Ok(()) => child.presented(),
            Err(_) => child.not_presented(),
        }
        pump_for(Duration::from_millis(200));
        assert!(
            !child.pacing.get().is_gone(),
            "a present before the host resizes leaves the child live"
        );
    }

    /// Writes a clip step for the farm's screenshot: the step, the child's rectangle and
    /// the part on screen. Inputs: the step and the child.
    fn clip_checkpoint(step: &str, child: &Child) {
        let (x, y, w, h) = child.device_rect();
        let line = json!({
            "step": step,
            "device": [x, y, w, h],
            "part": child.part().map(|part| [part.x, part.y, part.w, part.h]),
            "scale": child.scale(),
        });
        crate::native_term::linux::rows::checkpoint(step, &line);
    }

    /// A child past the page's and the toplevel's right and bottom edges, over a blue toplevel:
    /// only its part inside the page shows. The page shrinks, shrinks out of the rect and grows
    /// back through size-allocate alone, the child never presenting with no part; it then moves
    /// to a larger and a smaller rect, each shown first with its old buffer. Under
    /// NATIVE_ROW_DIR the farm checks pixels in, beside and outside the part at each step.
    #[test]
    #[ignore = "needs a display: the farm runs it under Xvfb, weston and sway"]
    fn a_child_shows_only_its_part_inside_the_webview_and_reclips_on_size_allocate() {
        let (back, window, page) = toplevel_over_back((400, 300));
        let rect = Rect {
            x: 300,
            y: 200,
            w: 600,
            h: 400,
        };
        let part = |w, h| {
            Some(Rect {
                x: 300,
                y: 200,
                w,
                h,
            })
        };
        let mut child = Child::new(&window, page.upcast_ref(), rect).expect("a child surface");
        let (mut shared, mut surface, frame) = green_terminal(&child);
        let size = child.size();
        show_frame(&mut child, &mut shared, &mut surface, &frame, size);
        clip_checkpoint("cropped", &child);
        assert_eq!(child.part(), part(100, 100));

        resize_page(&page, (360, 260));
        clip_checkpoint("reclipped", &child);
        assert_eq!(child.part(), part(60, 60));

        resize_page(&page, (200, 150));
        assert_eq!(child.part(), None);
        child.changed();
        let none = Instant::now();
        while none.elapsed() < Duration::from_millis(300) {
            pump();
            assert!(
                !child.ready(),
                "a child with no part on screen never presents"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        clip_checkpoint("none", &child);

        resize_page(&page, (400, 300));
        show_frame(&mut child, &mut shared, &mut surface, &frame, size);
        clip_checkpoint("back", &child);
        assert_eq!(child.part(), part(100, 100));

        let larger = Rect {
            x: 250,
            y: 150,
            w: 700,
            h: 500,
        };
        let larger_part = Some(Rect {
            x: 250,
            y: 150,
            w: 150,
            h: 150,
        });
        child.place(larger);
        pump_for(Duration::from_millis(200));
        clip_checkpoint("larger_old", &child);
        assert_eq!(child.part(), larger_part);
        present_unresized(&mut child, &mut shared, &mut surface, &frame, size);
        let larger_size = child.size();
        shared.resize(&mut surface, larger_size.0, larger_size.1);
        show_frame(&mut child, &mut shared, &mut surface, &frame, larger_size);
        clip_checkpoint("larger", &child);
        assert_eq!(child.part(), larger_part);

        let smaller = Rect {
            x: 320,
            y: 220,
            w: 40,
            h: 30,
        };
        child.place(smaller);
        pump_for(Duration::from_millis(200));
        clip_checkpoint("smaller_old", &child);
        assert_eq!(child.part(), Some(smaller));
        let smaller_size = child.size();
        shared.resize(&mut surface, smaller_size.0, smaller_size.1);
        show_frame(&mut child, &mut shared, &mut surface, &frame, smaller_size);
        clip_checkpoint("smaller", &child);
        assert_eq!(child.part(), Some(smaller));

        drop(surface);
        drop(child);
        drop(shared);
        unsafe {
            window.destroy();
            back.destroy();
        }
        pump_for(Duration::from_millis(100));
    }

    /// Places a child over a white toplevel, paces presents on its frame callbacks, hides
    /// it, sends output while hidden, shows it, then unmaps and destroys the toplevel with
    /// the wgpu surface still alive. Under NATIVE_ROW_DIR the farm takes a screenshot at each
    /// step; the row checks pacing and present times. Run 3 rule: never present while
    /// hidden, and never present before the last present's frame callback.
    #[test]
    #[ignore = "needs a display: the farm runs it under Xvfb, weston and sway"]
    fn a_child_places_hides_shows_and_presents_only_on_its_frame_callback() {
        let criticals = gtk::glib::log_set_handler(
            Some("GLib-GObject"),
            gtk::glib::LogLevels::LEVEL_CRITICAL,
            false,
            false,
            |_, _, message| {
                eprintln!("GLib-GObject critical: {message}");
                GOBJECT_CRITICALS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
        );
        let (window, page) = white_toplevel();
        let placed = Rect {
            x: 100,
            y: 120,
            w: 400,
            h: 240,
        };
        let mut child = Child::new(&window, page.upcast_ref(), placed).expect("a child surface");
        let (mut shared, mut surface, frame) = green_terminal(&child);
        let size = child.size();

        let mut presents = Vec::with_capacity(FRAMES);
        let mut waits = Vec::with_capacity(FRAMES);
        while presents.len() < FRAMES {
            waits.push(wait_ready(&mut child));
            presents.push(present(&mut child, &mut shared, &mut surface, &frame, size));
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
        let shown_present = present(&mut child, &mut shared, &mut surface, &frame, size);
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

        gtk::glib::log_remove_handler(Some("GLib-GObject"), criticals);
        let worst = presents.iter().copied().fold(shown_present, f64::max);
        let mut sorted = presents.clone();
        sorted.sort_by(f64::total_cmp);
        let p95 = sorted[((sorted.len() - 1) as f64 * 0.95).round() as usize];
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
            p95 < PRESENT_MS_MAX,
            "presents took {p95:.2} ms at p95: {report}"
        );
        assert!(
            worst < PRESENT_BLOCK_MS_MAX,
            "a present blocked {worst:.2} ms: {report}"
        );
        assert_eq!(
            GOBJECT_CRITICALS.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the child left no GObject critical behind"
        );
    }
}
