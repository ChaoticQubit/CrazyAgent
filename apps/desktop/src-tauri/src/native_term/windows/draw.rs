use std::num::NonZeroIsize;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use wgpu::SurfaceTexture;

use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, Win32WindowHandle, WindowsDisplayHandle,
};

use super::super::render::{GPU_FAULT, Shared, Surface};
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
#[cfg(test)]
static REFUSE_RESIZE: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static LOSE_AFTER_WAIT: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static FAIL_WAITS: AtomicU64 = AtomicU64::new(0);

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

/// Test builds only: refuses one view's next resize as wgpu would, with
/// `native_terminal_gpu`. Input: the view id, above 0.
#[cfg(test)]
pub fn trial_refuse_resize(id: u64) {
    assert!(id > 0, "a view id");
    REFUSE_RESIZE.store(id, Ordering::SeqCst);
    assert_eq!(REFUSE_RESIZE.load(Ordering::SeqCst), id);
}

/// Test builds only: loses the device right after one view's next swap-chain wait, as a
/// GPU reset between the wait and the draw would. Input: the view id, above 0.
#[cfg(test)]
pub fn trial_lose_after_wait(id: u64) {
    assert!(id > 0, "a view id");
    LOSE_AFTER_WAIT.store(id, Ordering::SeqCst);
    assert_eq!(LOSE_AFTER_WAIT.load(Ordering::SeqCst), id);
}

/// Test builds only: fails every swap-chain wait of one view, or none, each recording a
/// wgpu error as a refused reconfigure does. Input: the view id, above 0, or none.
#[cfg(test)]
pub fn trial_fail_waits(id: Option<u64>) {
    let id = id.unwrap_or(0);
    FAIL_WAITS.store(id, Ordering::SeqCst);
    assert_eq!(FAIL_WAITS.load(Ordering::SeqCst), id);
}

#[cfg(test)]
fn waited(id: u64, shared: &Shared, surface: &mut Surface) -> Result<SurfaceTexture, String> {
    if id == FAIL_WAITS.load(Ordering::SeqCst) {
        shared.trial_error();
        return Err("a trial wait".to_owned());
    }
    shared.acquire(surface)
}

#[cfg(not(test))]
fn waited(_: u64, shared: &Shared, surface: &mut Surface) -> Result<SurfaceTexture, String> {
    shared.acquire(surface)
}

#[cfg(test)]
fn refused(id: u64) -> bool {
    let asked = REFUSE_RESIZE.compare_exchange(id, 0, Ordering::SeqCst, Ordering::SeqCst);
    asked.is_ok()
}

#[cfg(not(test))]
fn refused(_: u64) -> bool {
    false
}

#[cfg(test)]
fn after_wait(id: u64, shared: &Shared) {
    let asked = LOSE_AFTER_WAIT.compare_exchange(id, 0, Ordering::SeqCst, Ordering::SeqCst);
    if asked.is_ok() {
        shared.trial_lose();
    }
}

#[cfg(not(test))]
fn after_wait(_: u64, _: &Shared) {}

/// Resizes a view's surface; a lost device, or a test row's ask, refuses it. Inputs: the
/// view id, its shared state, its surface and the size in device pixels. Output: ok, or
/// `native_terminal_gpu`.
fn resized(
    id: u64,
    shared: &Shared,
    surface: &mut Surface,
    size: (u32, u32),
) -> Result<(), &'static str> {
    assert!(size.0 >= 1, "a placed width");
    assert!(size.1 >= 1, "a placed height");
    if refused(id) || shared.lost() {
        return Err(GPU_FAULT);
    }
    shared.resize(surface, size.0, size.1)
}

/// The frame a ready view draws: the one posted since the last take, else the last one
/// drawn. A ready view always has one: it is ready only once a frame was posted, and only
/// the draw takes it. Inputs: the last frame and the one taken. Output: the frame.
fn newest<T>(last: &mut Option<T>, taken: Option<T>) -> &T {
    if let Some(frame) = taken {
        *last = Some(frame);
    }
    last.as_ref().expect("a ready view has a frame")
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
    /// Makes a fresh view's surface; one that cannot be made faults the view, as macOS
    /// ends a view with no surface. Input: the view the board handed over.
    fn make(&mut self, fresh: Fresh) {
        assert!(
            self.views.iter().all(|view| view.id != fresh.id),
            "a view made once"
        );
        let made = self.surface(fresh.handle, fresh.geometry);
        let live = made.is_ok();
        self.views.push(Drawn {
            id: fresh.id,
            handle: fresh.handle,
            geometry: fresh.geometry,
            surface: made.ok(),
            last: None,
        });
        assert!(self.views.len() <= VIEWS_MAX);
        if live {
            self.post_cell(fresh.id, fresh.geometry.scale_key);
        } else {
            failed();
            BOARD.fault(fresh.id);
        }
    }

    /// Makes a view's surface on the shared state for its scale, dropping first the shared
    /// state no surface uses, so there is never more of it than views plus one and a lost
    /// device goes once its surfaces have. A lost device makes nothing.
    fn surface(&mut self, handle: isize, geometry: Geometry) -> Result<Surface, String> {
        let views = &self.views;
        self.shared.retain(|(key, _)| {
            views
                .iter()
                .any(|view| view.surface.is_some() && view.geometry.scale_key == *key)
        });
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
        if shared.lost() {
            return Err(GPU_FAULT.to_owned());
        }
        unsafe { shared.surface_raw(display, RawWindowHandle::Win32(window), width, height) }
    }

    /// Gives a view its new geometry before the swap-chain wait: a resize on the same
    /// scale, or a new surface on the new scale's shared state, the old one dropped first
    /// since a window has one swap chain. Inputs: the view's index and geometry. Output: ok,
    /// or `native_terminal_gpu` when the resize or the new surface was refused.
    fn refit(&mut self, index: usize, geometry: Geometry) -> Result<(), &'static str> {
        let view = &mut self.views[index];
        let same = view.geometry.scale_key == geometry.scale_key;
        view.geometry = geometry;
        if same {
            let surface = view.surface.as_mut().expect("a live view has a surface");
            let held = self
                .shared
                .iter()
                .find(|(key, _)| *key == geometry.scale_key);
            let (_, shared) = held.expect("a live view's scale has its shared state");
            return resized(view.id, shared, surface, geometry.size);
        }
        view.surface = None;
        let handle = view.handle;
        let surface = self.surface(handle, geometry).map_err(|_| GPU_FAULT)?;
        self.views[index].surface = Some(surface);
        self.post_cell(self.views[index].id, geometry.scale_key);
        Ok(())
    }

    /// Gives up on a view's GPU work for good: its surface goes and the board never hands
    /// it out as ready again, so its host ends it as macOS does on `native_terminal_gpu`.
    /// Input: the view's index.
    fn fault(&mut self, index: usize) {
        let view = &mut self.views[index];
        view.surface = None;
        BOARD.fault(view.id);
        assert!(BOARD.faulted(view.id), "the board holds the fault");
    }

    /// Tells the board a view's cell size on its scale's shared state, which the view's
    /// thread reads to place the IME. Inputs: the view id and its scale key.
    fn post_cell(&self, id: u64, key: u32) {
        let held = self
            .shared
            .iter()
            .find(|(shared_key, _)| *shared_key == key);
        let (_, shared) = held.expect("a made surface has its shared state");
        let posted = BOARD.post_cell(id, shared.cell_points());
        assert!(
            posted,
            "a drawn view stays on the board until the render side drops it"
        );
    }

    /// Draws one ready view, frame-ready first: geometry, then the swap-chain wait, then
    /// the newest frame, so a frame posted during the wait is the one drawn. A refused
    /// geometry or frame faults the view. Input: the view id.
    fn draw(&mut self, id: u64) {
        let index = self.views.iter().position(|view| view.id == id);
        let index = index.expect("a ready view has a drawn entry");
        assert!(
            self.views[index].surface.is_some(),
            "a ready view has not faulted, so it has a surface"
        );
        let fitted = match BOARD.geometry(id) {
            Some(geometry) => self.refit(index, geometry),
            None => Ok(()),
        };
        if fitted.and_then(|()| self.present(index)).is_err() {
            failed();
            self.fault(index);
        }
    }

    /// Waits for a view's next texture and draws its newest frame into it. Input: the
    /// view's index. Output: ok, with a failed wait on a live device left to the board's
    /// bounded retry, or `native_terminal_gpu`.
    fn present(&mut self, index: usize) -> Result<(), &'static str> {
        let Painter { shared, views, .. } = self;
        let view = &mut views[index];
        let key = view.geometry.scale_key;
        let held = shared.iter_mut().find(|(held, _)| *held == key);
        let (_, shared) = held.expect("a live view's scale has its shared state");
        let surface = view.surface.as_mut().expect("a live view has a surface");
        let Ok(output) = waited(view.id, shared, surface) else {
            if shared.lost() {
                return Err(GPU_FAULT);
            }
            failed();
            BOARD.missed(view.id);
            return Ok(());
        };
        after_wait(view.id, shared);
        let frame = newest(&mut view.last, BOARD.take(view.id));
        match shared.draw_acquired(surface, frame, output) {
            Ok(()) => {
                PRESENTS.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
            Err(code) if code == GPU_FAULT => Err(GPU_FAULT),
            Err(_) => {
                failed();
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_frame_is_drawn_and_kept_for_a_redraw() {
        let mut last = None;
        assert_eq!(*newest(&mut last, Some(1_u8)), 1);
        assert_eq!(
            *newest(&mut last, None),
            1,
            "a geometry alone redraws the last"
        );
        assert_eq!(*newest(&mut last, Some(2)), 2);
        assert_eq!(last, Some(2));
    }

    #[test]
    #[should_panic(expected = "a ready view has a frame")]
    fn a_ready_view_with_no_frame_is_a_bug() {
        let mut last: Option<u8> = None;
        let _ = newest(&mut last, None);
    }
}
