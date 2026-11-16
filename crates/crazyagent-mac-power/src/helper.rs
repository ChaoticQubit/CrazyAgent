//! The helper's core (threat model sections 6, 8, 9 and 10): leases, the journal, the uid pin
//! and the floor, driven by verified requests, lease expiry and readings every 30 s. It runs
//! `pmset` only through `Pmset` and touches files only in its folder.

use crate::ffi;
use crate::floor::{self, Battery, Floor, Pressure};
use crate::journal::{self, Entry, Journal, Key};
use crate::lease::{Ask, HOLDERS_MAX, Holder, Leases};
use crate::lid::Lid;
use crate::pin::{self, Pin};
use crate::pmset::{Pmset, Values};
use crate::protocol::{
    FloorView, LEASE_S_MAX, LEASE_S_MIN, PinView, Refusal, Reply, Request, View,
};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub const FOLDER: &str = "/Library/Application Support/dev.crazyagent.power";

/// The most `pmset` runs a served reply may wait on: the engine's call wait covers this many
/// runs of `pmset::WAIT` each and no more (rv-code ka12 F1).
pub const REPLY_PMSET_RUNS_MAX: usize = 2;

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

/// The running helper's state. `display_slept` says `displaysleepnow` ran since the lid
/// last read open or at a desk, or since no sleep-off lease lived. `owed_at_start` says the
/// start put back a `SleepDisabled` entry of ours and no hold was accepted since (QUESTION
/// 38e). `low_power_left` says the battery's Low Power Mode is left alone until no lease asks
/// for it: the owner's own value, a Mac without the key, or ours turned off by the owner
/// (threat model 15.2, N1).
#[derive(Debug)]
pub struct Helper {
    config: Config,
    pmset: Pmset,
    journal: Option<Journal>,
    leases: Leases,
    pin: Pin,
    floor: Option<Floor>,
    sleep_owed: bool,
    display_slept: bool,
    owed_at_start: bool,
    low_power_left: bool,
}

/// Opens the journal in `config`'s folder, logging why when it cannot be used.
fn open_journal(config: &Config) -> Option<Journal> {
    Journal::open(&config.folder, config.owner_uid)
        .inspect_err(|error| ffi::log(&format!("journal unusable: {error:?}")))
        .ok()
}

impl Helper {
    /// Starts the helper: opens the journal, puts back every entry left in it, and reads or
    /// writes the pin. A `SleepDisabled` of ours put back marks sleep as owed, never slept
    /// here, since the request that started the helper is not served yet; one not put back
    /// owes it through `sleep_owed` (QUESTION 38e). A `low_power` entry goes back only while
    /// the battery value still reads what the helper set (threat model 15.5). Input: the
    /// configuration. Output: the helper.
    pub fn start(config: Config) -> Self {
        let pmset = Pmset::new(&config.pmset);
        let journal = open_journal(&config);
        let mut helper = Self {
            config,
            pmset,
            journal,
            leases: Leases::default(),
            pin: Pin::None,
            floor: None,
            sleep_owed: false,
            display_slept: false,
            owed_at_start: false,
            low_power_left: false,
        };
        helper.owed_at_start = helper.put_back_sleep(None, true);
        helper.put_back_low_power(true);
        let config = &helper.config;
        helper.pin = pin::at_start(&config.folder, config.owner_uid, &config.console)
            .inspect_err(|error| ffi::log(&format!("pin unusable: {error:?}")))
            .unwrap_or(Pin::None);
        assert!(helper.leases.is_empty());
        helper
    }

    /// Answers one verified request. It keeps the display's count but never sleeps it and
    /// never changes Low Power Mode: those wait for the next reading, so the reply waits on at
    /// most `REPLY_PMSET_RUNS_MAX` pmset runs (rv-code ka12 F1, threat model 15.2). Inputs:
    /// the caller, the request, the monotonic time and the lid. Output: the reply.
    pub fn handle(
        &mut self,
        holder: Holder,
        request: Request,
        now_ms: u64,
        lid: Option<Lid>,
    ) -> Reply {
        let reply = match request {
            Request::Hold {
                lease_s,
                lid_display,
                low_power,
            } => {
                let ask = Ask::SleepOff {
                    lid_display,
                    low_power,
                };
                self.hold(holder, lease_s, ask, now_ms, lid)
            }
            Request::LowPower { lease_s } => self.low_power(holder, lease_s, now_ms, lid),
            Request::Release => self.release(holder, lid),
            Request::Read => Reply::State(self.view(holder, now_ms, lid)),
            Request::Forget => self.forget(holder, lid),
        };
        self.end_left_alone();
        let display_due = self.display_due(lid);
        assert!(!display_due || self.leases.asks_display_sleep());
        assert!(self.leases.len() <= HOLDERS_MAX);
        assert!(self.floor.is_none() || !self.leases.sleep_off());
        assert!(!self.sleep_owed || self.has_entry(Key::SleepDisabled));
        assert!(!self.owed_at_start || !self.leases.sleep_off());
        assert!(!self.display_slept || self.leases.sleep_off());
        reply
    }

    /// Ends the leases that ran out at `now_ms`, putting sleep back when the last sleep-off
    /// lease ended; Low Power Mode goes back at the next reading.
    pub fn expire(&mut self, now_ms: u64, lid: Option<Lid>) {
        let was_sleep_off = self.leases.sleep_off();
        let ended = self.leases.expire(now_ms);
        assert!(ended > 0 || self.leases.sleep_off() == was_sleep_off);
        if was_sleep_off && !self.leases.sleep_off() {
            self.put_back_sleep(lid, true);
        }
        self.end_left_alone();
        self.sleep_display(lid);
        assert!(!self.has_expired(now_ms));
        assert!(!self.display_slept || self.leases.sleep_off());
    }

    /// Whether a lease ran out at `now_ms`, so the caller reads the lid before `expire`.
    pub fn has_expired(&self, now_ms: u64) -> bool {
        let next = self.leases.next_expiry_ms();
        next.is_some_and(|until_ms| now_ms >= until_ms)
    }

    /// Applies one reading: lease expiry, the floor, Low Power Mode set or put back, the sleep
    /// a start owes, a value still to put back tried again, `SleepDisabled` set again if
    /// another tool reset it while a sleep-off lease lives, and the display's sleep once per
    /// close.
    pub fn sense(&mut self, now_ms: u64, readings: Readings) {
        self.expire(now_ms, readings.lid);
        if self.wants_readings() {
            self.apply(readings);
        }
        self.sleep_display(readings.lid);
        assert!(!self.owed_at_start);
        assert!(!self.display_slept || self.leases.sleep_off());
    }

    /// The part of `sense` that runs while readings are wanted. Low Power Mode is settled
    /// before any `sleepnow`, so its write never races the system's sleep (39i). Input: the
    /// reading.
    fn apply(&mut self, readings: Readings) {
        let owed = self.owed_at_start;
        assert!(!owed || !self.leases.sleep_off(), "an accepted hold clears the owed sleep");
        let ended = match floor::judge(readings.battery, readings.pressure) {
            Some(floor) => self.end_for_floor(floor),
            None => {
                self.floor = None;
                false
            }
        };
        self.settle_low_power();
        if std::mem::take(&mut self.owed_at_start) {
            self.sleep_owed_at_start(readings.lid);
        }
        if !self.leases.sleep_off() {
            self.put_back_sleep(readings.lid, ended);
        } else if let Err(reason) = self.ensure_disabled() {
            ffi::log(&format!("SleepDisabled not set again: {reason:?}"));
        }
        assert!(self.floor.is_none() || !self.leases.sleep_off());
    }

    /// Puts every journaled value back before the helper exits on SIGTERM, Low Power Mode
    /// first, and serves a sleep the start still owes last, so no put-back races the system's
    /// sleep and a SIGTERM before the first reading does not skip it (rv-secreview ka12 N2,
    /// QUESTION 39i). Input: the lid.
    pub fn terminate(&mut self, lid: Option<Lid>) {
        let was_sleep_off = self.leases.sleep_off();
        let owed_at_start = std::mem::take(&mut self.owed_at_start);
        self.leases.clear();
        self.end_left_alone();
        self.put_back_low_power(true);
        self.put_back_sleep(lid, was_sleep_off);
        if owed_at_start {
            self.sleep_owed_at_start(lid);
        }
        let display_due = self.display_due(lid);
        assert!(!display_due, "no lease lives, so the display is never due");
        assert!(self.leases.is_empty());
        assert!(!self.display_slept);
        assert!(!self.owed_at_start);
    }

    /// Whether readings are due: a lease lives, the floor holds, a value waits to be put
    /// back, or the start owes sleep.
    pub fn wants_readings(&self) -> bool {
        !self.leases.is_empty() || self.floor.is_some() || self.pending() || self.owed_at_start
    }

    /// Whether no lease lives, no value waits to be put back and the start owes no sleep, so
    /// the helper may exit after its idle time.
    pub fn idle(&self) -> bool {
        self.leases.is_empty() && !self.pending() && !self.owed_at_start
    }

    /// Sleeps the Mac for the start, at the first reading with no sleep-off lease: when the
    /// lid reads closed with no external display. Input: the lid, none when unread.
    fn sleep_owed_at_start(&self, lid: Option<Lid>) {
        assert!(!self.leases.sleep_off());
        if lid.is_some_and(Lid::sleeps_now)
            && let Err(error) = self.pmset.sleep_now()
        {
            ffi::log(&format!("sleepnow owed at start failed: {error:?}"));
        }
    }

    /// Keeps the once-per-close count (QUESTION 38c, 38d) and runs nothing: the count starts
    /// again when no sleep-off lease lives and when the lid reads open or at a desk; an
    /// unread lid changes nothing. Input: the lid. Output: whether the display is due to
    /// sleep, which needs a live sleep-off lease that asks for it, the lid closed alone and no
    /// run since.
    fn display_due(&mut self, lid: Option<Lid>) -> bool {
        if !self.leases.sleep_off() {
            self.display_slept = false;
            return false;
        }
        let Some(lid) = lid else {
            return false;
        };
        if !lid.sleeps_now() {
            self.display_slept = false;
            return false;
        }
        !self.display_slept && self.leases.asks_display_sleep()
    }

    /// Runs `pmset displaysleepnow` when the display is due, after a reading or an expiry,
    /// never inside a reply (rv-code ka12 F1). A failed run is logged and waits for the next
    /// close. Input: the lid.
    fn sleep_display(&mut self, lid: Option<Lid>) {
        if !self.display_due(lid) {
            return;
        }
        assert!(self.leases.sleep_off());
        assert!(self.leases.asks_display_sleep());
        self.display_slept = true;
        if let Err(error) = self.pmset.display_sleep_now() {
            ffi::log(&format!("displaysleepnow failed: {error:?}"));
        }
    }

    /// Whether the journal holds an entry not yet put back.
    fn pending(&self) -> bool {
        let journal = self.journal.as_ref();
        journal.is_some_and(|journal| !journal.entries().is_empty())
    }

    /// Whether the journal holds an entry for `key`.
    fn has_entry(&self, key: Key) -> bool {
        let journal = self.journal.as_ref();
        journal.is_some_and(|journal| journal.entry(key).is_some())
    }

    /// Starts or renews `holder`'s sleep-off lease, with its display and Low Power asks, once
    /// the pin, the floor and the journal allow it and `SleepDisabled` is 1; an accepted hold
    /// owes no sleep. When that fails, a renewal keeps the lease it had, to run out and put
    /// sleep back on its own; a new hold changes nothing. Output: `Held`, or the refusal.
    fn hold(
        &mut self,
        holder: Holder,
        lease_s: u16,
        ask: Ask,
        now_ms: u64,
        lid: Option<Lid>,
    ) -> Reply {
        assert!(lease_s >= LEASE_S_MIN);
        assert!(lease_s <= LEASE_S_MAX);
        assert!(ask.sleep_off());
        if let Some(reason) = self.refusal(holder, ask) {
            return Reply::Refused { reason };
        }
        let renewing = self.leases.holds(&holder);
        if let Err(reason) = self.ensure_disabled() {
            if !self.leases.sleep_off() {
                self.put_back_sleep(lid, false);
            }
            assert_eq!(self.leases.holds(&holder), renewing);
            return Reply::Refused { reason };
        }
        let held = self.leases.hold(holder, now_ms, lease_s, ask);
        assert!(held.is_ok(), "room for a new holder was checked before pmset ran");
        self.sleep_owed = false;
        self.owed_at_start = false;
        Reply::Held {
            lease_left_s: lease_s,
        }
    }

    /// Starts or renews `holder`'s power-only lease (threat model 15.1): the pin, journal and
    /// holder rules of `hold` but never the floor, which ends and refuses only sleep-off
    /// leases (r9.1 F3). Turning the last sleep-off lease power-only ends that hold:
    /// `SleepDisabled` goes back and a Mac closed alone sleeps, the reply's two runs (r9.1
    /// F1). Low Power Mode is set at the next reading. Output: `Held`, or the refusal.
    fn low_power(&mut self, holder: Holder, lease_s: u16, now_ms: u64, lid: Option<Lid>) -> Reply {
        assert!(lease_s >= LEASE_S_MIN);
        assert!(lease_s <= LEASE_S_MAX);
        if let Some(reason) = self.refusal(holder, Ask::PowerOnly) {
            return Reply::Refused { reason };
        }
        let was_sleep_off = self.leases.holds_sleep_off(&holder);
        let held = self.leases.hold(holder, now_ms, lease_s, Ask::PowerOnly);
        assert!(held.is_ok(), "room for a new holder was checked");
        if was_sleep_off && !self.leases.sleep_off() {
            self.put_back_sleep(lid, true);
        }
        assert!(!self.leases.holds_sleep_off(&holder));
        Reply::Held {
            lease_left_s: lease_s,
        }
    }

    /// Why `holder` may not start or renew a lease that asks `ask`, if it may not: another
    /// pin, a floor for a sleep-off lease, an unreadable journal, or no room for a new holder.
    fn refusal(&self, holder: Holder, ask: Ask) -> Option<Refusal> {
        let renewing = self.leases.holds(&holder);
        if self.pin != Pin::Uid(holder.euid) {
            Some(Refusal::Unpinned)
        } else if ask.sleep_off() && self.floor.is_some() {
            Some(Refusal::Floor)
        } else if self.journal.is_none() {
            Some(Refusal::JournalUnreadable)
        } else if !renewing && self.leases.len() == HOLDERS_MAX {
            Some(Refusal::HoldersFull)
        } else {
            None
        }
    }

    /// Ends `holder`'s own lease, putting sleep back when it was the last sleep-off lease;
    /// Low Power Mode goes back at the next reading. Output: `Released`.
    fn release(&mut self, holder: Holder, lid: Option<Lid>) -> Reply {
        let was_sleep_off = self.leases.holds_sleep_off(&holder);
        let had = self.leases.release(&holder);
        assert!(had || !was_sleep_off);
        if was_sleep_off && !self.leases.sleep_off() {
            self.put_back_sleep(lid, true);
        }
        Reply::Released
    }

    /// Forgets the pin (Remove lid helper): allowed for the pinned uid, or under no pin for the
    /// non-system console user. Ends every lease and puts sleep back first. The pin stays, on
    /// disk and in memory, while the journal cannot be read (opened again here, so a journal
    /// the owner removed no longer blocks it) or a value is still not put back, so the engine
    /// keeps reporting it instead of unregistering. A pin file that cannot be removed is a
    /// third refusal: the in-memory pin is dropped only once the file is gone, and the reply
    /// is `journal_unreadable` with `Read` still saying `journal_ok`. Each value goes back in
    /// the reply with no read, and `sleepnow` runs only when no `low_power` entry was ours, so
    /// the reply stays within two runs (threat model 15.5, r9.1 F4): the lid then counts as
    /// unread, which never sleeps. Output: the reply.
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
        if self.journal.is_none() {
            self.journal = open_journal(&self.config);
        }
        if self.journal.is_none() {
            assert!(self.leases.is_empty(), "no hold starts without a journal");
            return Reply::Refused {
                reason: Refusal::JournalUnreadable,
            };
        }
        let was_sleep_off = self.leases.sleep_off();
        self.leases.clear();
        self.end_left_alone();
        let lid = if self.has_entry(Key::LowPower) { None } else { lid };
        self.put_back_sleep(lid, was_sleep_off);
        self.put_back_low_power(false);
        if self.pending() {
            return Reply::Refused {
                reason: Refusal::Pmset,
            };
        }
        let folder = journal::checked_folder(&self.config.folder, self.config.owner_uid);
        match folder.and_then(|folder| pin::forget(&folder)) {
            Ok(()) => {
                self.pin = Pin::None;
                Reply::Forgotten
            }
            Err(error) => {
                ffi::log(&format!("pin file not removed: {error:?}"));
                Reply::Refused {
                    reason: Refusal::JournalUnreadable,
                }
            }
        }
    }

    /// The state as one caller sees it, from two pmset runs: `-g` and the battery section of
    /// `-g custom` (threat model 15.2). Inputs: the caller, the time and the lid.
    fn view(&self, holder: Holder, now_ms: u64, lid: Option<Lid>) -> View {
        let values = self.pmset.values().unwrap_or_else(|error| {
            ffi::log(&format!("pmset -g unread: {error:?}"));
            Values::default()
        });
        let low_power = self.pmset.battery_low_power().unwrap_or_else(|error| {
            ffi::log(&format!("pmset -g custom unread: {error:?}"));
            None
        });
        let ours = self.journal.as_ref();
        let ours = ours.is_some_and(|journal| journal.entry(Key::SleepDisabled).is_some());
        let view = View {
            held: self.leases.sleep_off(),
            holders: u8::try_from(self.leases.len()).expect("at most HOLDERS_MAX leases"),
            lease_left_s: self.leases.left_s(now_ms),
            sleep_disabled: values.sleep_disabled,
            foreign: values.sleep_disabled == Some(true) && !ours,
            journal_ok: self.journal.is_some(),
            floor: floor_view(self.floor),
            pin: pin_view(self.pin, holder),
            lid_closed: lid.map(|lid| lid.closed),
            external_display: lid.map(|lid| lid.external_display),
            low_power,
            low_power_held: self.leases.asks_low_power(),
        };
        assert!(!view.held || view.holders > 0);
        assert!(!view.low_power_held || view.holders > 0);
        assert!(view.holders > 0 || view.lease_left_s == 0);
        view
    }

    /// Ends every sleep-off lease because the floor applies and keeps the power-only ones
    /// (threat model 15.3, r9.1 F3); the caller puts sleep back. Input: the floor. Output:
    /// whether a sleep-off lease ended.
    fn end_for_floor(&mut self, floor: Floor) -> bool {
        let was_sleep_off = self.leases.sleep_off();
        self.leases.end_sleep_off();
        if self.floor != Some(floor) {
            ffi::log(&format!("floor {floor:?}: every sleep-off lease ended"));
        }
        self.floor = Some(floor);
        assert!(!self.leases.sleep_off());
        assert!(self.floor.is_some());
        was_sleep_off
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

    /// Puts the `SleepDisabled` entry back, if one is journaled, removing it only after
    /// `pmset` succeeded, and sleeps the Mac now when a hold ended (now, or earlier with its
    /// put-back failed, so sleep is owed), sleep was put back and the lid sleeps alone. Inputs:
    /// the lid and whether a sleep-off hold ended now. Output: whether a `SleepDisabled` of
    /// ours was put back now.
    fn put_back_sleep(&mut self, lid: Option<Lid>, ended: bool) -> bool {
        assert!(!self.leases.sleep_off());
        let owed = ended || self.sleep_owed;
        let Some(journal) = self.journal.as_mut() else {
            assert!(!self.sleep_owed);
            return false;
        };
        let Some(entry) = journal.entry(Key::SleepDisabled) else {
            self.sleep_owed = false;
            return false;
        };
        if let Err(error) = self.pmset.set(entry.key, entry.before == 1) {
            ffi::log(&format!("put back failed, entry kept: {error:?}"));
            self.sleep_owed = owed && entry.before == 0;
            return false;
        }
        if let Err(error) = journal.remove(entry.key) {
            ffi::log(&format!("entry not removed: {error:?}"));
        }
        let kept = journal.entry(Key::SleepDisabled);
        self.sleep_owed = owed && kept.is_some_and(|kept| kept.before == 0);
        let sleep_back = entry.before == 0;
        let sleeps = owed && sleep_back && lid.is_some_and(Lid::sleeps_now);
        if sleeps && let Err(error) = self.pmset.sleep_now() {
            ffi::log(&format!("sleepnow failed: {error:?}"));
        }
        sleep_back
    }

    /// Puts the `low_power` entry back, if one is journaled (threat model 15.2, 15.5). With
    /// `checked`, it writes only while the battery value still reads what the helper set, and
    /// drops the entry when the owner changed it or the key is gone; without, as `Forget`
    /// does, it writes with no read. The entry is removed only after `pmset` succeeded.
    /// Input: whether to read first. Output: whether the value was written now.
    fn put_back_low_power(&mut self, checked: bool) -> bool {
        assert!(!self.leases.asks_low_power());
        let Some(journal) = self.journal.as_mut() else {
            return false;
        };
        let Some(entry) = journal.entry(Key::LowPower) else {
            return false;
        };
        let still_ours = !checked
            || match self.pmset.battery_low_power() {
                Ok(value) => value == Some(entry.before == 0),
                Err(error) => {
                    ffi::log(&format!("Low Power Mode unread, entry kept: {error:?}"));
                    return false;
                }
            };
        if still_ours && let Err(error) = self.pmset.set(entry.key, entry.before == 1) {
            ffi::log(&format!("Low Power Mode put back failed, entry kept: {error:?}"));
            return false;
        }
        if let Err(error) = journal.remove(entry.key) {
            ffi::log(&format!("entry not removed: {error:?}"));
        }
        still_ours
    }

    /// Brings Low Power Mode to the leases' asks at a reading (threat model 15.2): set while a
    /// lease asks for it, put back while none does.
    fn settle_low_power(&mut self) {
        self.end_left_alone();
        if self.leases.asks_low_power() {
            self.ensure_low_power();
        } else {
            self.put_back_low_power(true);
        }
        assert!(!self.low_power_left || self.leases.asks_low_power());
    }

    /// Makes the battery's Low Power Mode 1 for a lease that asks for it: only from 0, with
    /// the entry journaled first. A value at 1 that is not ours, a Mac without the key, or an
    /// owner who turned ours off (its entry is then dropped) is left alone until no lease asks
    /// (N1). A failed set keeps its entry: the next reading finds 1 and keeps it as ours, or 0
    /// and counts it as the owner's.
    fn ensure_low_power(&mut self) {
        assert!(self.leases.asks_low_power());
        if self.low_power_left {
            return;
        }
        let value = match self.pmset.battery_low_power() {
            Ok(value) => value,
            Err(error) => {
                ffi::log(&format!("Low Power Mode unread: {error:?}"));
                return;
            }
        };
        let journal = self.journal.as_mut().expect("no lease starts without a journal");
        let ours = journal.entry(Key::LowPower).is_some();
        match (value, ours) {
            (Some(true), true) => {}
            (Some(false), false) => {
                let entry = Entry {
                    key: Key::LowPower,
                    before: 0,
                    at_ms: wall_ms(),
                };
                if let Err(error) = journal.record(entry) {
                    ffi::log(&format!("journal not written, Low Power Mode not set: {error:?}"));
                    return;
                }
                if let Err(error) = self.pmset.set(Key::LowPower, true) {
                    ffi::log(&format!("Low Power Mode not set, entry kept: {error:?}"));
                }
            }
            (_, true) => {
                if let Err(error) = journal.remove(Key::LowPower) {
                    ffi::log(&format!("entry not removed: {error:?}"));
                }
                self.low_power_left = true;
            }
            (_, false) => self.low_power_left = true,
        }
    }

    /// Ends the time Low Power Mode is left alone once no lease asks for it, so the next ask
    /// may set it again (N1).
    fn end_left_alone(&mut self) {
        if !self.leases.asks_low_power() {
            self.low_power_left = false;
        }
        assert!(!self.low_power_left || self.leases.asks_low_power());
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
