use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::usage::{HOLDERS_MAX, Holder, TurnOff, Usage};

fn holder(run: &str) -> Holder {
    Holder {
        run_id: run.to_owned(),
        label: format!("run {run}"),
    }
}

fn no_stop() -> Box<dyn FnOnce() + Send> {
    Box::new(|| {})
}

/// The confirm before a plugin turns off lists every run holding it, in the order they
/// took it, and a run that ends leaves the list.
#[test]
fn the_confirm_lists_every_holder() {
    let usage = Usage::default();
    let first = usage.hold("near", holder("r1"), no_stop()).unwrap();
    let second = usage.hold("near", holder("r2"), no_stop()).unwrap();
    let _third = usage.hold("near", holder("r3"), no_stop()).unwrap();
    let _other = usage.hold("acme", holder("r9"), no_stop()).unwrap();
    assert_eq!(
        usage.users("near"),
        [holder("r1"), holder("r2"), holder("r3")]
    );
    assert_eq!(usage.users("acme"), [holder("r9")]);
    assert!(usage.users("missing").is_empty());
    drop(second);
    assert_eq!(usage.users("near"), [holder("r1"), holder("r3")]);
    drop(first);
    assert_eq!(usage.users("near"), [holder("r3")]);
}

/// A plugin holds at most `HOLDERS_MAX` runs: one more is refused with
/// `plugin_holders_capacity`, other plugins stay free, and a run ending frees a place.
#[test]
fn a_lease_past_its_bound_is_refused() {
    let usage = Usage::default();
    let mut leases: Vec<_> = (0..HOLDERS_MAX)
        .map(|index| {
            usage
                .hold("near", holder(&format!("r{index}")), no_stop())
                .unwrap()
        })
        .collect();
    assert_eq!(usage.users("near").len(), HOLDERS_MAX);
    let refused = usage
        .hold("near", holder("one-more"), no_stop())
        .expect_err("past the bound");
    assert_eq!(refused.code, "plugin_holders_capacity");
    assert_eq!(usage.users("near").len(), HOLDERS_MAX);
    let _other = usage.hold("acme", holder("r0"), no_stop()).unwrap();
    leases.pop();
    let _again = usage.hold("near", holder("one-more"), no_stop()).unwrap();
    assert_eq!(usage.users("near").len(), HOLDERS_MAX);
}

/// Stopping a plugin's runs calls each holder's stop once, a second stop calls none, and
/// the runs stay listed until their leases drop.
#[test]
fn stopping_runs_calls_each_stop_once_and_keeps_the_holders_until_they_end() {
    let usage = Usage::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let stop = || {
        let calls = Arc::clone(&calls);
        Box::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
        }) as Box<dyn FnOnce() + Send>
    };
    let first = usage.hold("near", holder("r1"), stop()).unwrap();
    let second = usage.hold("near", holder("r2"), stop()).unwrap();
    let _other = usage.hold("acme", holder("r3"), stop()).unwrap();
    assert_eq!(usage.stop_runs("near"), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(usage.stop_runs("near"), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(usage.users("near").len(), 2);
    drop((first, second));
    assert!(usage.users("near").is_empty());
    assert_eq!(usage.users("acme").len(), 1);
}

/// The owner setting `plugins.when_turned_off` defaults to refusing new calls, and its two
/// values read and write as `refuse_new_calls` and `stop_runs`.
#[test]
fn the_setting_defaults_to_refuse_new_calls() {
    assert_eq!(TurnOff::default(), TurnOff::RefuseNewCalls);
    for (mode, text) in [
        (TurnOff::RefuseNewCalls, "\"refuse_new_calls\""),
        (TurnOff::StopRuns, "\"stop_runs\""),
    ] {
        assert_eq!(serde_json::to_string(&mode).unwrap(), text);
        assert_eq!(serde_json::from_str::<TurnOff>(text).unwrap(), mode);
    }
    assert!(serde_json::from_str::<TurnOff>("\"stop\"").is_err());
}
