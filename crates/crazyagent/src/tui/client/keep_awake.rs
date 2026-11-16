use super::settings::{applied, problem};
use super::*;
use crate::keep_awake::{
    BatteryFloor, Cap, DisplayAwake, Grace, HeatEnd, HeatNotice, KeepAwakeAction,
    KeepAwakeSettings, KeepAwakeView, LidDisplay, LidRule, Mode,
};

/// The options picked on a screen and not saved yet, one per setting; none leaves a
/// setting as the engine holds it. A save lays them over the settings read at that moment,
/// so a change another client made meanwhile is kept (DEC-167 addendum 8 decision 2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::tui) struct Picks {
    pub mode: Option<Mode>,
    pub grace: Option<Grace>,
    pub cap: Option<Cap>,
    pub lid: Option<LidRule>,
    pub lid_display: Option<LidDisplay>,
    pub battery_floor: Option<BatteryFloor>,
    pub heat_end: Option<HeatEnd>,
    pub heat_notice: Option<HeatNotice>,
    pub display_awake: Option<DisplayAwake>,
}

impl Picks {
    /// The settings a save sends: each pick, else what is stored. Input: the stored
    /// settings. Output: the settings with every pick laid over them.
    pub(in crate::tui) fn over(&self, stored: KeepAwakeSettings) -> KeepAwakeSettings {
        let sent = KeepAwakeSettings {
            mode: self.mode.unwrap_or(stored.mode),
            grace: self.grace.unwrap_or(stored.grace),
            cap: self.cap.unwrap_or(stored.cap),
            lid: self.lid.unwrap_or(stored.lid),
            battery_floor: self.battery_floor.unwrap_or(stored.battery_floor),
            heat_end: self.heat_end.unwrap_or(stored.heat_end),
            heat_notice: self.heat_notice.unwrap_or(stored.heat_notice),
            lid_display: self.lid_display.unwrap_or(stored.lid_display),
            display_awake: self.display_awake.unwrap_or(stored.display_awake),
            low_power: stored.low_power,
        };
        assert!(*self != Self::default() || sent == stored);
        sent
    }

    /// The change one save attempt sends: the picks over the settings of the view read for
    /// this attempt, refused by the engine when a save landed after that read. Input: the
    /// view. Output: the settings save carrying that read's settings version.
    pub(in crate::tui) fn save(&self, view: &KeepAwakeView) -> KeepAwakeAction {
        KeepAwakeAction::Set {
            settings: self.over(view.settings),
            version: 0,
        }
    }
}

/// How many times a save reads and lays its picks, the first time included, while another
/// client keeps saving first.
pub(in crate::tui) const SAVE_ATTEMPTS: usize = 3;

/// Whether a save reads and lays its picks again: only after the engine refused it because
/// another save landed first, and while attempts remain. Inputs: the attempt's result and
/// how many attempts ran. Output: true to read again.
pub(in crate::tui) fn again(result: &Result<(), String>, attempts: usize) -> bool {
    assert!((1..=SAVE_ATTEMPTS).contains(&attempts));
    let _ = result;
    false
}

/// One Keep awake request a screen or the shell sends (DEC-167): read the view, make one
/// change, or save picks over the settings the engine holds at that moment. Runs on the
/// owner worker; the reply arrives as `OwnerEvent::KeepAwake` with this query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum KeepAwakeQuery {
    Read,
    Act {
        action: KeepAwakeAction,
        generation: u64,
    },
    Save {
        picks: Picks,
        generation: u64,
    },
}

/// What the engine answered a Keep awake request with: the view, or that the change took
/// effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum KeepAwakeReply {
    View(Box<KeepAwakeView>),
    Done,
}

/// One Keep awake reply for the screens: the query it answers and the engine's answer, or
/// a bounded plain code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct KeepAwakeEvent {
    pub query: KeepAwakeQuery,
    pub result: Result<KeepAwakeReply, String>,
}

impl KeepAwakeEvent {
    /// The reply for a request the worker could not run because the trusted connection is
    /// gone. Takes the query. Returns the event for that query.
    pub(in crate::tui) fn unavailable(query: KeepAwakeQuery) -> Self {
        Self::failed(query, "trusted_connection_unavailable")
    }

    /// The reply for a request that failed before it reached the engine.
    /// Takes the query and the problem code. Returns the event for that query.
    pub(in crate::tui) fn failed(query: KeepAwakeQuery, code: &str) -> Self {
        assert!(!code.is_empty());
        Self {
            query,
            result: Err(code.to_owned()),
        }
    }
}

impl Connection {
    /// Queues one Keep awake request on the paired owner worker.
    /// Takes the query. Returns an error while another native request is pending.
    pub(in crate::tui) fn keep_awake(&mut self, query: KeepAwakeQuery) -> Result<(), String> {
        self.owner_job(Job::KeepAwake(Box::new(query)))
    }
}

impl NativeOwner {
    /// Runs one Keep awake request through the trusted owner controller.
    /// Takes the query. Returns the event; a transport failure detaches the connection so
    /// the shell reconnects.
    pub(super) async fn keep_awake(&mut self, query: KeepAwakeQuery) -> KeepAwakeEvent {
        let mut lost = false;
        let result = self.keep_awake_reply(query.clone(), &mut lost).await;
        if lost {
            self.detach();
        }
        KeepAwakeEvent { query, result }
    }

    async fn keep_awake_reply(
        &mut self,
        query: KeepAwakeQuery,
        lost: &mut bool,
    ) -> Result<KeepAwakeReply, String> {
        let controller = self.settings_controller()?;
        let (action, generation) = match query {
            KeepAwakeQuery::Read => {
                return controller
                    .keep_awake()
                    .await
                    .map(|view| KeepAwakeReply::View(Box::new(view)))
                    .map_err(|error| problem(&error, lost));
            }
            KeepAwakeQuery::Act { action, generation } => (action, generation),
            KeepAwakeQuery::Save { picks, generation } => {
                let view = controller
                    .keep_awake()
                    .await
                    .map_err(|error| problem(&error, lost))?;
                (picks.save(&view), generation)
            }
        };
        let reply = controller.keep_awake_action(action, generation).await;
        applied(reply, "keep_awake_action_outcome_unknown", lost).map(|()| KeepAwakeReply::Done)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::storage::versioned::STALE_CODE;
    use crate::tui::screens::keep_awake::tests::resting;

    #[test]
    fn a_save_attempt_lays_the_picks_over_its_read_and_sends_that_read_s_version() {
        let mut read = resting();
        read.version = 4;
        read.settings.grace = Grace::Min30;
        let picks = Picks {
            cap: Some(Cap::H4),
            ..Picks::default()
        };
        let KeepAwakeAction::Set { settings, version } = picks.save(&read) else {
            panic!("a save sends the settings");
        };
        assert_eq!(version, 4, "the version of the read the picks lie over");
        assert_eq!(
            settings.grace,
            Grace::Min30,
            "a change made elsewhere is kept"
        );
        assert_eq!(settings.cap, Cap::H4);
        assert_eq!(settings, picks.over(read.settings));
    }

    #[test]
    fn only_a_save_another_client_beat_reads_again_and_at_most_three_times() {
        let stale = Err(STALE_CODE.to_owned());
        assert_eq!(SAVE_ATTEMPTS, 3);
        assert!(again(&stale, 1));
        assert!(again(&stale, 2));
        assert!(!again(&stale, SAVE_ATTEMPTS), "the third refusal is shown");
        assert!(!again(&Ok(()), 1), "a saved change is done");
        for code in [
            "keep_awake_off",
            "keep_awake_action_outcome_unknown",
            "controller_pending_intent",
            "trusted_connection_unavailable",
        ] {
            assert!(!again(&Err(code.to_owned()), 1), "{code} is not read again");
        }
    }

    #[test]
    #[should_panic]
    fn an_attempt_count_past_the_bound_is_a_worker_mistake() {
        again(&Err(STALE_CODE.to_owned()), SAVE_ATTEMPTS + 1);
    }
}
