use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::place::SCALE_KEYS;

/// The most native views one render thread draws.
pub const VIEWS_MAX: usize = 64;

/// The most swap-chain waits in a row that may fail before a view's change is dropped.
pub const MISSES_MAX: u8 = 3;

/// The plain code for adding a view past `VIEWS_MAX`.
pub const VIEWS_FULL: &str = "native_terminal_views_full";

/// A view's size in device pixels and its display scale key, in percent of 96 DPI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub size: (u32, u32),
    pub scale_key: u32,
}

impl Geometry {
    fn check(self) {
        assert!(self.size.0 >= 1, "a placed width");
        assert!(self.size.1 >= 1, "a placed height");
        assert!(SCALE_KEYS.contains(&self.scale_key), "a scale key");
    }
}

/// A view the render thread has no surface for yet: its id, its native window handle and
/// its geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fresh {
    pub id: u64,
    pub handle: isize,
    pub geometry: Geometry,
}

/// What the render thread does next: drop the surfaces of removed views, make surfaces for
/// fresh views, then draw each ready view.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Round {
    pub gone: Vec<u64>,
    pub fresh: Vec<Fresh>,
    pub ready: Vec<u64>,
}

impl Round {
    fn is_empty(&self) -> bool {
        self.gone.is_empty() && self.fresh.is_empty() && self.ready.is_empty()
    }
}

struct Slot<T> {
    id: u64,
    handle: isize,
    frame: Option<T>,
    framed: bool,
    geometry: Geometry,
    geometry_owed: bool,
    shown: bool,
    changed: bool,
    fresh: bool,
    removing: bool,
}

impl<T> Slot<T> {
    fn ready(&self) -> bool {
        self.shown && self.changed && self.framed && !self.fresh && !self.removing
    }
}

/// The hand-off from the window thread to the render thread: per view, the newest frame,
/// geometry and shown state. A post never waits on drawing and replaces what it posted
/// before, so a burst of changes costs one frame and the last change always lands.
pub struct Board<T> {
    slots: Mutex<Vec<Slot<T>>>,
    bell: Condvar,
}

impl<T> Board<T> {
    /// An empty board.
    pub const fn new() -> Board<T> {
        Board {
            slots: Mutex::new(Vec::new()),
            bell: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Slot<T>>> {
        self.slots.lock().expect("the board lock")
    }

    /// Adds a view, hidden, for the render thread to make a surface for. Inputs: the view
    /// id, its native window handle and geometry. Output: ok, or `VIEWS_FULL`.
    pub fn add(&self, id: u64, handle: isize, geometry: Geometry) -> Result<(), &'static str> {
        geometry.check();
        let mut slots = self.lock();
        assert!(
            slots.iter().all(|slot| slot.id != id),
            "a view id added twice"
        );
        if slots.len() >= VIEWS_MAX {
            return Err(VIEWS_FULL);
        }
        slots.push(Slot {
            id,
            handle,
            frame: None,
            framed: false,
            geometry,
            geometry_owed: false,
            shown: false,
            changed: false,
            fresh: true,
            removing: false,
        });
        assert!(slots.len() <= VIEWS_MAX);
        drop(slots);
        self.bell.notify_all();
        Ok(())
    }

    fn post(&self, id: u64, change: impl FnOnce(&mut Slot<T>)) -> bool {
        let mut slots = self.lock();
        let Some(slot) = slots
            .iter_mut()
            .find(|slot| slot.id == id && !slot.removing)
        else {
            return false;
        };
        change(slot);
        slot.changed = true;
        drop(slots);
        self.bell.notify_all();
        true
    }

    /// Posts a view's newest frame. Inputs: the view id and the frame. Output: whether a
    /// live view took it.
    pub fn post_frame(&self, id: u64, frame: T) -> bool {
        self.post(id, |slot| {
            slot.frame = Some(frame);
            slot.framed = true;
        })
    }

    /// Posts a view's newest geometry. Inputs: the view id and the geometry. Output:
    /// whether a live view took it.
    pub fn post_geometry(&self, id: u64, geometry: Geometry) -> bool {
        geometry.check();
        self.post(id, |slot| {
            slot.geometry = geometry;
            slot.geometry_owed = true;
        })
    }

    /// Posts whether a view is shown; showing redraws it. Inputs: the view id and the
    /// state. Output: whether a live view took it.
    pub fn post_shown(&self, id: u64, shown: bool) -> bool {
        self.post(id, |slot| slot.shown = shown)
    }

    /// Removes a view and waits, up to a bound, until the render thread has dropped its
    /// surface, so the window can be destroyed after it. Inputs: the view id and the
    /// bound. Output: whether no surface is left for it.
    pub fn remove(&self, id: u64, wait: Duration) -> bool {
        let deadline = Instant::now() + wait;
        let mut slots = self.lock();
        let Some(index) = slots.iter().position(|slot| slot.id == id) else {
            return true;
        };
        if slots[index].fresh {
            slots.remove(index);
            return true;
        }
        slots[index].removing = true;
        self.bell.notify_all();
        loop {
            if slots.iter().all(|slot| slot.id != id) {
                return true;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            slots = self
                .bell
                .wait_timeout(slots, left)
                .expect("the board lock")
                .0;
        }
    }

    /// The render thread's next round, waiting while there is none: a round has a removed
    /// view, a fresh view, or a shown view with a frame and a change not yet drawn. Output:
    /// the round.
    pub fn wait(&self) -> Round {
        let mut slots = self.lock();
        loop {
            let round = next_round(&mut slots);
            if !round.is_empty() {
                return round;
            }
            slots = self.bell.wait(slots).expect("the board lock");
        }
    }

    /// The render thread's word that a view's surface is gone, or was never made. Input:
    /// the view id, which the board handed over.
    pub fn dropped(&self, id: u64) {
        let mut slots = self.lock();
        let before = slots.len();
        slots.retain(|slot| slot.id != id);
        assert_eq!(before - slots.len(), 1, "a dropped view was on the board");
        drop(slots);
        self.bell.notify_all();
    }

    /// A view's geometry posted since the render thread last took it, taken before the
    /// swap-chain wait. Input: the view id. Output: the geometry, or none.
    pub fn geometry(&self, id: u64) -> Option<Geometry> {
        let mut slots = self.lock();
        let slot = slots.iter_mut().find(|slot| slot.id == id)?;
        assert!(!slot.fresh, "geometry is taken only after the hand-over");
        let owed = std::mem::take(&mut slot.geometry_owed);
        owed.then_some(slot.geometry)
    }

    /// The render thread's word that a ready view's swap-chain wait failed: the change stays
    /// owed for another round, up to `MISSES_MAX` misses in a row, then is dropped until the
    /// next post, so a surface lost for good cannot spin the thread. Input: the view id.
    /// Output: whether the change is still owed.
    pub fn missed(&self, id: u64) -> bool {
        let _ = id;
        unimplemented!("H1 r2 red")
    }

    /// A view's newest frame, taken after the swap-chain wait, so a frame posted during
    /// the wait is drawn in it; clears the view's change. Input: the view id. Output: the
    /// frame posted since the last take, or none.
    pub fn take(&self, id: u64) -> Option<T> {
        let mut slots = self.lock();
        let slot = slots.iter_mut().find(|slot| slot.id == id)?;
        assert!(!slot.fresh, "a frame is taken only after the hand-over");
        slot.changed = false;
        slot.frame.take()
    }
}

fn next_round<T>(slots: &mut [Slot<T>]) -> Round {
    let mut round = Round::default();
    for slot in slots.iter_mut() {
        if slot.removing {
            round.gone.push(slot.id);
        } else if slot.fresh {
            slot.fresh = false;
            slot.geometry_owed = false;
            round.fresh.push(Fresh {
                id: slot.id,
                handle: slot.handle,
                geometry: slot.geometry,
            });
        } else if slot.ready() {
            round.ready.push(slot.id);
        }
    }
    assert!(round.gone.len() + round.fresh.len() + round.ready.len() <= slots.len());
    round
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Instant;

    const ONE: Geometry = Geometry {
        size: (640, 480),
        scale_key: 100,
    };

    const WAIT: Duration = Duration::from_secs(5);

    fn round_after(board: &Arc<Board<&'static str>>, change: impl FnOnce()) -> Round {
        let (send, receive) = mpsc::channel();
        let render = Arc::clone(board);
        let waiter = thread::spawn(move || send.send(render.wait()).expect("the round"));
        let idle = receive.recv_timeout(Duration::from_millis(100));
        assert!(idle.is_err(), "no round while nothing changed: {idle:?}");
        change();
        let round = receive
            .recv_timeout(WAIT)
            .expect("wait returns after the change");
        waiter.join().expect("the waiter");
        round
    }

    fn round_within(board: &Arc<Board<&'static str>>) -> Round {
        let (send, receive) = mpsc::channel();
        let render = Arc::clone(board);
        thread::spawn(move || send.send(render.wait()).expect("the round"));
        receive
            .recv_timeout(Duration::from_secs(1))
            .expect("a round, not a wait")
    }

    fn shown_board() -> Arc<Board<&'static str>> {
        let board = Arc::new(Board::new());
        board.add(1, 11, ONE).expect("a view");
        let round = board.wait();
        assert_eq!(
            round.fresh,
            vec![Fresh {
                id: 1,
                handle: 11,
                geometry: ONE
            }]
        );
        assert!(round.ready.is_empty());
        assert!(board.post_shown(1, true));
        board
    }

    #[test]
    fn a_fresh_view_is_handed_over_once_with_its_handle_and_geometry() {
        let board = shown_board();
        assert!(board.post_frame(1, "a"));
        let round = board.wait();
        assert!(round.fresh.is_empty());
        assert!(round.gone.is_empty());
        assert_eq!(round.ready, vec![1]);
    }

    #[test]
    fn a_view_draws_only_shown_with_a_frame_and_a_change() {
        let board = shown_board();
        let idle = round_after(&board, || assert!(board.post_frame(1, "a")));
        assert_eq!(idle.ready, vec![1]);
        assert_eq!(board.take(1), Some("a"));
        let hidden = round_after(&board, || {
            assert!(board.post_shown(1, false));
            assert!(board.post_frame(1, "b"));
            board.add(2, 22, ONE).expect("a second view");
        });
        assert!(hidden.ready.is_empty(), "a hidden view never draws");
        assert_eq!(hidden.fresh.len(), 1);
        let again = round_after(&board, || assert!(board.post_shown(1, true)));
        assert_eq!(again.ready, vec![1], "showing redraws");
        assert_eq!(board.take(1), Some("b"));
    }

    #[test]
    fn the_newest_state_wins() {
        let board = shown_board();
        let bigger = Geometry {
            size: (800, 600),
            scale_key: 150,
        };
        assert!(board.post_geometry(
            1,
            Geometry {
                size: (700, 500),
                ..ONE
            }
        ));
        assert!(board.post_geometry(1, bigger));
        assert!(board.post_frame(1, "first"));
        assert!(board.post_frame(1, "second"));
        assert_eq!(board.wait().ready, vec![1]);
        assert_eq!(board.geometry(1), Some(bigger));
        assert_eq!(board.geometry(1), None);
        assert_eq!(board.take(1), Some("second"));
        assert_eq!(board.take(1), None);
    }

    #[test]
    fn a_frame_posted_during_the_swap_chain_wait_lands_in_that_frame() {
        let board = shown_board();
        assert!(board.post_frame(1, "before"));
        assert_eq!(board.wait().ready, vec![1]);
        assert_eq!(board.geometry(1), None);
        assert!(board.post_frame(1, "during"));
        assert_eq!(board.take(1), Some("during"));
        assert!(board.post_frame(1, "after"));
        assert_eq!(
            board.wait().ready,
            vec![1],
            "a change after the take is owed again"
        );
        assert_eq!(board.take(1), Some("after"));
    }

    #[test]
    fn a_geometry_posted_during_the_swap_chain_wait_is_owed_after_the_take() {
        let board = shown_board();
        assert!(board.post_frame(1, "a"));
        assert_eq!(board.wait().ready, vec![1]);
        assert_eq!(board.geometry(1), None);
        let resized = Geometry {
            size: (10, 10),
            ..ONE
        };
        assert!(board.post_geometry(1, resized));
        assert_eq!(board.take(1), Some("a"));
        assert_eq!(
            round_within(&board).ready,
            vec![1],
            "the owed geometry redraws"
        );
        assert_eq!(board.geometry(1), Some(resized));
        assert_eq!(board.take(1), None);
        let idle = round_after(&board, || assert!(board.post_frame(1, "b")));
        assert_eq!(idle.ready, vec![1], "nothing owed after that take");
    }

    #[test]
    fn a_failed_swap_chain_wait_keeps_the_change_for_a_bounded_retry() {
        let board = shown_board();
        assert!(board.post_frame(1, "a"));
        for miss in 1..MISSES_MAX {
            assert_eq!(round_within(&board).ready, vec![1], "retry {miss}");
            assert!(board.missed(1), "miss {miss} keeps the change");
        }
        assert_eq!(round_within(&board).ready, vec![1]);
        assert!(!board.missed(1), "the last miss drops the change");
        let after = round_after(&board, || assert!(board.post_shown(1, true)));
        assert_eq!(after.ready, vec![1], "the next post owes it again");
        assert_eq!(board.take(1), Some("a"), "the frame was never taken");
        assert!(board.post_frame(1, "b"));
        assert_eq!(board.wait().ready, vec![1]);
        assert!(board.missed(1), "a take starts the count again");
    }

    #[test]
    fn a_geometry_change_alone_redraws_the_last_frame() {
        let board = shown_board();
        assert!(board.post_frame(1, "a"));
        assert_eq!(board.wait().ready, vec![1]);
        assert_eq!(board.take(1), Some("a"));
        let resized = round_after(&board, || {
            assert!(board.post_geometry(
                1,
                Geometry {
                    size: (10, 10),
                    ..ONE
                }
            ));
        });
        assert_eq!(resized.ready, vec![1]);
        assert_eq!(
            board.take(1),
            None,
            "no new frame: the render thread redraws its last"
        );
    }

    #[test]
    fn remove_waits_for_the_render_side_drop_within_its_bound() {
        let board = shown_board();
        let started = Instant::now();
        assert!(
            !board.remove(1, Duration::from_millis(200)),
            "no render thread dropped it"
        );
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(200), "{waited:?}");
        assert!(waited < WAIT, "{waited:?}");
        let render = Arc::clone(&board);
        let dropper = thread::spawn(move || {
            let round = render.wait();
            assert_eq!(round.gone, vec![1]);
            render.dropped(1);
        });
        assert!(board.remove(1, WAIT), "the render side dropped it");
        dropper.join().expect("the dropper");
        assert!(!board.post_frame(1, "late"), "a removed view takes no post");
        assert!(!board.post_shown(1, true));
    }

    #[test]
    fn a_view_never_handed_over_is_removed_at_once() {
        let board: Board<&str> = Board::new();
        board.add(5, 55, ONE).expect("a view");
        let started = Instant::now();
        assert!(board.remove(5, WAIT));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!board.post_frame(5, "late"));
        board.add(5, 56, ONE).expect("the id is free again");
    }

    #[test]
    fn views_are_bounded() {
        let board: Board<&str> = Board::new();
        for id in 0..VIEWS_MAX as u64 {
            board.add(id, 1, ONE).expect("a view under the bound");
        }
        assert_eq!(board.add(VIEWS_MAX as u64, 1, ONE), Err(VIEWS_FULL));
        assert!(board.remove(0, WAIT));
        board.add(VIEWS_MAX as u64, 1, ONE).expect("room again");
    }

    #[test]
    #[should_panic(expected = "a view id added twice")]
    fn adding_an_id_twice_is_a_bug() {
        let board: Board<&str> = Board::new();
        board.add(1, 1, ONE).expect("a view");
        let _ = board.add(1, 2, ONE);
    }
}
