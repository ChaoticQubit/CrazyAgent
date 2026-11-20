use std::path::PathBuf;
use std::time::{Duration, Instant};

use alacritty_terminal::vte::ansi::Rgb;
use gtk::prelude::*;
use serde_json::Value;

use super::surface::Child;
use crate::native_term::render::{Shared, Surface};
use crate::native_term::screen::Frame;
use crate::native_term::style::{Style, Theme, font_for};

/// How long a display row waits for one step: a window to map, a page to load, a frame
/// callback or an event.
pub const STEP_WAIT: Duration = Duration::from_secs(10);

/// How long a display row waits for the farm at a checkpoint: a screenshot or typed keys.
pub const FARM_WAIT: Duration = Duration::from_secs(60);

/// Runs GTK's pending events without blocking. Output: none.
pub fn pump() {
    while gtk::events_pending() {
        gtk::main_iteration_do(false);
    }
}

/// Runs GTK's events for a while. Input: how long. Output: none.
pub fn pump_for(wait: Duration) {
    let end = Instant::now() + wait;
    while Instant::now() < end {
        pump();
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Runs GTK's events until a condition holds, failing the row after the step wait.
/// Inputs: what is awaited, for the failure, and the condition. Output: none.
pub fn pump_until(what: &str, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(
            started.elapsed() < STEP_WAIT,
            "no {what} within the step wait"
        );
        pump();
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Milliseconds since a moment. Input: the moment. Output: the milliseconds.
pub fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

/// The folder the farm shares with a display row, from NATIVE_ROW_DIR. Output: it, or
/// none outside the farm.
pub fn row_dir() -> Option<PathBuf> {
    std::env::var_os("NATIVE_ROW_DIR").map(PathBuf::from)
}

/// Writes a step file for the farm and runs GTK's events until the farm's go file comes;
/// outside the farm it does nothing. Inputs: the step name and its JSON line. Output: none.
pub fn checkpoint(step: &str, line: &Value) {
    let Some(dir) = row_dir() else {
        return;
    };
    std::fs::write(dir.join(format!("{step}.json")), format!("{line}\n")).expect("step file");
    let go = dir.join(format!("go-{step}"));
    let started = Instant::now();
    while !go.exists() {
        assert!(
            started.elapsed() < FARM_WAIT,
            "the farm did not finish step {step}"
        );
        pump();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The child's colour, which the farm's pixel checks look for.
pub const GREEN: Rgb = Rgb { r: 0, g: 255, b: 0 };

/// A white toplevel holding a page widget that fills it, mapped. Output: both.
pub fn white_toplevel() -> (gtk::Window, gtk::DrawingArea) {
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
pub fn add_css(window: &gtk::Window, css: &[u8]) {
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
pub fn wait_mapped(window: &gtk::Window) {
    let started = Instant::now();
    while !window.window().is_some_and(|gdk| gdk.is_viewable()) {
        assert!(started.elapsed() < STEP_WAIT, "the toplevel did not map");
        pump_for(Duration::from_millis(10));
    }
}

/// A GPU, a wgpu surface on the child at its size and an empty frame, drawn in a green
/// background the farm's screenshots look for. Input: the child. Output: them.
pub fn green_terminal(child: &Child) -> (Shared, Surface, Frame) {
    let (width, height) = child.size();
    let (mut shared, frame) = green_shared(child.scale());
    let (display, handle) = child.handles();
    let surface =
        unsafe { shared.surface_raw(display, handle, width, height) }.expect("a wgpu surface");
    (shared, surface, frame)
}

/// A GPU renderer at a scale and an empty frame, drawn in the farm's green, for a host row
/// that makes its own surface. Input: the scale, 1 to 8. Output: them.
pub fn green_shared(scale: i32) -> (Shared, Frame) {
    assert!((1..=8).contains(&scale));
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
    let shared = Shared::new(scale as f32, &style).expect("a GPU");
    let frame = Frame {
        rows: Vec::new(),
        keys: Vec::new(),
        fills: Vec::new(),
        cursor: None,
        preedit: None,
    };
    (shared, frame)
}
