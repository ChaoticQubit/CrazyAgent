use std::cell::RefCell;
use std::ffi::c_void;
use std::io::Write as _;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use gtk::glib::translate::ToGlibPtr;
use gtk::prelude::*;
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
    XlibDisplayHandle, XlibWindowHandle,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_subcompositor::WlSubcompositor;
use wayland_client::protocol::wl_subsurface::WlSubsurface;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{delegate_noop, Connection, Dispatch, EventQueue, Proxy, QueueHandle};

const SHAPE_INPUT: i32 = 2;
const FRAMES: usize = 60;
const CHECKPOINT_WAIT: Duration = Duration::from_secs(180);
const RECT_MAX: f64 = 16_384.0;
const KEY_TEXT_MAX: usize = 16;
const CLIP_TEXT: &str = "crazyagent-clipboard-row";
const PRIMARY_TEXT: &str = "crazyagent-primary-row";
const GREEN: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 1.0,
    b: 0.0,
    a: 1.0,
};

/// A host rectangle in the page's CSS pixels, which are GTK logical pixels at zoom 1.
#[derive(Clone, Copy, Debug, Deserialize)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// The probe's Wayland queue state: every object it binds sends no event it reads.
struct WaylandState;

delegate_noop!(WaylandState: WlCompositor);
delegate_noop!(WaylandState: WlSubcompositor);
delegate_noop!(WaylandState: WlSubsurface);
delegate_noop!(WaylandState: WlRegion);
delegate_noop!(WaylandState: ignore WlSurface);

impl Dispatch<WlRegistry, GlobalListContents> for WaylandState {
    /// Ignores registry changes after the first roundtrip. Inputs: the event. Output: none.
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

/// The platform window a surface draws into: an X11 child window of the toplevel, or a
/// Wayland subsurface of the toplevel's surface made on GTK's own display connection.
enum Native {
    X11 {
        xlib: x11_dl::xlib::Xlib,
        display: *mut x11_dl::xlib::Display,
        screen: i32,
        window: u64,
    },
    Wayland {
        display: *mut c_void,
        conn: Connection,
        _queue: EventQueue<WaylandState>,
        surface: WlSurface,
        subsurface: WlSubsurface,
        parent: WlSurface,
    },
}

impl Native {
    /// The raw handles wgpu draws through. Output: the display and window handles.
    fn handles(&self) -> Result<(RawDisplayHandle, RawWindowHandle), String> {
        match self {
            Native::X11 {
                display,
                screen,
                window,
                ..
            } => Ok((
                RawDisplayHandle::Xlib(XlibDisplayHandle::new(
                    NonNull::new(display.cast::<c_void>()),
                    *screen,
                )),
                RawWindowHandle::Xlib(XlibWindowHandle::new(*window)),
            )),
            Native::Wayland {
                display, surface, ..
            } => {
                let display = NonNull::new(*display).ok_or("no Wayland display")?;
                let surface = NonNull::new(surface.id().as_ptr().cast::<c_void>())
                    .ok_or("no Wayland surface")?;
                Ok((
                    RawDisplayHandle::Wayland(WaylandDisplayHandle::new(display)),
                    RawWindowHandle::Wayland(WaylandWindowHandle::new(surface)),
                ))
            }
        }
    }

    /// Moves and sizes the window over the host. Inputs: the device rectangle and the
    /// logical origin. Output: none.
    fn place(&self, device: (i32, i32, u32, u32), logical: (i32, i32)) {
        match self {
            Native::X11 {
                xlib,
                display,
                window,
                ..
            } => unsafe {
                (xlib.XMoveResizeWindow)(*display, *window, device.0, device.1, device.2, device.3);
                (xlib.XFlush)(*display);
            },
            Native::Wayland {
                conn,
                subsurface,
                parent,
                ..
            } => {
                subsurface.set_position(logical.0, logical.1);
                parent.commit();
                let _ = conn.flush();
            }
        }
    }

    /// Takes the window off screen until the next frame or show. Output: none.
    fn hide(&self) {
        match self {
            Native::X11 {
                xlib,
                display,
                window,
                ..
            } => unsafe {
                (xlib.XUnmapWindow)(*display, *window);
                (xlib.XFlush)(*display);
            },
            Native::Wayland {
                conn,
                surface,
                parent,
                ..
            } => {
                surface.attach(None, 0, 0);
                surface.commit();
                parent.commit();
                let _ = conn.flush();
            }
        }
    }

    /// Puts an X11 window back on screen; a Wayland surface comes back with its next frame.
    /// Output: none.
    fn show(&self) {
        if let Native::X11 {
            xlib,
            display,
            window,
            ..
        } = self
        {
            unsafe {
                (xlib.XMapRaised)(*display, *window);
                (xlib.XFlush)(*display);
            }
        }
    }

    /// Where the window sits on the X11 root, for pixel checks. Output: the root origin, or
    /// none on Wayland, where the kiosk shell puts the toplevel at the output's origin.
    fn root_origin(&self) -> Option<(i32, i32)> {
        match self {
            Native::X11 {
                xlib,
                display,
                window,
                ..
            } => unsafe {
                let root = (xlib.XDefaultRootWindow)(*display);
                let (mut x, mut y, mut child) = (0, 0, 0);
                let found = (xlib.XTranslateCoordinates)(
                    *display, *window, root, 0, 0, &mut x, &mut y, &mut child,
                );
                (found != 0).then_some((x, y))
            },
            Native::Wayland { .. } => None,
        }
    }
}

impl Drop for Native {
    /// Destroys the window after the GPU surface that drew into it. Output: none.
    fn drop(&mut self) {
        match self {
            Native::X11 {
                xlib,
                display,
                window,
                ..
            } => unsafe {
                (xlib.XDestroyWindow)(*display, *window);
                (xlib.XFlush)(*display);
            },
            Native::Wayland {
                conn,
                surface,
                subsurface,
                parent,
                ..
            } => {
                subsurface.destroy();
                surface.destroy();
                parent.commit();
                let _ = conn.flush();
            }
        }
    }
}

/// The wgpu objects of one surface. The surface drops before the device and instance.
struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    adapter: String,
    _instance: wgpu::Instance,
}

impl Gpu {
    /// Configures the surface to a new device size. Inputs: the width and height. Output: none.
    fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
    }
}

/// One placed surface: its GPU objects, then its window, dropped in that order.
struct Drawn {
    gpu: Gpu,
    native: Native,
    keys: u32,
}

/// What the native input path saw: whether it owns the keys, its IM context, the time of
/// the key in progress and its timings, commits and preedit strings.
#[derive(Default)]
struct NativeInput {
    on: bool,
    im: Option<gtk::IMMulticontext>,
    key_started: Option<Instant>,
    handled_ms: Vec<f64>,
    commit_ms: Vec<f64>,
    commits: Vec<String>,
    preedits: Vec<String>,
}

/// The toplevel the webview sits in: its GTK and GDK windows, the webview's offset in it,
/// its scale and whether GDK runs on X11.
struct Host {
    toplevel: gtk::ApplicationWindow,
    gdk: gtk::gdk::Window,
    offset: (i32, i32),
    scale: i32,
    x11: bool,
}

thread_local! {
    static DRAWN: RefCell<Option<Drawn>> = const { RefCell::new(None) };
    static NATIVE: RefCell<NativeInput> = RefCell::new(NativeInput::default());
}

/// Milliseconds since a start. Input: the start. Output: the milliseconds.
fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1e3
}

/// The p50, p95 and max of a timing row. Input: the samples. Output: the summary.
fn stats(samples: &mut [f64]) -> Value {
    if samples.is_empty() {
        return json!({ "n": 0 });
    }
    samples.sort_by(|left, right| left.total_cmp(right));
    let count = samples.len();
    let at = |share: f64| samples[((share * count as f64).ceil() as usize).clamp(1, count) - 1];
    json!({ "n": count, "p50": at(0.5), "p95": at(0.95), "max": samples[count - 1] })
}

/// The cell this run measures, from SPIKE_CELL. Output: the name.
fn cell() -> String {
    std::env::var("SPIKE_CELL").unwrap_or_else(|_| "unnamed".into())
}

/// Prints one result row for the farm script. Inputs: the row name and its value. Output: none.
fn report(row: &str, value: Value) {
    println!(
        "SPIKE {}",
        json!({ "row": row, "cell": cell(), "value": value })
    );
    let _ = std::io::stdout().flush();
}

/// A row or checkpoint name: 1 to 32 of a-z, 0-9 and _. Input: the name. Output: it, or why not.
fn checked_name(name: &str) -> Result<&str, String> {
    let fits = !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    fits.then_some(name)
        .ok_or_else(|| format!("a name is 1 to 32 of a-z, 0-9 and _: {name:?}"))
}

/// The wgpu backends SPIKE_GPU picks. Output: Vulkan or GL, or why not.
fn backends() -> Result<wgpu::Backends, String> {
    match std::env::var("SPIKE_GPU").as_deref() {
        Ok("vulkan") => Ok(wgpu::Backends::VULKAN),
        Ok("gl") => Ok(wgpu::Backends::GL),
        other => Err(format!("SPIKE_GPU is vulkan or gl: {other:?}")),
    }
}

/// The toplevel around the webview. Input: the webview window. Output: the host, or why not.
fn host(window: &WebviewWindow) -> Result<Host, String> {
    let toplevel = window.gtk_window().map_err(|error| error.to_string())?;
    let vbox = window.default_vbox().map_err(|error| error.to_string())?;
    let gdk = toplevel
        .window()
        .ok_or("the toplevel has no GdkWindow yet")?;
    let offset = vbox
        .translate_coordinates(&toplevel, 0, 0)
        .ok_or("the webview box is not inside its toplevel")?;
    let scale = toplevel.scale_factor();
    let x11 = gdk.display().downcast_ref::<gdkx11::X11Display>().is_some();
    Ok(Host {
        toplevel,
        gdk,
        offset,
        scale,
        x11,
    })
}

/// The host rectangle in toplevel logical pixels. Inputs: the host and the page rectangle.
/// Output: x, y, width and height, or why the rectangle is refused.
fn logical(host: &Host, rect: Rect) -> Result<(i32, i32, i32, i32), String> {
    let finite = [rect.x, rect.y, rect.w, rect.h]
        .iter()
        .all(|value| value.is_finite() && value.abs() <= RECT_MAX);
    if !finite || rect.w < 1.0 || rect.h < 1.0 {
        return Err(format!(
            "a host rectangle is finite, bounded and at least 1 px: {rect:?}"
        ));
    }
    Ok((
        rect.x.round() as i32 + host.offset.0,
        rect.y.round() as i32 + host.offset.1,
        rect.w.round() as i32,
        rect.h.round() as i32,
    ))
}

/// The logical rectangle in device pixels. Inputs: the rectangle and the scale. Output: it.
fn device(logical: (i32, i32, i32, i32), scale: i32) -> (i32, i32, u32, u32) {
    (
        logical.0 * scale,
        logical.1 * scale,
        (logical.2 * scale).max(1) as u32,
        (logical.3 * scale).max(1) as u32,
    )
}

/// Makes an X11 child window of the toplevel above the webview, with an empty input shape
/// so the pointer reaches the page. Inputs: the toplevel's GDK window and the device
/// rectangle. Output: the window, or why not.
fn x11_child(gdk: &gtk::gdk::Window, device: (i32, i32, u32, u32)) -> Result<Native, String> {
    let parent = gdk
        .downcast_ref::<gdkx11::X11Window>()
        .ok_or("the toplevel is not an X11 window")?
        .xid();
    let gdk_display = gdk.display();
    let raw: *mut gdkx11::ffi::GdkX11Display = gdk_display.to_glib_none().0 as *mut _;
    let display =
        unsafe { gdkx11::ffi::gdk_x11_display_get_xdisplay(raw) }.cast::<x11_dl::xlib::Display>();
    if display.is_null() {
        return Err("GDK gave no X display".into());
    }
    let xlib = x11_dl::xlib::Xlib::open().map_err(|error| error.to_string())?;
    let xfixes = x11_dl::xfixes::XFixes::open().map_err(|error| error.to_string())?;
    unsafe {
        let screen = (xlib.XDefaultScreen)(display);
        let window = (xlib.XCreateSimpleWindow)(
            display, parent, device.0, device.1, device.2, device.3, 0, 0, 0,
        );
        if window == 0 {
            return Err("XCreateSimpleWindow made no window".into());
        }
        let region = (xfixes.XFixesCreateRegion)(display, std::ptr::null_mut(), 0);
        (xfixes.XFixesSetWindowShapeRegion)(display, window, SHAPE_INPUT, 0, 0, region);
        (xfixes.XFixesDestroyRegion)(display, region);
        (xlib.XMapRaised)(display, window);
        (xlib.XFlush)(display);
        Ok(Native::X11 {
            xlib,
            display,
            screen,
            window,
        })
    }
}

/// Makes a Wayland subsurface of the toplevel's surface on GTK's own display connection,
/// synced or desynced by SPIKE_SYNC, with an empty input region so the pointer reaches
/// the page. Inputs: the toplevel's GDK window, the logical origin and the scale. Output:
/// the subsurface, or why not.
fn wayland_child(gdk: &gtk::gdk::Window, origin: (i32, i32), scale: i32) -> Result<Native, String> {
    let gdk_display = gdk.display();
    let raw_display: *mut gdkwayland_sys::GdkWaylandDisplay =
        gdk_display.to_glib_none().0 as *mut _;
    let raw_window: *mut gdkwayland_sys::GdkWaylandWindow = gdk.to_glib_none().0 as *mut _;
    let display = unsafe { gdkwayland_sys::gdk_wayland_display_get_wl_display(raw_display) };
    let parent_ptr = unsafe { gdkwayland_sys::gdk_wayland_window_get_wl_surface(raw_window) };
    if display.is_null() || parent_ptr.is_null() {
        return Err("GDK gave no Wayland display or surface".into());
    }
    let backend = unsafe { wayland_backend::client::Backend::from_foreign_display(display.cast()) };
    let conn = Connection::from_backend(backend);
    let (globals, mut queue) =
        registry_queue_init::<WaylandState>(&conn).map_err(|error| error.to_string())?;
    let handle = queue.handle();
    let compositor: WlCompositor = globals
        .bind(&handle, 3..=4, ())
        .map_err(|error| error.to_string())?;
    let subcompositor: WlSubcompositor = globals
        .bind(&handle, 1..=1, ())
        .map_err(|error| error.to_string())?;
    let parent_id = unsafe {
        wayland_backend::client::ObjectId::from_ptr(WlSurface::interface(), parent_ptr.cast())
    }
    .map_err(|error| error.to_string())?;
    let parent = WlSurface::from_id(&conn, parent_id).map_err(|error| error.to_string())?;
    let surface = compositor.create_surface(&handle, ());
    let subsurface = subcompositor.get_subsurface(&surface, &parent, &handle, ());
    subsurface.set_position(origin.0, origin.1);
    if std::env::var("SPIKE_SYNC").as_deref() == Ok("desync") {
        subsurface.set_desync();
    }
    let region = compositor.create_region(&handle, ());
    surface.set_input_region(Some(&region));
    region.destroy();
    surface.set_buffer_scale(scale);
    surface.commit();
    parent.commit();
    queue
        .roundtrip(&mut WaylandState)
        .map_err(|error| error.to_string())?;
    Ok(Native::Wayland {
        display,
        conn,
        _queue: queue,
        surface,
        subsurface,
        parent,
    })
}

/// Opens wgpu on the window with the backend SPIKE_GPU picks and the present mode
/// SPIKE_PRESENT picks. Inputs: the window and its device size. Output: the GPU objects.
fn gpu(native: &Native, size: (u32, u32)) -> Result<Gpu, String> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: backends()?,
        ..Default::default()
    });
    let (display, window) = native.handles()?;
    let target = wgpu::SurfaceTargetUnsafe::RawHandle {
        raw_display_handle: display,
        raw_window_handle: window,
    };
    let surface =
        unsafe { instance.create_surface_unsafe(target) }.map_err(|error| error.to_string())?;
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: Some(&surface),
        ..Default::default()
    }))
    .map_err(|error| error.to_string())?;
    let info = adapter.get_info();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits:
            wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .map_err(|error| error.to_string())?;
    let mut config = surface
        .get_default_config(&adapter, size.0.max(1), size.1.max(1))
        .ok_or("the adapter cannot present to this surface")?;
    config.present_mode = match std::env::var("SPIKE_PRESENT").as_deref() {
        Ok("immediate") => wgpu::PresentMode::AutoNoVsync,
        _ => wgpu::PresentMode::AutoVsync,
    };
    surface.configure(&device, &config);
    let adapter = format!(
        "{:?} {:?} {} {}",
        info.backend, info.device_type, info.name, info.driver_info
    );
    Ok(Gpu {
        surface,
        device,
        queue,
        config,
        adapter,
        _instance: instance,
    })
}

/// Clears the surface to one color and presents it. Inputs: the GPU objects and the color.
/// Output: milliseconds to present and to the GPU finishing.
fn frame(gpu: &Gpu, color: wgpu::Color) -> Result<(f64, f64), String> {
    let started = Instant::now();
    let texture = gpu
        .surface
        .get_current_texture()
        .map_err(|error| error.to_string())?;
    let view = texture
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(color),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
    }));
    gpu.queue.submit([encoder.finish()]);
    texture.present();
    let presented = ms(started);
    gpu.device
        .poll(wgpu::PollType::Wait)
        .map_err(|error| error.to_string())?;
    Ok((presented, ms(started)))
}

/// The color of the n-th key frame, so each key changes the surface. Input: the count.
/// Output: the color.
fn key_color(keys: u32) -> wgpu::Color {
    wgpu::Color {
        r: 0.0,
        g: 1.0 - 0.2 * f64::from(keys % 2),
        b: 0.0,
        a: 1.0,
    }
}

/// Draws one key frame on the placed surface. Output: the milliseconds, or why not.
fn draw_key() -> Result<f64, String> {
    let started = Instant::now();
    DRAWN.with(|cell| {
        let mut drawn = cell.borrow_mut();
        let placed = drawn.as_mut().ok_or("no surface is placed")?;
        placed.keys += 1;
        frame(&placed.gpu, key_color(placed.keys)).map(|_| ())
    })?;
    Ok(ms(started))
}

/// The steps and cell the page runs, from SPIKE_STEPS and SPIKE_CELL. Output: both.
#[tauri::command]
fn config() -> Value {
    let steps = std::env::var("SPIKE_STEPS").unwrap_or_default();
    json!({ "cell": cell(), "steps": steps.split(',').filter(|step| !step.is_empty()).collect::<Vec<_>>() })
}

/// Row 0: a surface on a bare GTK window with no webview presents 60 frames. Output: the
/// adapter and frame timings, or why it cannot present.
#[tauri::command]
fn plain() -> Result<Value, String> {
    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_decorated(false);
    window.set_default_size(640, 400);
    window.show_all();
    for _ in 0..200 {
        if !gtk::events_pending() {
            break;
        }
        gtk::main_iteration_do(false);
    }
    let gdk = window.window().ok_or("the bare window has no GdkWindow")?;
    let scale = window.scale_factor();
    let logical = (40, 40, 320, 200);
    let size = device(logical, scale);
    let native = if gdk.display().downcast_ref::<gdkx11::X11Display>().is_some() {
        x11_child(&gdk, size)?
    } else {
        wayland_child(&gdk, (logical.0, logical.1), scale)?
    };
    let result = gpu(&native, (size.2, size.3)).and_then(|gpu| {
        let mut presented = Vec::with_capacity(FRAMES);
        let mut done = Vec::with_capacity(FRAMES);
        for index in 0..FRAMES {
            let (shown, finished) = frame(&gpu, key_color(index as u32))?;
            presented.push(shown);
            done.push(finished);
        }
        Ok(json!({ "adapter": gpu.adapter, "presented_ms": stats(&mut presented), "done_ms": stats(&mut done) }))
    });
    drop(native);
    window.close();
    result
}

/// Puts the surface over the host rectangle, making it on the first call, and draws green.
/// Inputs: the webview window and the page rectangle. Output: the adapter, the rectangles
/// and the time it took.
#[tauri::command]
fn place(window: WebviewWindow, rect: Rect) -> Result<Value, String> {
    let host = host(&window)?;
    let logical = logical(&host, rect)?;
    let size = device(logical, host.scale);
    let started = Instant::now();
    DRAWN.with(|cell| {
        let mut drawn = cell.borrow_mut();
        let placed = match drawn.take() {
            Some(mut placed) => {
                placed.native.place(size, (logical.0, logical.1));
                placed.gpu.resize(size.2, size.3);
                placed
            }
            None => {
                let native = if host.x11 {
                    x11_child(&host.gdk, size)?
                } else {
                    wayland_child(&host.gdk, (logical.0, logical.1), host.scale)?
                };
                let gpu = gpu(&native, (size.2, size.3))?;
                Drawn {
                    gpu,
                    native,
                    keys: 0,
                }
            }
        };
        let (_, done) = frame(&placed.gpu, GREEN)?;
        let reply = json!({
            "adapter": placed.gpu.adapter,
            "x11": host.x11,
            "scale": host.scale,
            "logical": [logical.0, logical.1, logical.2, logical.3],
            "device": [size.0, size.1, size.2, size.3],
            "root": placed.native.root_origin().map(|(x, y)| [x, y]),
            "place_ms": ms(started),
            "frame_ms": done,
        });
        *drawn = Some(placed);
        Ok(reply)
    })
}

/// Times 60 frames on the placed surface, then draws green again. Output: the timings.
#[tauri::command]
fn frame_times() -> Result<Value, String> {
    DRAWN.with(|cell| {
        let drawn = cell.borrow();
        let placed = drawn.as_ref().ok_or("no surface is placed")?;
        let mut presented = Vec::with_capacity(FRAMES);
        let mut done = Vec::with_capacity(FRAMES);
        for index in 0..FRAMES {
            let (shown, finished) = frame(&placed.gpu, key_color(index as u32))?;
            presented.push(shown);
            done.push(finished);
        }
        frame(&placed.gpu, GREEN)?;
        Ok(json!({ "presented_ms": stats(&mut presented), "done_ms": stats(&mut done) }))
    })
}

/// Hides the surface while the page shows something over the host. Output: the milliseconds.
#[tauri::command]
fn hide() -> Result<f64, String> {
    let started = Instant::now();
    DRAWN.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|placed| placed.native.hide())
            .ok_or("no surface is placed")
    })?;
    Ok(ms(started))
}

/// Shows the surface again and draws green. Output: the milliseconds.
#[tauri::command]
fn show() -> Result<f64, String> {
    let started = Instant::now();
    DRAWN.with(|cell| {
        let drawn = cell.borrow();
        let placed = drawn.as_ref().ok_or("no surface is placed")?;
        placed.native.show();
        frame(&placed.gpu, GREEN).map(|_| ())
    })?;
    Ok(ms(started))
}

/// Input A: the page saw a key and sends it here; one frame is drawn for it. Input: the
/// key's text, at most 16 characters. Output: the milliseconds to draw it.
#[tauri::command]
fn key(text: String) -> Result<f64, String> {
    if text.chars().count() > KEY_TEXT_MAX {
        return Err(format!("a key carries at most {KEY_TEXT_MAX} characters"));
    }
    draw_key()
}

/// Wires input B once: the toplevel's key-press-event, ahead of the webview, feeds an IM
/// multicontext (IBus when GTK_IM_MODULE says so) and draws a frame per key or commit.
/// Inputs: the toplevel and its GDK window. Output: the IM context.
fn wire(toplevel: &gtk::ApplicationWindow, gdk: &gtk::gdk::Window) -> gtk::IMMulticontext {
    let im = gtk::IMMulticontext::new();
    im.set_client_window(Some(gdk));
    im.connect_commit(|_, text| {
        let drew = draw_key();
        NATIVE.with(|cell| {
            let mut input = cell.borrow_mut();
            input.commits.push(text.to_owned());
            if let (Ok(_), Some(started)) = (drew, input.key_started) {
                input.commit_ms.push(ms(started));
            }
        });
    });
    im.connect_preedit_changed(|im| {
        let (text, _, _) = im.preedit_string();
        NATIVE.with(|cell| cell.borrow_mut().preedits.push(text.to_string()));
    });
    toplevel.connect_key_press_event(|_, event| {
        let started = Instant::now();
        let (on, im) = NATIVE.with(|cell| {
            let mut input = cell.borrow_mut();
            if input.on {
                input.key_started = Some(started);
            }
            (input.on, input.im.clone())
        });
        if !on {
            return gtk::glib::Propagation::Proceed;
        }
        let eaten = im.as_ref().is_some_and(|im| im.filter_keypress(event));
        if !eaten && draw_key().is_ok() {
            NATIVE.with(|cell| cell.borrow_mut().handled_ms.push(ms(started)));
        }
        gtk::glib::Propagation::Stop
    });
    im
}

/// Turns input B on or off: on, the toplevel takes the keys before the webview and the IM
/// context gets focus at the host's cursor cell. Inputs: the webview window, on or off and
/// the host rectangle. Output: none, or why not.
#[tauri::command]
fn native_focus(window: WebviewWindow, on: bool, rect: Rect) -> Result<(), String> {
    let host = host(&window)?;
    let logical = logical(&host, rect)?;
    let wired = NATIVE.with(|cell| cell.borrow().im.clone());
    let im = match wired {
        Some(im) => im,
        None => {
            let im = wire(&host.toplevel, &host.gdk);
            NATIVE.with(|cell| cell.borrow_mut().im = Some(im.clone()));
            im
        }
    };
    NATIVE.with(|cell| cell.borrow_mut().on = on);
    if on {
        im.set_cursor_location(&gtk::gdk::Rectangle::new(logical.0, logical.1, 1, 16));
        im.focus_in();
    } else {
        im.focus_out();
    }
    Ok(())
}

/// What input B saw since the last report, then clears it. Output: timings, commits and
/// preedit strings.
#[tauri::command]
fn native_report() -> Value {
    NATIVE.with(|cell| {
        let mut input = cell.borrow_mut();
        let reply = json!({
            "handled_ms": stats(&mut input.handled_ms),
            "commit_ms": stats(&mut input.commit_ms),
            "commits": input.commits.iter().take(64).collect::<Vec<_>>(),
            "preedits": input.preedits.iter().take(64).collect::<Vec<_>>(),
        });
        input.handled_ms.clear();
        input.commit_ms.clear();
        input.commits.clear();
        input.preedits.clear();
        input.key_started = None;
        reply
    })
}

/// The owner's copy: sets CLIPBOARD and PRIMARY from the app. Output: none.
#[tauri::command]
fn clip_set() {
    gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).set_text(CLIP_TEXT);
    gtk::Clipboard::get(&gtk::gdk::SELECTION_PRIMARY).set_text(PRIMARY_TEXT);
}

/// The owner's paste: reads CLIPBOARD and PRIMARY into the app. Output: both texts and
/// the time each read took.
#[tauri::command]
fn clip_read() -> Value {
    let started = Instant::now();
    let clipboard = gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD)
        .wait_for_text()
        .map(|text| text.to_string());
    let clipboard_ms = ms(started);
    let started = Instant::now();
    let primary = gtk::Clipboard::get(&gtk::gdk::SELECTION_PRIMARY)
        .wait_for_text()
        .map(|text| text.to_string());
    json!({ "clipboard": clipboard, "clipboard_ms": clipboard_ms, "primary": primary, "primary_ms": ms(started) })
}

/// Hands the screen to the farm script: writes ready-<name> in SPIKE_OUT and waits up to
/// three minutes for go-<name>. Input: the checkpoint name. Output: none, or why not.
#[tauri::command(async)]
fn checkpoint(name: String) -> Result<(), String> {
    let name = checked_name(&name)?;
    let out = std::env::var_os("SPIKE_OUT")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or("SPIKE_OUT names an absolute folder")?;
    std::fs::write(out.join(format!("ready-{name}")), b"").map_err(|error| error.to_string())?;
    let go = out.join(format!("go-{name}"));
    let started = Instant::now();
    while !go.exists() {
        if started.elapsed() > CHECKPOINT_WAIT {
            return Err(format!("no go for {name} in {CHECKPOINT_WAIT:?}"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// A result the page measured. Inputs: the row name and its value. Output: none, or why not.
#[tauri::command]
fn page_report(row: String, value: Value) -> Result<(), String> {
    report(checked_name(&row)?, value);
    Ok(())
}

/// Ends the run once the page has walked its steps. Input: the app. Output: none.
#[tauri::command]
fn done(app: tauri::AppHandle) {
    app.exit(0);
}

/// Opens the spike window, undecorated at the origin so page pixels map to screen pixels,
/// and serves the page's commands. Output: none.
fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            config,
            plain,
            place,
            frame_times,
            hide,
            show,
            key,
            native_focus,
            native_report,
            clip_set,
            clip_read,
            checkpoint,
            page_report,
            done
        ])
        .setup(|app| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("linux-native-spike")
                .inner_size(960.0, 640.0)
                .position(0.0, 0.0)
                .decorations(false)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the spike app runs");
}
