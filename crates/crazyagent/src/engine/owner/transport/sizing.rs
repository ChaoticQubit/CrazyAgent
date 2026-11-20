use crate::terminal::{HostError, SIZE_MAX, SIZE_MIN};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// Sizes one unit's window through the view that holds it. Takes the columns and rows.
/// Returns once the host applied them, or its error.
pub(crate) type Apply = Arc<dyn Fn((u16, u16)) -> Result<(), HostError> + Send + Sync>;

/// The latest size a follower of a unit asked for: the view that asked, the size and the call
/// that sizes the window through that view.
struct Ask {
    view: u64,
    size: (u16, u16),
    apply: Apply,
}

/// One unit's sizing: how many of its views size it and the latest follower ask, applied
/// once when the last sizer ends. Its lock orders the unit's sizes.
#[derive(Default)]
struct Unit {
    sizers: usize,
    ask: Option<Ask>,
}

/// One unit in the table: its sizing and how many views sit in it, counted under the
/// table's lock so a unit is forgotten only once no view of it is left or on its way in.
#[derive(Default)]
struct Room {
    place: Arc<Mutex<Unit>>,
    seated: usize,
}

/// Every unit a live view sits in, by the host's unit key, the seats open and the last
/// view number given. Its lock is never held while a unit's lock is waited on.
#[derive(Default)]
struct Table {
    units: HashMap<String, Room>,
    seats: usize,
    views: u64,
}

/// The engine's sizing of terminal units (V243-PANE-SIZE-ACROSS-WINDOWS): a follower sizes its
/// unit only while no sizer view of that unit is open, and the latest size a follower asked
/// for is applied once when the last sizer ends. Each unit's own lock orders its sizes, so a
/// slow server never holds up another unit's.
#[derive(Default)]
pub(crate) struct Units {
    table: Mutex<Table>,
}

/// One view's place in its unit for the view's whole life. Dropping it, which every way a
/// view ends does, takes a sizer off the unit's count or a follower's ask off the unit, on a
/// blocking thread when a runtime runs, since the last sizer's end may size the window.
pub(crate) struct Seat {
    units: Arc<Units>,
    unit: String,
    place: Arc<Mutex<Unit>>,
    view: u64,
    follows: bool,
    apply: Apply,
}

/// Takes a lock, past a panic that held it, since every count it guards is whole between
/// statements. Takes the lock. Returns its guard.
fn locked<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Units {
    /// Seats one view in its unit; a sizer counts from now on. Takes the units, the unit's
    /// key from `Host::unit`, whether the view follows and the call that sizes the window
    /// through it. Returns the seat.
    pub(crate) fn seat(units: &Arc<Units>, unit: String, follows: bool, apply: Apply) -> Seat {
        let mut table = locked(&units.table);
        table.views += 1;
        table.seats += 1;
        let view = table.views;
        let room = table.units.entry(unit.clone()).or_default();
        room.seated += 1;
        let place = Arc::clone(&room.place);
        assert!(table.units.len() <= table.seats);
        drop(table);
        locked(&place).sizers += usize::from(!follows);
        Seat {
            units: Arc::clone(units),
            unit,
            place,
            view,
            follows,
            apply,
        }
    }

    /// Takes one view out of its unit: a follower's own ask goes, a sizer leaves the count,
    /// which never goes below 0, and the last sizer's end applies the follower ask once,
    /// under the unit's lock only; then the seat leaves the table, and a unit with no view
    /// left is forgotten. Takes the seat's unit key and place, its view and whether it follows.
    fn release(&self, unit: &str, place: &Arc<Mutex<Unit>>, view: u64, follows: bool) {
        let mut held = locked(place);
        let mut replay = None;
        if follows {
            if held.ask.as_ref().is_some_and(|ask| ask.view == view) {
                held.ask = None;
            }
        } else {
            assert!(held.sizers > 0, "a unit's sizer count never goes below 0");
            held.sizers -= 1;
            if held.sizers == 0 {
                replay = held
                    .ask
                    .as_ref()
                    .map(|ask| (Arc::clone(&ask.apply), ask.size));
            }
        }
        if let Some((apply, size)) = replay
            && let Err(error) = apply(size)
        {
            eprintln!(
                "terminal_follow_replay_refused code={}",
                crate::protocol::plain_code(&error.0)
            );
        }
        drop(held);
        let mut table = locked(&self.table);
        let room = table
            .units
            .get_mut(unit)
            .expect("a seat's unit stays while the seat does");
        assert!(Arc::ptr_eq(&room.place, place));
        assert!(room.seated > 0, "a unit's seat count never goes below 0");
        room.seated -= 1;
        if room.seated == 0 {
            let held = locked(place);
            assert!(held.sizers == 0 && held.ask.is_none());
            drop(held);
            table.units.remove(unit);
        }
        assert!(table.seats > 0, "the seat count never goes below 0");
        table.seats -= 1;
        assert!(table.units.len() <= table.seats);
    }

    /// One unit's open sizer views and its follower ask's size, none for a unit no view
    /// sits in. Takes the unit's key.
    #[cfg(test)]
    fn standing(&self, unit: &str) -> Option<(usize, Option<(u16, u16)>)> {
        let place = Arc::clone(&locked(&self.table).units.get(unit)?.place);
        let held = locked(&place);
        Some((held.sizers, held.ask.as_ref().map(|ask| ask.size)))
    }
}

impl Seat {
    /// Sizes the unit for this view under the unit's lock, so a sizer seated meanwhile wins:
    /// a sizer's size always; a follower's kept as the unit's ask and applied only while no
    /// sizer is open. Takes the size, within `SIZE_MIN..=SIZE_MAX`. Returns whether it was
    /// applied, `terminal_size_bounds`, or the host's error.
    pub(crate) fn size(&self, size: (u16, u16)) -> Result<bool, HostError> {
        let bounds = SIZE_MIN..=SIZE_MAX;
        if !bounds.contains(&size.0) || !bounds.contains(&size.1) {
            return Err(HostError("terminal_size_bounds".into()));
        }
        let mut held = locked(&self.place);
        if self.follows {
            held.ask = Some(Ask {
                view: self.view,
                size,
                apply: Arc::clone(&self.apply),
            });
            if held.sizers > 0 {
                return Ok(false);
            }
        }
        assert!(!self.follows || held.sizers == 0);
        (self.apply)(size)?;
        Ok(true)
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        let units = Arc::clone(&self.units);
        let place = Arc::clone(&self.place);
        let unit = std::mem::take(&mut self.unit);
        let (view, follows) = (self.view, self.follows);
        let release = move || units.release(&unit, &place, view, follows);
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => drop(runtime.spawn_blocking(release)),
            Err(_) => release(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every size one apply was given, in order.
    type Record = Arc<Mutex<Vec<(u16, u16)>>>;

    /// An apply that records every size it is given. Returns the apply and the record.
    fn recorded() -> (Apply, Record) {
        let sizes = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&sizes);
        let apply: Apply = Arc::new(move |size| {
            kept.lock().unwrap().push(size);
            Ok(())
        });
        (apply, sizes)
    }

    /// V243-PANE-SIZE-ACROSS-WINDOWS: a follower alone sizes; while a sizer sits in its unit
    /// its asks are kept, not applied; the last sizer's end applies the latest ask once; a
    /// follower's end takes its own ask away; another unit is never touched.
    #[test]
    fn a_follower_sizes_alone_and_its_last_ask_lands_once_the_last_sizer_ends() {
        let units = Arc::new(Units::default());
        let (tile, tiled) = recorded();
        let (main, mained) = recorded();
        let follower = Units::seat(&units, "s @1".into(), true, tile);
        assert_eq!(follower.size((47, 11)), Ok(true), "a follower alone sizes");
        let sizer = Units::seat(&units, "s @1".into(), false, Arc::clone(&main));
        let second = Units::seat(&units, "s @1".into(), false, main);
        assert_eq!(sizer.size((100, 30)), Ok(true));
        assert_eq!(follower.size((40, 10)), Ok(false), "a sizer is open");
        assert_eq!(follower.size((41, 12)), Ok(false));
        assert_eq!(
            follower.size((1, 12)),
            Err(HostError("terminal_size_bounds".into()))
        );
        assert_eq!(units.standing("s @1"), Some((2, Some((41, 12)))));
        drop(sizer);
        assert_eq!(*tiled.lock().unwrap(), [(47, 11)], "one sizer still open");
        drop(second);
        assert_eq!(
            *tiled.lock().unwrap(),
            [(47, 11), (41, 12)],
            "the latest ask, once"
        );
        assert_eq!(*mained.lock().unwrap(), [(100, 30)]);
        assert_eq!(
            follower.size((44, 13)),
            Ok(true),
            "the follower's next resize applies once no sizer is open"
        );
        let (other, othered) = recorded();
        let elsewhere = Units::seat(&units, "s @2".into(), false, other);
        drop(elsewhere);
        assert!(othered.lock().unwrap().is_empty());
        let (late, lated) = recorded();
        let late = Units::seat(&units, "s @1".into(), false, late);
        drop(follower);
        assert_eq!(
            units.standing("s @1"),
            Some((1, None)),
            "a follower's end takes its ask"
        );
        drop(late);
        assert!(lated.lock().unwrap().is_empty());
        assert_eq!(
            *tiled.lock().unwrap(),
            [(47, 11), (41, 12), (44, 13)],
            "no ask left to replay"
        );
        assert_eq!(
            units.standing("s @1"),
            None,
            "a unit with no view is forgotten"
        );
        assert_eq!(locked(&units.table).seats, 0);
    }

    /// A sizer seat its unit never counted is a bug, and its end trips the count's floor.
    #[test]
    #[should_panic(expected = "a unit's sizer count never goes below 0")]
    fn a_sizer_count_never_goes_below_zero() {
        let units = Arc::new(Units::default());
        let (apply, _) = recorded();
        let place = Arc::new(Mutex::new(Unit::default()));
        {
            let mut table = locked(&units.table);
            table.seats = 1;
            let room = Room {
                place: Arc::clone(&place),
                seated: 1,
            };
            table.units.insert("s @1".into(), room);
        }
        drop(Seat {
            units: Arc::clone(&units),
            unit: "s @1".into(),
            place,
            view: 1,
            follows: false,
            apply,
        });
    }
}
