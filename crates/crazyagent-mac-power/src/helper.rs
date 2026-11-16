//! The helper's core (threat model sections 6, 8, 9 and 10): leases, the journal, the uid pin
//! and the floor, driven by verified requests, lease expiry and readings every 30 s. It runs
//! `pmset` only through `Pmset` and touches files only in its folder.

use crate::ffi;
use crate::floor::{self, Battery, Floor, Pressure};
use crate::journal::{self, Entry, Journal, Key};
use crate::lease::{HOLDERS_MAX, Holder, Leases};
use crate::lid::Lid;
use crate::pin::{self, Pin};
use crate::pmset::{Pmset, Values};
use crate::protocol::{
    FloorView, LEASE_S_MAX, LEASE_S_MIN, PinView, Refusal, Reply, Request, View,
};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub const FOLDER: &str = "/Library/Application Support/dev.crazyagent.power";

/// Where the helper keeps its state and what it runs: its folder and the uid that must own it
/// (root in production), the `pmset` program, and the console whose owner it pins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub folder: PathBuf,
    pub owner_uid: u32,
    pub pmset: PathBuf,
    pub console: PathBuf,
}

impl Config {
    /// The installed helper's configuration.
    pub fn production() -> Self {
        Self {
            folder: PathBuf::from(FOLDER),
            owner_uid: 0,
            pmset: PathBuf::from(crate::pmset::PMSET),
            console: PathBuf::from(crate::pin::CONSOLE),
        }
    }
}

/// One reading for the floor, taken every 30 s while holding, while the floor holds, or while
/// a value waits to be put back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Readings {
    pub battery: Battery,
    pub pressure: Pressure,
    pub lid: Option<Lid>,
}

/// The running helper's state.
#[derive(Debug)]
pub struct Helper {
    config: Config,
    pmset: Pmset,
    journal: Option<Journal>,
    leases: Leases,
    pin: Pin,
    floor: Option<Floor>,
}

impl Helper {
    /// Starts the helper: opens the journal, puts back every entry left in it, and reads or
    /// writes the pin. Input: the configuration. Output: the helper.
    pub fn start(config: Config) -> Self {
        let pmset = Pmset::new(&config.pmset);
        let journal = Journal::open(&config.folder, config.owner_uid)
            .inspect_err(|error| ffi::log(&format!("journal unusable: {error:?}")))
            .ok();
        let mut helper = Self {
            config,
            pmset,
            journal,
            leases: Leases::default(),
            pin: Pin::None,
            floor: None,
        };
        helper.put_back(None, false);
        let config = &helper.config;
        helper.pin = pin::at_start(&config.folder, config.owner_uid, &config.console)
            .inspect_err(|error| ffi::log(&format!("pin unusable: {error:?}")))
            .unwrap_or(Pin::None);
        assert!(helper.leases.is_empty());
        helper
    }

    /// Answers one verified request. Inputs: the caller, the request, the monotonic time and
    /// the lid. Output: the reply.
    pub fn handle(
        &mut self,
        holder: Holder,
        request: Request,
        now_ms: u64,
        lid: Option<Lid>,
    ) -> Reply {
        let reply = match request {
            Request::Hold { lease_s } => self.hold(holder, lease_s, now_ms, lid),
            Request::Release => self.release(holder, lid),
            Request::Read => Reply::State(self.view(holder, now_ms, lid)),
            Request::Forget => self.forget(holder, lid),
        };
        assert!(self.leases.len() <= HOLDERS_MAX);
        assert!(self.floor.is_none() || self.leases.is_empty());
        reply
    }

    /// Ends the leases that ran out at `now_ms`, putting sleep back when none is left.
    pub fn expire(&mut self, now_ms: u64, lid: Option<Lid>) {
        let ended = self.leases.expire(now_ms);
        if ended > 0 && self.leases.is_empty() {
            self.put_back(lid, true);
        }
        assert!(!self.has_expired(now_ms));
    }

    /// Whether a lease ran out at `now_ms`, so the caller reads the lid before `expire`.
    pub fn has_expired(&self, now_ms: u64) -> bool {
        let next = self.leases.next_expiry_ms();
        next.is_some_and(|until_ms| now_ms >= until_ms)
    }

    /// Applies one reading: lease expiry, the floor, a value still to put back tried again,
    /// and `SleepDisabled` set again if another tool reset it while a lease lives.
    pub fn sense(&mut self, now_ms: u64, readings: Readings) {
        self.expire(now_ms, readings.lid);
        if !self.wants_readings() {
            return;
        }
        if let Some(floor) = floor::judge(readings.battery, readings.pressure) {
            self.end_for_floor(floor, readings.lid);
            return;
        }
        self.floor = None;
        if self.leases.is_empty() {
            self.put_back(readings.lid, false);
            return;
        }
        if let Err(reason) = self.ensure_disabled() {
            ffi::log(&format!("SleepDisabled not set again: {reason:?}"));
        }
    }

    /// Puts every journaled value back before the helper exits on SIGTERM.
    pub fn terminate(&mut self, lid: Option<Lid>) {
        let was_holding = !self.leases.is_empty();
        self.leases.clear();
        self.put_back(lid, was_holding);
        assert!(self.leases.is_empty());
    }

    /// Whether readings are due: a lease lives, the floor holds, or a value waits to be put
    /// back.
    pub fn wants_readings(&self) -> bool {
        !self.leases.is_empty() || self.floor.is_some() || self.pending()
    }

    /// Whether no lease lives and no value waits to be put back, so the helper may exit after
    /// its idle time.
    pub fn idle(&self) -> bool {
        self.leases.is_empty() && !self.pending()
    }

    /// Whether the journal holds an entry not yet put back.
    fn pending(&self) -> bool {
        let journal = self.journal.as_ref();
        journal.is_some_and(|journal| !journal.entries().is_empty())
    }

    /// Starts or renews `holder`'s lease once the pin, the floor and the journal allow it, and
    /// makes sure `SleepDisabled` is 1. Output: `Held`, or why the hold was refused.
    fn hold(&mut self, holder: Holder, lease_s: u16, now_ms: u64, lid: Option<Lid>) -> Reply {
        assert!(lease_s >= LEASE_S_MIN);
        assert!(lease_s <= LEASE_S_MAX);
        let refusal = if self.pin != Pin::Uid(holder.euid) {
            Some(Refusal::Unpinned)
        } else if self.floor.is_some() {
            Some(Refusal::Floor)
        } else if self.journal.is_none() {
            Some(Refusal::JournalUnreadable)
        } else if self.leases.hold(holder, now_ms, lease_s).is_err() {
            Some(Refusal::HoldersFull)
        } else {
            None
        };
        if let Some(reason) = refusal {
            return Reply::Refused { reason };
        }
        if let Err(reason) = self.ensure_disabled() {
            self.leases.release(&holder);
            if self.leases.is_empty() {
                self.put_back(lid, false);
            }
            return Reply::Refused { reason };
        }
        Reply::Held {
            lease_left_s: lease_s,
        }
    }

    /// Ends `holder`'s own lease, putting sleep back when none is left. Output: `Released`.
    fn release(&mut self, holder: Holder, lid: Option<Lid>) -> Reply {
        if self.leases.release(&holder) && self.leases.is_empty() {
            self.put_back(lid, true);
        }
        Reply::Released
    }

    /// Forgets the pin (Remove lid helper): allowed for the pinned uid, or under no pin for the
    /// non-system console user. Ends every lease and puts sleep back first. Output: the reply.
    fn forget(&mut self, holder: Holder, lid: Option<Lid>) -> Reply {
        let allowed = match self.pin {
            Pin::Uid(uid) => uid == holder.euid,
            Pin::None => pin::console_user(&self.config.console) == Some(holder.euid),
        };
        if !allowed {
            return Reply::Refused {
                reason: Refusal::Unpinned,
            };
        }
        let was_holding = !self.leases.is_empty();
        self.leases.clear();
        self.put_back(lid, was_holding);
        self.pin = Pin::None;
        let folder = journal::checked_folder(&self.config.folder, self.config.owner_uid);
        match folder.and_then(|folder| pin::forget(&folder)) {
            Ok(()) => Reply::Forgotten,
            Err(error) => {
                ffi::log(&format!("pin file not removed: {error:?}"));
                Reply::Refused {
                    reason: Refusal::JournalUnreadable,
                }
            }
        }
    }

    /// The state as one caller sees it. Inputs: the caller, the time and the lid.
    fn view(&self, holder: Holder, now_ms: u64, lid: Option<Lid>) -> View {
        let values = self.pmset.values().unwrap_or_else(|error| {
            ffi::log(&format!("pmset -g unread: {error:?}"));
            Values::default()
        });
        let ours = self.journal.as_ref();
        let ours = ours.is_some_and(|journal| journal.entry(Key::SleepDisabled).is_some());
        let view = View {
            held: !self.leases.is_empty(),
            holders: u8::try_from(self.leases.len()).expect("at most HOLDERS_MAX leases"),
            lease_left_s: self.leases.left_s(now_ms),
            sleep_disabled: values.sleep_disabled,
            foreign: values.sleep_disabled == Some(true) && !ours,
            journal_ok: self.journal.is_some(),
            floor: floor_view(self.floor),
            pin: pin_view(self.pin, holder),
            lid_closed: lid.map(|lid| lid.closed),
            external_display: lid.map(|lid| lid.external_display),
            low_power: values.low_power,
        };
        assert_eq!(view.held, view.holders > 0);
        assert!(view.held || view.lease_left_s == 0);
        view
    }

    /// Ends every lease because the floor applies, and puts sleep back.
    fn end_for_floor(&mut self, floor: Floor, lid: Option<Lid>) {
        let was_holding = !self.leases.is_empty();
        self.leases.clear();
        if self.floor != Some(floor) {
            ffi::log(&format!("floor {floor:?}: every lease ended"));
        }
        self.floor = Some(floor);
        self.put_back(lid, was_holding);
        assert!(self.leases.is_empty());
    }

    /// Makes `SleepDisabled` 1: left alone when already 1 (ours or foreign), else journaled
    /// first and then set. Output: the result, or why it could not be done.
    fn ensure_disabled(&mut self) -> Result<(), Refusal> {
        let values = self.pmset.values().map_err(|error| {
            ffi::log(&format!("pmset -g failed: {error:?}"));
            Refusal::Pmset
        })?;
        match values.sleep_disabled {
            Some(true) => return Ok(()),
            Some(false) => {}
            None => return Err(Refusal::Pmset),
        }
        let journal = self.journal.as_mut().ok_or(Refusal::JournalUnreadable)?;
        if journal.entry(Key::SleepDisabled).is_none() {
            let entry = Entry {
                key: Key::SleepDisabled,
                before: 0,
                at_ms: wall_ms(),
            };
            journal.record(entry).map_err(|error| {
                ffi::log(&format!("journal not written: {error:?}"));
                Refusal::JournalUnreadable
            })?;
        }
        assert!(journal.entry(Key::SleepDisabled).is_some());
        self.pmset.set(Key::SleepDisabled, true).map_err(|error| {
            ffi::log(&format!("pmset set failed: {error:?}"));
            Refusal::Pmset
        })
    }

    /// Puts every journaled value back, removing each entry only after `pmset` succeeded, and
    /// sleeps the Mac now when a hold just ended, sleep was put back and the lid sleeps alone.
    fn put_back(&mut self, lid: Option<Lid>, was_holding: bool) {
        let Some(journal) = self.journal.as_mut() else {
            return;
        };
        let entries = journal.entries().to_vec();
        let mut sleep_back = false;
        for entry in entries {
            if let Err(error) = self.pmset.set(entry.key, entry.before == 1) {
                ffi::log(&format!("put back failed, entry kept: {error:?}"));
                continue;
            }
            sleep_back |= entry.key == Key::SleepDisabled && entry.before == 0;
            if let Err(error) = journal.remove(entry.key) {
                ffi::log(&format!("entry not removed: {error:?}"));
            }
        }
        let sleeps = was_holding && sleep_back && lid.is_some_and(Lid::sleeps_now);
        if sleeps && let Err(error) = self.pmset.sleep_now() {
            ffi::log(&format!("sleepnow failed: {error:?}"));
        }
    }
}

/// The floor as `Read` reports it.
fn floor_view(floor: Option<Floor>) -> FloorView {
    match floor {
        None => FloorView::None,
        Some(Floor::Battery) => FloorView::Battery,
        Some(Floor::Heat) => FloorView::Heat,
    }
}

/// The pin as `Read` reports it to `holder`.
fn pin_view(pin: Pin, holder: Holder) -> PinView {
    match pin {
        Pin::Uid(uid) if uid == holder.euid => PinView::You,
        Pin::Uid(_) => PinView::Other,
        Pin::None => PinView::None,
    }
}

/// The wall-clock millisecond a journal entry records.
fn wall_ms() -> i64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH);
    let since = since.unwrap_or_default();
    i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
}
