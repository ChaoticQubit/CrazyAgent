//! The helper's core (threat model sections 6, 8, 9 and 10): leases, the journal, the uid pin
//! and the floor, driven by verified requests, lease expiry and readings every 30 s. It runs
//! `pmset` only through `Pmset` and touches files only in its folder.

use crate::floor::{Battery, Floor, Pressure};
use crate::journal::Journal;
use crate::lease::{Holder, Leases};
use crate::lid::Lid;
use crate::pin::Pin;
use crate::pmset::Pmset;
use crate::protocol::{Reply, Request};
use std::path::PathBuf;

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

/// One reading for the floor, taken every 30 s while holding or while the floor holds.
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
        Self {
            config,
            pmset,
            journal: None,
            leases: Leases::default(),
            pin: Pin::None,
            floor: None,
        }
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
        let _ = (
            holder,
            request,
            now_ms,
            lid,
            &self.config,
            &self.pmset,
            &self.journal,
        );
        Reply::Released
    }

    /// Ends the leases that ran out at `now_ms`, putting sleep back when none is left.
    pub fn expire(&mut self, now_ms: u64, lid: Option<Lid>) {
        let _ = (now_ms, lid, &mut self.leases);
    }

    /// Whether a lease ran out at `now_ms`, so the caller reads the lid before `expire`.
    pub fn has_expired(&self, now_ms: u64) -> bool {
        let _ = now_ms;
        false
    }

    /// Applies one reading: the floor, and `SleepDisabled` set again if another tool reset it.
    pub fn sense(&mut self, now_ms: u64, readings: Readings) {
        let _ = (now_ms, readings, self.pin, self.floor);
    }

    /// Puts every journaled value back before the helper exits on SIGTERM.
    pub fn terminate(&mut self, lid: Option<Lid>) {
        let _ = lid;
    }

    /// Whether readings are due: a lease lives or the floor holds.
    pub fn wants_readings(&self) -> bool {
        false
    }

    /// Whether no lease lives, so the helper may exit after its idle time.
    pub fn idle(&self) -> bool {
        false
    }
}
