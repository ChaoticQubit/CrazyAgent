use std::cell::{Cell, RefCell};

use sdk::settings::{self, LeftRunning, OnQuit, Provide, QuitSettings};
use sdk::{Capability, Refusal, Tier};

use super::*;

/// The settings provider fake: counts calls, keeps the last settings, answers as told.
struct CountingProvider {
    calls: Cell<u32>,
    last: Cell<Option<QuitSettings>>,
    answer: Result<(), Refusal>,
}

impl CountingProvider {
    /// A provider that answers every call with one result. Input: the result.
    /// Output: the provider, no call seen.
    fn answering(answer: Result<(), Refusal>) -> Self {
        Self {
            calls: Cell::new(0),
            last: Cell::new(None),
            answer,
        }
    }
}

impl Provide for CountingProvider {
    fn save_quit(&self, settings: QuitSettings) -> Result<(), Refusal> {
        self.calls.set(self.calls.get() + 1);
        self.last.set(Some(settings));
        self.answer.clone()
    }
}

/// The refusal sink fake: keeps every report in order.
#[derive(Default)]
struct RecordingRefusals {
    seen: RefCell<Vec<(&'static str, &'static str, Refusal)>>,
}

impl Refusals for RecordingRefusals {
    fn refused(&self, plugin: &'static str, capability: &Capability, refusal: &Refusal) {
        self.seen
            .borrow_mut()
            .push((plugin, capability.name, refusal.clone()));
    }
}

/// The settings every row saves.
fn quit() -> QuitSettings {
    QuitSettings {
        on_quit: OnQuit::Stop,
        left_running: LeftRunning::Never,
        hours: 12,
    }
}

/// The grant table of the rows: `agent-quit` (t2, settings.write), `reader` (t2, no
/// write) and `low` (t1, test.use).
fn grants() -> Grants {
    Grants::new(&[
        ("agent-quit", Tier::T2, &["settings.write"]),
        ("reader", Tier::T2, &["settings.read"]),
        ("low", Tier::T1, &["test.use"]),
    ])
    .expect("the table builds")
}

#[test]
fn a_granted_call_reaches_the_provider_once() {
    let (grants, provider, refusals) = (grants(), CountingProvider::answering(Ok(())), RecordingRefusals::default());
    let gateway = Gateway::new(&grants, &provider, &refusals);
    assert_eq!(settings::save_quit(&gateway.bind("agent-quit"), quit()), Ok(()));
    assert_eq!(provider.calls.get(), 1);
    assert_eq!(provider.last.get(), Some(quit()));
    assert!(refusals.seen.borrow().is_empty());
}

#[test]
fn an_ungranted_call_is_refused_logged_once_and_never_reaches_the_provider() {
    let (grants, provider, refusals) = (grants(), CountingProvider::answering(Ok(())), RecordingRefusals::default());
    let gateway = Gateway::new(&grants, &provider, &refusals);
    let refusal = Refusal::NotGranted {
        capability: "settings.save_quit",
        needs: "settings.write",
    };
    assert_eq!(
        settings::save_quit(&gateway.bind("reader"), quit()),
        Err(refusal.clone())
    );
    assert_eq!(provider.calls.get(), 0);
    assert_eq!(
        *refusals.seen.borrow(),
        vec![("reader", "settings.save_quit", refusal)]
    );
}

#[test]
fn an_unknown_plugin_is_refused() {
    let (grants, provider, refusals) = (grants(), CountingProvider::answering(Ok(())), RecordingRefusals::default());
    let gateway = Gateway::new(&grants, &provider, &refusals);
    let refusal = Refusal::NotGranted {
        capability: "settings.save_quit",
        needs: "settings.write",
    };
    assert_eq!(
        settings::save_quit(&gateway.bind("stranger"), quit()),
        Err(refusal.clone())
    );
    assert_eq!(provider.calls.get(), 0);
    assert_eq!(
        *refusals.seen.borrow(),
        vec![("stranger", "settings.save_quit", refusal)]
    );
}

#[test]
fn a_t1_plugin_may_not_call_a_t2_capability() {
    const T2_ONLY: Capability = Capability {
        name: "test.t2_only",
        tier: Tier::T2,
        needs: "test.use",
    };
    const T1_SAME: Capability = Capability {
        name: "test.t1_same",
        tier: Tier::T1,
        needs: "test.use",
    };
    let (grants, provider, refusals) = (grants(), CountingProvider::answering(Ok(())), RecordingRefusals::default());
    let gateway = Gateway::new(&grants, &provider, &refusals);
    let refusal = Refusal::TierTooHigh {
        capability: "test.t2_only",
    };
    assert_eq!(gateway.admit("low", &T2_ONLY), Err(refusal.clone()));
    assert_eq!(gateway.admit("low", &T1_SAME), Ok(()));
    let ran = Cell::new(false);
    assert_eq!(
        gateway.call("low", &T2_ONLY, || {
            ran.set(true);
            Ok(())
        }),
        Err(refusal.clone())
    );
    assert!(!ran.get());
    assert_eq!(
        *refusals.seen.borrow(),
        vec![("low", "test.t2_only", refusal)]
    );
}

#[test]
fn the_provider_rejection_comes_back_unchanged() {
    let rejection = Refusal::Rejected {
        code: "quit_settings_invalid".to_owned(),
    };
    let (grants, provider, refusals) = (grants(), CountingProvider::answering(Err(rejection.clone())), RecordingRefusals::default());
    let gateway = Gateway::new(&grants, &provider, &refusals);
    assert_eq!(
        settings::save_quit(&gateway.bind("agent-quit"), quit()),
        Err(rejection)
    );
    assert_eq!(provider.calls.get(), 1);
    assert!(refusals.seen.borrow().is_empty());
}

#[test]
fn grants_refuse_a_duplicate_plugin() {
    assert_eq!(
        Grants::new(&[("a", Tier::T2, &[]), ("a", Tier::T1, &[])]),
        Err("grants_duplicate_plugin")
    );
}

#[test]
fn grants_refuse_a_duplicate_permission_and_a_kernel_tier() {
    assert_eq!(
        Grants::new(&[("a", Tier::T2, &["x.y", "x.y"])]),
        Err("grants_duplicate_permission")
    );
    assert_eq!(
        Grants::new(&[("a", Tier::Kernel, &[])]),
        Err("grants_kernel_tier")
    );
}

#[test]
fn grants_refuse_more_than_their_bounds() {
    let names: Vec<&'static str> = (0..=PLUGINS_MAX)
        .map(|index| &*Box::leak(format!("p{index}").into_boxed_str()))
        .collect();
    let entries: Vec<(&'static str, Tier, &[&'static str])> =
        names.iter().map(|name| (*name, Tier::T2, &[][..])).collect();
    assert_eq!(Grants::new(&entries), Err("grants_too_many"));
    assert!(Grants::new(&entries[..PLUGINS_MAX]).is_ok());
    let permissions: Vec<&'static str> = (0..=PERMISSIONS_MAX)
        .map(|index| &*Box::leak(format!("p.n{index}").into_boxed_str()))
        .collect();
    assert_eq!(
        Grants::new(&[("a", Tier::T2, &permissions)]),
        Err("grants_too_many")
    );
    assert!(Grants::new(&[("a", Tier::T2, &permissions[..PERMISSIONS_MAX])]).is_ok());
}
