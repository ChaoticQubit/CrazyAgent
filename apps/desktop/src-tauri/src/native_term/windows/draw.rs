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
use super::place::SCALE_KEYS;
use super::view;

/// The hand-off every native view on Windows posts to; the one render thread reads it.
pub static BOARD: Board<Frame> = Board::new();

/// How long removing a view waits for the render thread to drop its surface: longer than
/// a swap-chain wait on a loaded software adapter (frame p95 126 ms in the spike).
pub const DROP_WAIT: Duration = Duration::from_secs(2);

/// The plain code for a render thread that could not start.
const UNAVAILABLE: &str = "native_terminal_render_unavailable";

/// Device requests a scale may make after its first lost device, for the run, as
/// device-replace allows on macOS: one is counted where a surface is made on shared state
/// holding no device, failed ones too. Each lost device (adapter, device, queue, pipeline,
/// glyph cache) is forgotten, never dropped: at most `DEVICE_REPLACE_MAX` + 1 per scale.
const DEVICE_REPLACE_MAX: u32 = 2;

static STARTED: OnceLock<bool> = OnceLock::new();
static PRESENTS: AtomicU64 = AtomicU64::new(0);
static FAILURES: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static REFUSE_RESIZE: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static LOSE_AFTER_WAIT: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static SHARED_MADE: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static REMOVE_AT_PRESENT: AtomicU64 = AtomicU64::new(0);
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

/// Test builds only: removes the D3D12 device right before one view's next present, as a
/// driver reset that `Present` itself finds would. Input: the view id, above 0.
#[cfg(test)]
pub fn trial_remove_at_present(id: u64) {
    assert!(id > 0, "a view id");
    REMOVE_AT_PRESENT.store(id, Ordering::SeqCst);
    assert_eq!(REMOVE_AT_PRESENT.load(Ordering::SeqCst), id);
}

/// Test builds only: how many D3D12 devices the present seam removed. Output: the count.
#[cfg(test)]
pub fn trial_removals() -> u64 {
    super::super::render::trial_removals()
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
    let removal = REMOVE_AT_PRESENT.compare_exchange(id, 0, Ordering::SeqCst, Ordering::SeqCst);
    if removal.is_ok() {
        shared.trial_remove_at_present();
    }
}

#[cfg(not(test))]
fn after_wait(_: u64, _: &Shared) {}

/// Test builds only: how many shared states the render thread has made, so a row can tell a
/// new device in kept shared state from new shared state. Output: the count.
#[cfg(test)]
pub fn trial_shared_made() -> u64 {
    SHARED_MADE.load(Ordering::SeqCst)
}

#[cfg(test)]
fn made_shared() {
    SHARED_MADE.fetch_add(1, Ordering::SeqCst);
}

#[cfg(not(test))]
fn made_shared() {}

/// Resizes a view's surface; a lost device, or a test row's ask, refuses it. Inputs: the
/// view id, its shared state, its surface and the geometry. Output: ok, or
/// `native_terminal_gpu`.
fn resized(
    id: u64,
    shared: &Shared,
    surface: &mut Surface,
    geometry: Geometry,
) -> Result<(), &'static str> {
    geometry.check();
    if refused(id) || shared.lost() {
        return Err(GPU_FAULT);
    }
    shared.resize(surface, geometry.size.0, geometry.size.1)
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

/// Faults one drawn view for good: its surface goes, the board marks it, and its window
/// hears, so the view hides and hands a held focus to the page before the host ends it.
/// Input: the view, which has a window.
fn fault_drawn(drawn: &mut Drawn) {
    assert_ne!(drawn.handle, 0, "a faulted view has a window");
    drawn.surface = None;
    BOARD.fault(drawn.id);
    view::post_faulted(drawn.handle);
}

/// Per scale key that lost a device: the device requests made there since its first loss.
#[derive(Default)]
struct Replaced {
    keys: Vec<(u32, u32)>,
}

impl Replaced {
    /// Counts a lost device on a scale; every device request there is counted from now.
    /// Input: the scale key.
    fn lost(&mut self, key: u32) {
        assert!(SCALE_KEYS.contains(&key), "a scale key");
        if self.keys.iter().all(|(held, _)| *held != key) {
            self.keys.push((key, 0));
        }
    }

    /// Takes one device request on a scale: free before its first loss, else counted and
    /// refused past `DEVICE_REPLACE_MAX`. Input: the scale key. Output: whether a device
    /// may be requested.
    fn attempt(&mut self, key: u32) -> bool {
        let Some((_, made)) = self.keys.iter_mut().find(|(held, _)| *held == key) else {
            return true;
        };
        let allowed = *made < DEVICE_REPLACE_MAX;
        *made += u32::from(allowed);
        allowed
    }
}

struct Painter {
    style: Style,
    shared: Vec<(u32, Shared)>,
    views: Vec<Drawn>,
    replaced: Replaced,
}

fn run(style: Style) {
    let mut painter = Painter {
        style,
        shared: Vec::new(),
        views: Vec::new(),
        replaced: Replaced::default(),
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
        assert!(
            self.views.len() <= VIEWS_MAX,
            "views are bounded by the board's"
        );
        if live {
            self.post_cell(fresh.id, fresh.geometry.scale_key);
        } else {
            failed();
            self.fault(self.views.len() - 1);
        }
    }

    /// Makes a view's surface on the shared state for its scale, after retiring any lost
    /// device. Shared state is kept for the run, one per scale key as macOS keeps it, so
    /// none is ever dropped; a device is requested only where none is held, within the
    /// scale's replacement budget. Inputs: the view's window and geometry.
    fn surface(&mut self, handle: isize, geometry: Geometry) -> Result<Surface, String> {
        self.retire_lost();
        let key = geometry.scale_key;
        let index = match self.shared.iter().position(|(held, _)| *held == key) {
            Some(index) => index,
            None => {
                let made = Shared::new(key as f32 / 100.0, &self.style)?;
                made_shared();
                self.shared.push((key, made));
                self.shared.len() - 1
            }
        };
        let window = Win32WindowHandle::new(NonZeroIsize::new(handle).ok_or("no window")?);
        let shared = &mut self.shared[index].1;
        if !shared.has_device() && !self.replaced.attempt(key) {
            return Err(GPU_FAULT.to_owned());
        }
        let display = RawDisplayHandle::Windows(WindowsDisplayHandle::new());
        let (width, height) = geometry.size;
        unsafe { shared.surface_raw(display, RawWindowHandle::Win32(window), width, height) }
    }

    /// Retires every device wgpu reported lost: each view with a surface on it faults at
    /// once, and its shared state forgets the device and keeps the rest, so the next view
    /// at that scale requests a new one within the scale's budget.
    fn retire_lost(&mut self) {
        for (key, shared) in &mut self.shared {
            if !shared.retire_lost() {
                continue;
            }
            for drawn in &mut self.views {
                if drawn.geometry.scale_key == *key && drawn.surface.is_some() {
                    fault_drawn(drawn);
                }
            }
            self.replaced.lost(*key);
        }
    }

    /// Gives a view its new geometry before the swap-chain wait: a resize on the same
    /// scale, or a new surface on the new scale's shared state, the old one dropped and its
    /// device settled first since a window has one swap chain. Inputs: the view's index and
    /// geometry. Output: ok, or `native_terminal_gpu` when a step was refused.
    fn refit(&mut self, index: usize, geometry: Geometry) -> Result<(), &'static str> {
        let view = &mut self.views[index];
        let old = view.geometry.scale_key;
        let same = old == geometry.scale_key;
        view.geometry = geometry;
        if same {
            let held = self
                .shared
                .iter()
                .find(|(key, _)| *key == geometry.scale_key);
            let (_, shared) = held.expect("a live view's scale has its shared state");
            let surface = view.surface.as_mut().expect("a live view has a surface");
            return resized(view.id, shared, surface, geometry);
        }
        view.surface = None;
        let handle = view.handle;
        let held = self.shared.iter().find(|(key, _)| *key == old);
        let (_, left) = held.expect("a live view's scale has its shared state");
        left.settle()?;
        let surface = self.surface(handle, geometry).map_err(|_| GPU_FAULT)?;
        self.views[index].surface = Some(surface);
        self.post_cell(self.views[index].id, geometry.scale_key);
        Ok(())
    }

    /// Gives up on a view's GPU work for good: its surface goes and the board never hands
    /// it out as ready again, for H5b's host, which ends it with `native_terminal_gpu`; a
    /// lost device then retires every view on it. Input: the view's index.
    fn fault(&mut self, index: usize) {
        fault_drawn(&mut self.views[index]);
        self.retire_lost();
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
    /// geometry or frame faults the view; one a lost device retired earlier in the round is
    /// not drawn. Input: the view id.
    fn draw(&mut self, id: u64) {
        let index = self.views.iter().position(|view| view.id == id);
        let index = index.expect("a ready view has a drawn entry");
        if self.views[index].surface.is_none() {
            assert!(
                BOARD.faulted(id),
                "a ready view with no surface was retired"
            );
            return;
        }
        let fitted = match BOARD.geometry(id) {
            Some(geometry) => self.refit(index, geometry),
            None => Ok(()),
        };
        if fitted.and_then(|()| self.present(index)).is_err() {
            failed();
            self.fault(index);
        }
    }

    /// Waits for a view's next texture and draws its newest frame into it. A failed wait
    /// takes the error its reconfigure recorded, so that error faults this view, never the
    /// next one drawn. Input: the view's index. Output: ok, with a failed wait that recorded
    /// none left to the board's bounded retry, or `native_terminal_gpu`.
    fn present(&mut self, index: usize) -> Result<(), &'static str> {
        let Painter { shared, views, .. } = self;
        let view = &mut views[index];
        let key = view.geometry.scale_key;
        let held = shared.iter_mut().find(|(held, _)| *held == key);
        let (_, shared) = held.expect("a live view's scale has its shared state");
        let surface = view.surface.as_mut().expect("a live view has a surface");
        let Ok(output) = waited(view.id, shared, surface) else {
            if shared.lost() || shared.take_fault().is_err() {
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
    use super::super::super::style::{Theme, font_for};
    use super::*;

    fn painter(views: Vec<Drawn>) -> Painter {
        let (family, source) = font_for("JetBrains Mono").expect("the built-in font");
        let style = Style {
            family,
            source,
            font_px: 13.0,
            blink: false,
            theme: Theme::default(),
        };
        Painter {
            style,
            shared: Vec::new(),
            views,
            replaced: Replaced::default(),
        }
    }

    fn faulted(id: u64) -> Drawn {
        Drawn {
            id,
            handle: 0,
            geometry: Geometry {
                size: (10, 10),
                scale_key: 100,
            },
            surface: None,
            last: None,
        }
    }

    fn with_shared(mut painter: Painter) -> Painter {
        let shared = Shared::new(1.0, &painter.style).expect("shared state");
        painter.shared.push((100, shared));
        painter
    }

    #[test]
    fn an_unused_scale_keeps_its_shared_state() {
        let mut painter = with_shared(painter(Vec::new()));
        let other = Geometry {
            size: (10, 10),
            scale_key: 150,
        };
        assert!(
            painter.surface(0, other).is_err(),
            "no window to make a surface on"
        );
        assert!(
            painter.shared.iter().any(|(key, _)| *key == 100),
            "shared state no view uses is kept, never dropped"
        );
        assert!(
            painter.shared.iter().any(|(key, _)| *key == 150),
            "the make reached the shared state for its own scale"
        );
    }

    #[test]
    fn a_surface_that_fails_before_its_device_request_spends_no_attempt() {
        let mut painter = painter(Vec::new());
        painter.replaced.lost(100);
        let geometry = faulted(0).geometry;
        for _ in 0..=DEVICE_REPLACE_MAX {
            let made = painter.surface(0, geometry).err();
            assert_eq!(
                made.as_deref(),
                Some("no window"),
                "no device was requested"
            );
        }
        assert!(painter.replaced.attempt(100), "the budget is untouched");
    }

    #[test]
    fn a_view_retired_earlier_in_its_round_is_not_drawn() {
        let id = u64::MAX - 1;
        let geometry = faulted(id).geometry;
        BOARD.add(id, 1, geometry).expect("a view on the board");
        let round = BOARD.wait();
        assert!(
            round.fresh.iter().any(|fresh| fresh.id == id),
            "the board handed the view over"
        );
        BOARD.fault(id);
        painter(vec![faulted(id)]).draw(id);
        assert!(BOARD.faulted(id), "the retired view stays faulted");
    }

    #[test]
    #[should_panic(expected = "a ready view has a drawn entry")]
    fn drawing_a_view_the_render_side_never_made_is_a_bug() {
        painter(Vec::new()).draw(7);
    }

    #[test]
    #[should_panic(expected = "a ready view with no surface was retired")]
    fn drawing_a_view_with_no_surface_the_board_never_faulted_is_a_bug() {
        painter(vec![faulted(7)]).draw(7);
    }

    #[test]
    #[should_panic(expected = "a live view's scale has its shared state")]
    fn refitting_a_view_with_no_shared_state_is_a_bug() {
        let geometry = Geometry {
            size: (20, 20),
            scale_key: 100,
        };
        let _ = painter(vec![faulted(7)]).refit(0, geometry);
    }

    #[test]
    #[should_panic(expected = "a live view's scale has its shared state")]
    fn refitting_a_view_to_a_new_scale_with_no_shared_state_is_a_bug() {
        let geometry = Geometry {
            size: (20, 20),
            scale_key: 150,
        };
        let _ = painter(vec![faulted(7)]).refit(0, geometry);
    }

    #[test]
    #[should_panic(expected = "a faulted view has a window")]
    fn faulting_a_view_with_no_window_is_a_bug() {
        painter(vec![faulted(7)]).fault(0);
    }

    #[test]
    #[should_panic(expected = "a live view has a surface")]
    fn refitting_a_faulted_view_is_a_bug() {
        let geometry = Geometry {
            size: (20, 20),
            scale_key: 100,
        };
        let _ = with_shared(painter(vec![faulted(7)])).refit(0, geometry);
    }

    #[test]
    #[should_panic(expected = "a live view's scale has its shared state")]
    fn presenting_a_view_with_no_shared_state_is_a_bug() {
        let _ = painter(vec![faulted(7)]).present(0);
    }

    #[test]
    #[should_panic(expected = "a live view has a surface")]
    fn presenting_a_faulted_view_is_a_bug() {
        let _ = with_shared(painter(vec![faulted(7)])).present(0);
    }

    #[test]
    #[should_panic(expected = "a view made once")]
    fn making_a_view_twice_is_a_bug() {
        let fresh = Fresh {
            id: 7,
            handle: 0,
            geometry: faulted(7).geometry,
        };
        painter(vec![faulted(7)]).make(fresh);
    }

    #[test]
    #[should_panic(expected = "views are bounded by the board's")]
    fn a_view_past_the_bound_is_a_bug_to_make() {
        let views = (1..=VIEWS_MAX as u64).map(faulted).collect();
        let fresh = Fresh {
            id: 0,
            handle: 0,
            geometry: faulted(0).geometry,
        };
        painter(views).make(fresh);
    }

    #[test]
    #[should_panic(expected = "a made surface has its shared state")]
    fn posting_the_cell_of_a_scale_with_no_shared_state_is_a_bug() {
        painter(Vec::new()).post_cell(7, 100);
    }

    #[test]
    #[should_panic(expected = "a drawn view stays on the board until the render side drops it")]
    fn posting_the_cell_of_a_view_off_the_board_is_a_bug() {
        with_shared(painter(Vec::new())).post_cell(u64::MAX, 100);
    }

    #[test]
    fn a_scale_replaces_a_lost_device_at_most_twice_a_run() {
        let mut replaced = Replaced::default();
        for _ in 0..5 {
            assert!(replaced.attempt(100), "free before any loss");
        }
        replaced.lost(100);
        assert!(replaced.attempt(100), "the first replacement");
        assert!(replaced.attempt(100), "a failed first counts; the second");
        assert!(!replaced.attempt(100), "past the budget");
        assert!(replaced.attempt(150), "another scale keeps its own");
        replaced.lost(125);
        assert!(replaced.attempt(125), "one replacement");
        replaced.lost(125);
        assert!(
            replaced.attempt(125),
            "a second loss, the second replacement"
        );
        replaced.lost(125);
        assert!(
            !replaced.attempt(125),
            "a third loss finds the budget spent"
        );
    }

    #[test]
    #[should_panic(expected = "a scale key")]
    fn a_loss_on_an_unknown_scale_is_a_bug() {
        Replaced::default().lost(99);
    }

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
