use std::num::NonZeroIsize;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, Win32WindowHandle, WindowsDisplayHandle,
};

use super::super::render::{Shared, Surface};
use super::super::screen::Frame;
use super::super::style::Style;
use super::board::{Board, Fresh, Geometry, VIEWS_MAX};

/// The hand-off every native view on Windows posts to; the one render thread reads it.
pub static BOARD: Board<Frame> = Board::new();

/// How long removing a view waits for the render thread to drop its surface: longer than
/// a swap-chain wait on a loaded software adapter (frame p95 126 ms in the spike).
pub const DROP_WAIT: Duration = Duration::from_secs(2);

/// The plain code for a render thread that could not start.
const UNAVAILABLE: &str = "native_terminal_render_unavailable";

static STARTED: OnceLock<bool> = OnceLock::new();
static PRESENTS: AtomicU64 = AtomicU64::new(0);
static FAILURES: AtomicU64 = AtomicU64::new(0);

/// Starts the one render thread, once, drawing in the first style it is given. Input: the
/// style. Output: ok, or `native_terminal_render_unavailable`.
pub fn start(style: &Style) -> Result<(), &'static str> {
    let started = STARTED.get_or_init(|| {
        let style = style.clone();
        let spawned = thread::Builder::new()
            .name("native-term-render".into())
            .spawn(move || run(style));
        spawned.is_ok()
    });
    if *started { Ok(()) } else { Err(UNAVAILABLE) }
}

/// Test builds only: how many frames the render thread presented, and how many surface
/// setups, waits or draws failed. Output: the two counts.
#[cfg(test)]
pub fn counts() -> (u64, u64) {
    (
        PRESENTS.load(Ordering::SeqCst),
        FAILURES.load(Ordering::SeqCst),
    )
}

struct Drawn {
    id: u64,
    handle: isize,
    geometry: Geometry,
    surface: Option<Surface>,
    last: Option<Frame>,
}

struct Painter {
    style: Style,
    shared: Vec<(u32, Shared)>,
    views: Vec<Drawn>,
}

fn run(style: Style) {
    let mut painter = Painter {
        style,
        shared: Vec::new(),
        views: Vec::new(),
    };
    loop {
        let round = BOARD.wait();
        for id in round.gone {
            painter.views.retain(|view| view.id != id);
            BOARD.dropped(id);
        }
        for fresh in round.fresh {
            painter.make(fresh);
        }
        for id in round.ready {
            painter.draw(id);
        }
        assert!(painter.views.len() <= VIEWS_MAX);
    }
}

fn failed() {
    FAILURES.fetch_add(1, Ordering::SeqCst);
}

impl Painter {
    fn make(&mut self, fresh: Fresh) {
        match self.surface(fresh.handle, fresh.geometry) {
            Ok(surface) => {
                assert!(
                    self.views.iter().all(|view| view.id != fresh.id),
                    "a view made once"
                );
                self.views.push(Drawn {
                    id: fresh.id,
                    handle: fresh.handle,
                    geometry: fresh.geometry,
                    surface: Some(surface),
                    last: None,
                });
            }
            Err(_) => {
                failed();
                BOARD.dropped(fresh.id);
            }
        }
    }

    /// Makes a view's surface on the shared state for its scale, dropping shared state no
    /// view uses first, so there is never more of it than views plus one.
    fn surface(&mut self, handle: isize, geometry: Geometry) -> Result<Surface, String> {
        let views = &self.views;
        self.shared
            .retain(|(key, _)| views.iter().any(|view| view.geometry.scale_key == *key));
        let key = geometry.scale_key;
        let index = match self.shared.iter().position(|(held, _)| *held == key) {
            Some(index) => index,
            None => {
                let made = Shared::new(key as f32 / 100.0, &self.style)?;
                self.shared.push((key, made));
                self.shared.len() - 1
            }
        };
        assert!(self.shared.len() <= self.views.len() + 1);
        let window = Win32WindowHandle::new(NonZeroIsize::new(handle).ok_or("no window")?);
        let display = RawDisplayHandle::Windows(WindowsDisplayHandle::new());
        let (width, height) = geometry.size;
        let shared = &mut self.shared[index].1;
        unsafe { shared.surface_raw(display, RawWindowHandle::Win32(window), width, height) }
    }

    /// Gives a view its new geometry before the swap-chain wait: a resize on the same
    /// scale, or a new surface on the new scale's shared state, the old one dropped first
    /// since a window has one swap chain.
    fn refit(&mut self, index: usize, geometry: Geometry) {
        assert!(
            geometry.size.0 >= 1 && geometry.size.1 >= 1,
            "a placed size"
        );
        let view = &mut self.views[index];
        let same = view.geometry.scale_key == geometry.scale_key;
        view.geometry = geometry;
        let key = geometry.scale_key;
        let held = self
            .shared
            .iter()
            .find(|(shared_key, _)| *shared_key == key);
        if let (true, Some(surface), Some((_, shared))) = (same, view.surface.as_mut(), held) {
            shared.resize(surface, geometry.size.0, geometry.size.1);
            return;
        }
        view.surface = None;
        let handle = view.handle;
        let made = self.surface(handle, geometry);
        match made {
            Ok(surface) => self.views[index].surface = Some(surface),
            Err(_) => failed(),
        }
    }

    /// Draws one ready view, frame-ready first: geometry, then the swap-chain wait, then
    /// the newest frame, so a frame posted during the wait is the one drawn. A view with no
    /// surface or a failed wait keeps its change for the board's bounded retry.
    fn draw(&mut self, id: u64) {
        let index = self.views.iter().position(|view| view.id == id);
        let index = index.expect("a ready view has a drawn entry");
        let geometry = BOARD.geometry(id);
        let missing = self.views[index].surface.is_none();
        if let Some(geometry) = geometry.or(missing.then_some(self.views[index].geometry)) {
            self.refit(index, geometry);
        }
        let view = &mut self.views[index];
        let key = view.geometry.scale_key;
        let held = self
            .shared
            .iter_mut()
            .find(|(shared_key, _)| *shared_key == key);
        let (Some(surface), Some((_, shared))) = (view.surface.as_mut(), held) else {
            BOARD.missed(id);
            return;
        };
        let Ok(output) = shared.acquire(surface) else {
            failed();
            BOARD.missed(id);
            return;
        };
        if let Some(frame) = BOARD.take(id) {
            view.last = Some(frame);
        }
        let Some(frame) = view.last.as_ref() else {
            return;
        };
        match shared.draw_acquired(surface, frame, output) {
            Ok(()) => PRESENTS.fetch_add(1, Ordering::SeqCst),
            Err(_) => FAILURES.fetch_add(1, Ordering::SeqCst),
        };
    }
}
