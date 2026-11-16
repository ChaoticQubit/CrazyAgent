use crate::keep_awake::journal::{Entry, Journal, JournalLock};
use crate::keep_awake::{LidSupport, RESTORE_ENGINE_RUNNING, restore};
use crate::keep_awake::lid_windows::{
    Actions, KIND, LidSetting, NEEDS_ADMIN, NOT_CHANGED, Refusal, WindowsLid, undo_with,
};
use crate::keep_awake::power::{LidAsk, LidCommand};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

const PLAN: &str = "381b4222-f694-41f0-9685-ff5bb260df2e";
const OTHER: &str = "a1841308-3541-4fab-bc81-f71556f20b4a";
/// A lid ask with a rule Off and nothing wanted.
fn off() -> LidAsk {
    LidAsk::default()
}

/// A lid ask under a lid rule that does not want the lid now.
fn resting() -> LidAsk {
    LidAsk {
        rule_on: true,
        ..LidAsk::default()
    }
}

/// A lid ask that wants the closed lid held.
fn wanted() -> LidAsk {
    LidAsk {
        rule_on: true,
        wanted: true,
        ..LidAsk::default()
    }
}

/// What the stand-in power settings hold and what they saw: each write with the journal
/// entries on disk at that moment, and each sleep, so no row changes this computer.
#[derive(Debug)]
struct Plans {
    root: PathBuf,
    active: String,
    actions: BTreeMap<String, Actions>,
    check: Result<(), Refusal>,
    refusing: Option<Refusal>,
    closed: Option<bool>,
    on_ac: Option<bool>,
    writes: Vec<(String, Actions, Vec<Entry>)>,
    slept: Vec<bool>,
    applied: Vec<String>,
    unapplied: Option<Refusal>,
}

#[derive(Debug, Clone)]
struct StandIn(Arc<Mutex<Plans>>);

impl StandIn {
    fn plans(&self) -> MutexGuard<'_, Plans> {
        self.0.lock().unwrap()
    }

    fn now(&self) -> Actions {
        self.plans().actions[PLAN]
    }
}

impl LidSetting for StandIn {
    fn active(&mut self) -> Result<String, Refusal> {
        Ok(self.plans().active.clone())
    }

    fn writable(&mut self) -> Result<(), Refusal> {
        self.plans().check.clone()
    }

    fn read(&mut self, plan: &str) -> Result<Actions, Refusal> {
        self.plans().actions.get(plan).copied().ok_or(Refusal::Gone)
    }

    fn write(&mut self, plan: &str, actions: Actions) -> Result<(), Refusal> {
        let mut plans = self.plans();
        let on_disk = Journal::open(&plans.root)
            .map(|journal| journal.entries().to_vec())
            .unwrap_or_default();
        plans.writes.push((plan.to_owned(), actions, on_disk));
        if let Some(refusal) = plans.refusing.clone() {
            return Err(refusal);
        }
        let Some(found) = plans.actions.get_mut(plan) else {
            return Err(Refusal::Gone);
        };
        *found = actions;
        Ok(())
    }

    fn apply(&mut self, plan: &str) -> Result<(), Refusal> {
        let mut plans = self.plans();
        if let Some(refusal) = plans.unapplied.clone() {
            return Err(refusal);
        }
        if plans.active == plan {
            plans.applied.push(plan.to_owned());
        }
        Ok(())
    }

    fn closed(&mut self) -> Option<bool> {
        self.plans().closed
    }

    fn on_ac(&mut self) -> Option<bool> {
        self.plans().on_ac
    }

    fn suspend(&mut self, hibernate: bool) -> Result<(), Refusal> {
        self.plans().slept.push(hibernate);
        Ok(())
    }
}

fn line(ac: u32, dc: u32) -> (tempfile::TempDir, StandIn, WindowsLid<StandIn>) {
    let root = tempfile::tempdir().unwrap();
    let plans = Plans {
        root: root.path().to_path_buf(),
        active: PLAN.into(),
        actions: BTreeMap::from([(PLAN.to_owned(), Actions { ac, dc })]),
        check: Ok(()),
        refusing: None,
        closed: Some(false),
        on_ac: Some(true),
        writes: Vec::new(),
        slept: Vec::new(),
        applied: Vec::new(),
        unapplied: None,
    };
    let setting = StandIn(Arc::new(Mutex::new(plans)));
    let line = WindowsLid::new(setting.clone(), root.path().to_path_buf());
    (root, setting, line)
}

fn entries(root: &Path) -> Vec<Entry> {
    Journal::open(root).unwrap().entries().to_vec()
}

fn saved(ac: u32, dc: u32) -> Entry {
    saved_in(PLAN, ac, dc)
}

fn saved_in(plan: &str, ac: u32, dc: u32) -> Entry {
    Entry {
        kind: KIND.into(),
        restore: format!(r#"{{"scheme":"{plan}","ac":{ac},"dc":{dc}}}"#),
    }
}

const NOTHING: Actions = Actions { ac: 0, dc: 0 };

/// Plan Task 14, stand-in half of the smallest real check: the old values are on disk
/// before Do nothing is written, the lid is held once, and release puts both back.
#[test]
fn the_old_lid_action_is_journaled_before_the_write_and_put_back_at_release() {
    let (root, setting, mut line) = line(1, 2);
    assert_eq!(
        line.step(resting(), None),
        LidSupport::Held,
        "the lid can be held"
    );
    assert!(
        setting.plans().writes.is_empty(),
        "nothing is changed before it is wanted"
    );
    assert_eq!(line.step(wanted(), None), LidSupport::Held);
    let writes = setting.plans().writes.clone();
    assert_eq!(writes, [(PLAN.to_owned(), NOTHING, vec![saved(1, 2)])]);
    assert_eq!(setting.now(), NOTHING);
    assert_eq!(line.step(wanted(), None), LidSupport::Held);
    assert_eq!(
        setting.plans().writes.len(),
        1,
        "a held lid is written once"
    );
    assert_eq!(line.step(resting(), None), LidSupport::Held);
    assert_eq!(setting.now(), Actions { ac: 1, dc: 2 });
    assert!(
        entries(root.path()).is_empty(),
        "the entry is forgotten once put back"
    );
    assert!(setting.plans().slept.is_empty(), "the lid was open");
}

#[test]
fn release_restores_both_ac_and_dc_values() {
    for (ac, dc) in [(2, 3), (1, 0), (0, 1), (3, 3)] {
        let (root, setting, mut line) = line(ac, dc);
        assert_eq!(line.step(wanted(), None), LidSupport::Held, "{ac} {dc}");
        assert_eq!(setting.now(), NOTHING, "{ac} {dc}");
        assert_eq!(entries(root.path()), [saved(ac, dc)], "{ac} {dc}");
        assert_eq!(
            line.step(off(), None),
            LidSupport::Held,
            "a rule turned Off releases"
        );
        assert_eq!(setting.now(), Actions { ac, dc }, "{ac} {dc}");
        assert!(entries(root.path()).is_empty(), "{ac} {dc}");
    }
}

#[test]
fn a_policy_locked_lid_action_reports_locked_by_policy() {
    let (root, setting, mut line) = line(1, 1);
    setting.plans().check = Err(Refusal::Policy);
    assert_eq!(line.step(resting(), None), LidSupport::LockedByPolicy);
    assert_eq!(line.step(wanted(), None), LidSupport::LockedByPolicy);
    assert!(
        setting.plans().writes.is_empty(),
        "a locked lid action is never written"
    );
    assert!(entries(root.path()).is_empty());
    setting.plans().check = Err(Refusal::Denied);
    let needs_admin = LidSupport::NotAvailable {
        reason: NEEDS_ADMIN.into(),
    };
    assert_eq!(line.step(resting(), None), needs_admin);
    assert_eq!(
        line.step(wanted(), None),
        needs_admin,
        "never asks to elevate"
    );
    assert!(setting.plans().writes.is_empty());
    assert_eq!(setting.now(), Actions { ac: 1, dc: 1 });
}

#[test]
fn a_lid_action_already_do_nothing_is_held_with_nothing_recorded() {
    let (root, setting, mut line) = line(0, 0);
    assert_eq!(line.step(wanted(), None), LidSupport::Held);
    assert!(setting.plans().writes.is_empty());
    assert!(entries(root.path()).is_empty());
    setting.plans().closed = Some(true);
    assert_eq!(line.step(resting(), None), LidSupport::Held);
    assert!(setting.plans().writes.is_empty());
    assert!(
        setting.plans().slept.is_empty(),
        "Do nothing stays awake at the end"
    );
}

#[test]
fn a_change_that_cannot_be_recorded_is_never_made() {
    let (folder, setting, _) = line(1, 2);
    let blocked = folder.path().join("not-a-folder");
    std::fs::write(&blocked, "a file where the engine root should be").unwrap();
    let mut line = WindowsLid::new(setting.clone(), blocked);
    assert_eq!(line.step(wanted(), None), LidSupport::SleepNotRestored);
    assert!(
        setting.plans().writes.is_empty(),
        "no write without a journal"
    );
    assert_eq!(setting.now(), Actions { ac: 1, dc: 2 });
}

#[test]
fn a_write_that_fails_puts_back_what_was_written_and_holds_no_lid() {
    let (root, setting, mut line) = line(1, 2);
    setting.plans().refusing = Some(Refusal::Failed("the power service is busy".into()));
    let refused = LidSupport::NotAvailable {
        reason: format!("{NOT_CHANGED}the power service is busy"),
    };
    assert_eq!(line.step(wanted(), None), refused);
    assert_eq!(setting.now(), Actions { ac: 1, dc: 2 });
    assert!(
        entries(root.path()).is_empty(),
        "nothing was changed, nothing is left"
    );
    assert_eq!(
        setting.plans().writes.len(),
        1,
        "put back finds nothing to write"
    );
}

#[test]
fn a_hold_that_ends_with_the_lid_closed_does_what_the_lid_action_says() {
    let cases: [(u32, u32, Option<bool>, Option<bool>, &[bool]); 8] = [
        (1, 1, Some(true), Some(true), &[false]),
        (2, 1, Some(true), Some(true), &[true]),
        (3, 1, Some(true), Some(true), &[false]),
        (1, 2, Some(false), Some(true), &[true]),
        (0, 1, Some(true), Some(true), &[]),
        (1, 1, Some(true), Some(false), &[]),
        (1, 1, Some(true), None, &[]),
        (1, 2, None, Some(true), &[false]),
    ];
    for (ac, dc, on_ac, closed, slept) in cases {
        let (_root, setting, mut line) = line(ac, dc);
        setting.plans().on_ac = on_ac;
        assert_eq!(line.step(wanted(), None), LidSupport::Held);
        setting.plans().closed = closed;
        assert_eq!(line.step(resting(), None), LidSupport::Held);
        let case = format!("{ac} {dc} {on_ac:?} {closed:?}");
        assert_eq!(setting.plans().slept, slept, "{case}");
        assert_eq!(setting.now(), Actions { ac, dc }, "{case}");
    }
}

#[test]
fn a_value_the_owner_changed_during_the_hold_stays() {
    let (root, setting, mut line) = line(1, 2);
    assert_eq!(line.step(wanted(), None), LidSupport::Held);
    let changed = Actions { ac: 3, dc: 0 };
    setting.plans().actions.insert(PLAN.into(), changed);
    assert_eq!(line.step(resting(), None), LidSupport::Held);
    assert_eq!(setting.now(), Actions { ac: 3, dc: 2 });
    assert!(entries(root.path()).is_empty());
}

#[test]
fn a_failed_put_back_is_tried_again_and_reads_sleep_not_restored_until_it_works() {
    let (root, setting, mut line) = line(1, 2);
    assert_eq!(line.step(wanted(), None), LidSupport::Held);
    setting.plans().refusing = Some(Refusal::Failed("the power service is busy".into()));
    assert_eq!(line.step(resting(), None), LidSupport::SleepNotRestored);
    assert_eq!(entries(root.path()), [saved(1, 2)], "the entry stays");
    let writes = setting.plans().writes.len();
    assert_eq!(line.step(wanted(), None), LidSupport::SleepNotRestored);
    assert_eq!(
        setting.plans().writes.len(),
        writes,
        "no new hold over a change left"
    );
    assert_eq!(entries(root.path()), [saved(1, 2)], "and no second entry");
    assert_eq!(
        line.step(resting(), None),
        LidSupport::SleepNotRestored,
        "tried again"
    );
    assert_eq!(setting.plans().writes.len(), writes + 1);
    setting.plans().refusing = None;
    assert_eq!(line.step(resting(), None), LidSupport::Held);
    assert_eq!(setting.now(), Actions { ac: 1, dc: 2 });
    assert!(entries(root.path()).is_empty());
}

#[test]
fn put_the_lid_setting_back_now_waits_for_the_hold_and_runs_at_once_after_it() {
    let (root, setting, mut line) = line(1, 2);
    assert_eq!(line.step(wanted(), None), LidSupport::Held);
    let put_back = Some(LidCommand::PutBack);
    assert_eq!(
        line.step(wanted(), put_back),
        LidSupport::Held,
        "a held lid stays held"
    );
    assert_eq!(setting.now(), NOTHING);
    setting.plans().refusing = Some(Refusal::Failed("the power service is busy".into()));
    assert_eq!(line.step(resting(), None), LidSupport::SleepNotRestored);
    setting.plans().refusing = None;
    assert_eq!(line.step(resting(), put_back), LidSupport::Held);
    assert_eq!(setting.now(), Actions { ac: 1, dc: 2 });
    assert!(entries(root.path()).is_empty());
}

#[test]
fn a_plan_that_is_gone_counts_as_put_back() {
    let (root, setting, mut line) = line(1, 2);
    assert_eq!(line.step(wanted(), None), LidSupport::Held);
    setting.plans().actions.clear();
    assert_eq!(line.step(resting(), None), LidSupport::Held);
    assert!(
        entries(root.path()).is_empty(),
        "nothing is left to put back"
    );
}

#[test]
fn an_entry_an_earlier_engine_left_is_put_back_by_its_undo() {
    let (_root, mut setting, _) = line(0, 0);
    undo_with(&mut setting, &saved(1, 2)).unwrap();
    assert_eq!(setting.now(), Actions { ac: 1, dc: 2 });
    undo_with(&mut setting, &saved(3, 3)).unwrap();
    assert_eq!(
        setting.now(),
        Actions { ac: 1, dc: 2 },
        "a value not at Do nothing stays"
    );
    let invalid = [
        "not json".to_owned(),
        format!(r#"{{"scheme":"{PLAN}","ac":4,"dc":0}}"#),
        format!(r#"{{"scheme":"{PLAN}","ac":0,"dc":0,"more":1}}"#),
        r#"{"scheme":"","ac":1,"dc":1}"#.to_owned(),
    ];
    for restore in invalid {
        let entry = Entry {
            kind: KIND.into(),
            restore: restore.clone(),
        };
        let refused = undo_with(&mut setting, &entry).unwrap_err();
        assert!(
            refused.0.starts_with("keep_awake_lid_entry_invalid"),
            "{restore}"
        );
    }
    setting.plans().actions.clear();
    assert!(
        undo_with(&mut setting, &saved(1, 2)).is_ok(),
        "a plan that is gone is done"
    );
    assert_eq!(
        setting.plans().writes.len(),
        1,
        "only the first entry wrote"
    );
}

/// rv-ds241 KA T14 r1 F1, QUESTION 01:16 Q1: a plan switched during a hold puts the held
/// plan back and holds the new one in the same ask, so the lid stays held; release puts
/// each plan back.
#[test]
fn a_plan_switched_during_a_hold_is_put_back_and_the_new_plan_held() {
    let (root, setting, mut line) = line(1, 2);
    assert_eq!(line.step(wanted(), None), LidSupport::Held);
    {
        let mut plans = setting.plans();
        plans.actions.insert(OTHER.into(), Actions { ac: 2, dc: 3 });
        plans.active = OTHER.into();
    }
    assert_eq!(line.step(wanted(), None), LidSupport::Held, "still held");
    assert_eq!(
        setting.now(),
        Actions { ac: 1, dc: 2 },
        "the old plan is put back"
    );
    assert_eq!(
        setting.plans().actions[OTHER],
        NOTHING,
        "the new plan is held"
    );
    assert_eq!(entries(root.path()), [saved_in(OTHER, 2, 3)]);
    assert_eq!(line.step(wanted(), None), LidSupport::Held);
    assert_eq!(
        setting.plans().writes.len(),
        3,
        "a held plan is not written again"
    );
    assert_eq!(line.step(resting(), None), LidSupport::Held);
    assert_eq!(setting.plans().actions[OTHER], Actions { ac: 2, dc: 3 });
    assert!(entries(root.path()).is_empty());
}

/// rv-ds241 KA T14 r1 F1, QUESTION 01:16 Q1: a lid value set again during a hold, by the
/// owner or a policy refresh, is put back as it is now and held again in the same ask, so
/// the lid stays held and that value is what goes back at the end (QUESTION 22:58 Q5).
#[test]
fn a_lid_value_set_again_during_a_hold_is_held_again_and_goes_back_at_the_end() {
    let (root, setting, mut line) = line(1, 2);
    assert_eq!(line.step(wanted(), None), LidSupport::Held);
    let changed = Actions { ac: 3, dc: 0 };
    setting.plans().actions.insert(PLAN.into(), changed);
    assert_eq!(line.step(wanted(), None), LidSupport::Held, "still held");
    assert_eq!(setting.now(), NOTHING);
    assert_eq!(entries(root.path()), [saved(3, 2)]);
    assert_eq!(line.step(resting(), None), LidSupport::Held);
    assert_eq!(setting.now(), Actions { ac: 3, dc: 2 });
    assert!(entries(root.path()).is_empty());
    assert!(setting.plans().slept.is_empty(), "the lid was open");
}

/// rv-ds241 KA T14 r1 F2, rv-sec3 F3, QUESTION 01:16 Q2: a hold that ends with the lid
/// closed owes its sleep until the put back works, then sleeps once.
#[test]
fn a_hold_that_ends_closed_sleeps_once_its_put_back_works() {
    let (_root, setting, mut line) = line(1, 2);
    assert_eq!(line.step(wanted(), None), LidSupport::Held);
    setting.plans().closed = Some(true);
    setting.plans().refusing = Some(Refusal::Failed("the power service is busy".into()));
    assert_eq!(line.step(resting(), None), LidSupport::SleepNotRestored);
    assert!(
        setting.plans().slept.is_empty(),
        "nothing sleeps over Do nothing"
    );
    setting.plans().refusing = None;
    assert_eq!(line.step(resting(), None), LidSupport::Held);
    assert_eq!(setting.plans().slept, [false], "slept once put back");
    assert_eq!(line.step(resting(), None), LidSupport::Held);
    assert_eq!(setting.plans().slept, [false], "and only once");
}

/// QUESTION 01:16 Q2: a hold whose last answer was not Held owes no sleep when it ends with
/// the lid closed, since the keeper had stopped holding the lid by then.
#[test]
fn a_hold_whose_last_answer_was_not_held_owes_no_sleep() {
    let (root, setting, mut line) = line(1, 2);
    assert_eq!(line.step(wanted(), None), LidSupport::Held);
    setting.plans().check = Err(Refusal::Denied);
    let needs_admin = LidSupport::NotAvailable {
        reason: NEEDS_ADMIN.into(),
    };
    assert_eq!(line.step(wanted(), None), needs_admin);
    setting.plans().check = Ok(());
    setting.plans().closed = Some(true);
    assert_eq!(line.step(resting(), None), LidSupport::Held);
    assert_eq!(setting.now(), Actions { ac: 1, dc: 2 });
    assert!(entries(root.path()).is_empty());
    assert!(setting.plans().slept.is_empty(), "no sleep is owed");
}

/// rv-sec3 KA T14 r1 F3, QUESTION 01:16 Q2: a lid line built while an earlier engine's lid
/// change is still journaled owes a sleep; once the engine start's undo put it back, the
/// first ask sleeps with the lid closed, and with the lid open nothing is owed.
#[test]
fn an_engine_start_over_a_lid_change_left_sleeps_once_it_is_put_back() {
    for (closed, slept) in [(Some(true), &[false][..]), (Some(false), &[][..])] {
        let (root, setting, _) = line(0, 0);
        Journal::open(root.path())
            .unwrap()
            .record(saved(1, 2))
            .unwrap();
        setting.plans().closed = closed;
        let mut started = WindowsLid::new(setting.clone(), root.path().to_path_buf());
        let mut undo = setting.clone();
        Journal::open(root.path())
            .unwrap()
            .reconcile(|entry| undo_with(&mut undo, entry))
            .unwrap();
        assert_eq!(setting.now(), Actions { ac: 1, dc: 2 }, "{closed:?}");
        assert_eq!(
            started.step(resting(), None),
            LidSupport::Held,
            "{closed:?}"
        );
        assert_eq!(setting.plans().slept, slept, "{closed:?}");
        assert_eq!(
            started.step(resting(), None),
            LidSupport::Held,
            "{closed:?}"
        );
        assert_eq!(setting.plans().slept, slept, "paid once, {closed:?}");
    }
}

/// rv-sec3 KA T14 r1 F4: a put back that finds the saved values already stored applies
/// them again when its plan is the active one, since Windows applies a stored change to
/// the active plan only then; a failing apply keeps the entry.
#[test]
fn a_put_back_already_stored_is_applied_again_on_the_active_plan() {
    let (_root, mut setting, _) = line(1, 2);
    undo_with(&mut setting, &saved(1, 2)).unwrap();
    assert_eq!(setting.plans().applied, [PLAN], "applied again");
    assert!(setting.plans().writes.is_empty(), "nothing to write");
    setting.plans().active = OTHER.into();
    undo_with(&mut setting, &saved(1, 2)).unwrap();
    assert_eq!(
        setting.plans().applied,
        [PLAN],
        "a plan not active is not applied"
    );
    setting.plans().active = PLAN.into();
    setting.plans().unapplied = Some(Refusal::Failed("the power service is busy".into()));
    let kept = undo_with(&mut setting, &saved(1, 2)).unwrap_err();
    assert!(
        kept.0.starts_with("keep_awake_lid_not_put_back"),
        "{kept:?}"
    );
}

/// rv-ds241 KA T14 r1 nit 5: a hold whose write and put back both fail leaves its entry
/// and says SleepNotRestored, as every other ask with a change left does.
#[test]
fn a_hold_whose_write_and_put_back_both_fail_reads_sleep_not_restored() {
    let (root, setting, mut line) = line(1, 2);
    setting.plans().refusing = Some(Refusal::Failed("the power service is busy".into()));
    setting.plans().unapplied = Some(Refusal::Failed("the power service is busy".into()));
    assert_eq!(line.step(wanted(), None), LidSupport::SleepNotRestored);
    assert_eq!(entries(root.path()), [saved(1, 2)], "the entry stays");
}

/// rv-ds241 KA T14 r2 N1: the lid line keeps the journal lock it is handed until it is
/// dropped with the sensor thread, so a restore cannot write the journal before then.
#[test]
fn the_lid_line_keeps_the_journal_lock_until_it_is_dropped() {
    let (root, _setting, line) = line(1, 2);
    let lock = JournalLock::try_take(root.path()).unwrap().unwrap();
    let line = line.with_lock(lock);
    assert!(JournalLock::try_take(root.path()).unwrap().is_none());
    assert_eq!(restore(root.path()), Err(RESTORE_ENGINE_RUNNING));
    drop(line);
    assert!(JournalLock::try_take(root.path()).unwrap().is_some());
}

#[cfg(windows)]
mod on_windows {
    use super::*;
    use crate::keep_awake::Leftover;
    use crate::keep_awake::lid_windows::Windows;
    use crazyagent_win_sandbox::lid;

    /// Deletes the scratch power plan a row made when the row ends, however it ends.
    struct Deleted(lid::Scheme);

    impl Drop for Deleted {
        fn drop(&mut self) {
            if let Err(problem) = lid::delete(self.0) {
                eprintln!("the scratch power plan was not deleted: {problem}");
            }
        }
    }

    /// Windows' own lid setting with a scratch plan standing in as the active one, so a row
    /// writes only that plan and never sleeps this computer.
    struct Scratch(lid::Scheme);

    impl LidSetting for Scratch {
        fn active(&mut self) -> Result<String, Refusal> {
            Ok(self.0.text())
        }

        fn writable(&mut self) -> Result<(), Refusal> {
            Windows.writable()
        }

        fn read(&mut self, plan: &str) -> Result<Actions, Refusal> {
            assert_eq!(plan, self.0.text(), "rows read only the scratch plan");
            Windows.read(plan)
        }

        fn write(&mut self, plan: &str, actions: Actions) -> Result<(), Refusal> {
            assert_eq!(plan, self.0.text(), "rows write only the scratch plan");
            Windows.write(plan, actions)
        }

        fn apply(&mut self, plan: &str) -> Result<(), Refusal> {
            assert_eq!(plan, self.0.text(), "rows apply only the scratch plan");
            Windows.apply(plan)
        }

        fn closed(&mut self) -> Option<bool> {
            Some(false)
        }

        fn on_ac(&mut self) -> Option<bool> {
            Some(true)
        }

        fn suspend(&mut self, _hibernate: bool) -> Result<(), Refusal> {
            panic!("rows never put this computer to sleep");
        }
    }

    /// Plan Task 14's smallest real check, on the Windows probe machine: a scratch copy of
    /// the active plan, so the owner's plan is never written; the line is dropped while it
    /// holds, as a crashed engine is, and the next engine start puts the old value back.
    #[test]
    fn the_old_lid_action_is_journaled_before_the_write_and_restored_at_engine_start() {
        let owner = lid::active().unwrap();
        let owner_actions = lid::read(owner).unwrap();
        let plan = lid::duplicate(owner).unwrap();
        let _deleted = Deleted(plan);
        assert_ne!(plan, owner, "the copy is a new plan");
        lid::write(plan, lid::Actions { ac: 1, dc: 2 }).unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut line = WindowsLid::new(Scratch(plan), root.path().to_path_buf());
        let held = line.step(wanted(), None);
        assert_eq!(
            held,
            LidSupport::Held,
            "K0-7: this account may change the lid action"
        );
        assert_eq!(lid::read(plan).unwrap(), lid::Actions { ac: 0, dc: 0 });
        let expected = format!(r#"{{"scheme":"{}","ac":1,"dc":2}}"#, plan.text());
        let recorded = entries(root.path());
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].kind, KIND);
        assert_eq!(recorded[0].restore, expected);
        drop(line);
        let left = crate::keep_awake::reconcile(root.path());
        assert_eq!(left, Leftover::Kinds(Vec::new()));
        assert_eq!(lid::read(plan).unwrap(), lid::Actions { ac: 1, dc: 2 });
        assert_eq!(
            lid::active().unwrap(),
            owner,
            "the owner's plan stays active"
        );
        assert_eq!(
            lid::read(owner).unwrap(),
            owner_actions,
            "and keeps its lid values"
        );
    }
}
