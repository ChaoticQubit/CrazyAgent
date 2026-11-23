use std::cell::{Cell, RefCell};

use bus_gateway::{Gateway, Grants, Refusals};
use sdk::secrets::{self, Secret};
use sdk::settings::{self, LeftRunning, OnQuit, Provide, QuitSettings};
use sdk::{Capability, Refusal, TEXT_BYTES_MAX, Tier};

use super::*;

/// The settings provider fake: counts calls, keeps the last caller and settings, answers
/// as told.
struct CountingProvider {
    calls: Cell<u32>,
    caller: Cell<Option<&'static str>>,
    last: Cell<Option<QuitSettings>>,
    answer: Result<(), Refusal>,
}

impl CountingProvider {
    /// A provider that answers every call with one result. Input: the result.
    /// Output: the provider, no call seen.
    fn answering(answer: Result<(), Refusal>) -> Self {
        Self {
            calls: Cell::new(0),
            caller: Cell::new(None),
            last: Cell::new(None),
            answer,
        }
    }
}

impl Provide for CountingProvider {
    fn save_quit(&self, caller: &'static str, settings: QuitSettings) -> Result<(), Refusal> {
        self.calls.set(self.calls.get() + 1);
        self.caller.set(Some(caller));
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

/// The secrets provider of rows that never reach it.
struct UnusedSecrets;

/// The one `UnusedSecrets` the rows pass.
const UNUSED_SECRETS: UnusedSecrets = UnusedSecrets;

impl secrets::Provide for UnusedSecrets {
    fn put_from_owner(&self, caller: &'static str, _: String) -> Result<Secret, Refusal> {
        panic!("the secrets provider was reached by {caller}");
    }

    fn find(&self, caller: &'static str, _: String) -> Result<Option<Secret>, Refusal> {
        panic!("the secrets provider was reached by {caller}");
    }

    fn forget(&self, caller: &'static str, _: Secret) -> Result<(), Refusal> {
        panic!("the secrets provider was reached by {caller}");
    }
}

/// The secrets provider fake: counts calls and keeps each caller, answering handle 7.
#[derive(Default)]
struct CountingSecrets {
    callers: RefCell<Vec<(&'static str, &'static str)>>,
}

impl secrets::Provide for CountingSecrets {
    fn put_from_owner(&self, caller: &'static str, _: String) -> Result<Secret, Refusal> {
        self.callers.borrow_mut().push(("put_from_owner", caller));
        Ok(Secret::from_kernel(7))
    }

    fn find(&self, caller: &'static str, _: String) -> Result<Option<Secret>, Refusal> {
        self.callers.borrow_mut().push(("find", caller));
        Ok(Some(Secret::from_kernel(7)))
    }

    fn forget(&self, caller: &'static str, _: Secret) -> Result<(), Refusal> {
        self.callers.borrow_mut().push(("forget", caller));
        Ok(())
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
/// write), `low` (t1, test.use), `keeper` (t1, every secrets grant) and `finder` (t1,
/// secrets.find only).
fn grants() -> Grants {
    Grants::new(&[
        ("agent-quit", Tier::T2, &["settings.write"]),
        ("reader", Tier::T2, &["settings.read"]),
        ("low", Tier::T1, &["test.use"]),
        (
            "keeper",
            Tier::T1,
            &["secrets.put", "secrets.find", "secrets.forget"],
        ),
        ("finder", Tier::T1, &["secrets.find"]),
    ])
    .expect("the table builds")
}

/// The refusal of an input over its bound.
fn input_too_large() -> Refusal {
    Refusal::Rejected {
        code: "cap_input_too_large".to_owned(),
    }
}

/// The three secrets calls of one bound plugin, in order. Inputs: the gateway, the
/// providers and the plugin. Output: each call's result, as a refusal or `()`.
fn secrets_calls(
    gateway: &Gateway<'_>,
    providers: &Providers<'_>,
    plugin: &'static str,
) -> [Result<(), Refusal>; 3] {
    let host = Bound::new(gateway.bind(plugin), providers);
    [
        secrets::put_from_owner(&host, "token".to_owned()).map(drop),
        secrets::find(&host, "token".to_owned()).map(drop),
        secrets::forget(&host, Secret::from_kernel(7)),
    ]
}

/// The refusal of a plugin missing one secrets grant. Input: the capability's short
/// name. Output: the refusal.
fn secrets_not_granted(name: &'static str) -> Refusal {
    let (capability, needs) = match name {
        "put_from_owner" => ("secrets.put_from_owner", "secrets.put"),
        "find" => ("secrets.find", "secrets.find"),
        _ => ("secrets.forget", "secrets.forget"),
    };
    Refusal::NotGranted { capability, needs }
}

#[test]
fn a_granted_call_reaches_the_provider_once() {
    let (grants, provider, refusals) = (
        grants(),
        CountingProvider::answering(Ok(())),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &refusals);
    let providers = Providers {
        secrets: &UNUSED_SECRETS,
        settings: &provider,
    };
    assert_eq!(
        settings::save_quit(&Bound::new(gateway.bind("agent-quit"), &providers), quit()),
        Ok(())
    );
    assert_eq!(provider.calls.get(), 1);
    assert_eq!(provider.last.get(), Some(quit()));
    assert!(refusals.seen.borrow().is_empty());
}

#[test]
fn an_ungranted_call_is_refused_logged_once_and_never_reaches_the_provider() {
    let (grants, provider, refusals) = (
        grants(),
        CountingProvider::answering(Ok(())),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &refusals);
    let providers = Providers {
        secrets: &UNUSED_SECRETS,
        settings: &provider,
    };
    let refusal = Refusal::NotGranted {
        capability: "settings.save_quit",
        needs: "settings.write",
    };
    assert_eq!(
        settings::save_quit(&Bound::new(gateway.bind("reader"), &providers), quit()),
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
    let (grants, provider, refusals) = (
        grants(),
        CountingProvider::answering(Ok(())),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &refusals);
    let providers = Providers {
        secrets: &UNUSED_SECRETS,
        settings: &provider,
    };
    let refusal = Refusal::NotGranted {
        capability: "settings.save_quit",
        needs: "settings.write",
    };
    assert_eq!(
        settings::save_quit(&Bound::new(gateway.bind("stranger"), &providers), quit()),
        Err(refusal.clone())
    );
    assert_eq!(provider.calls.get(), 0);
    assert_eq!(
        *refusals.seen.borrow(),
        vec![("stranger", "settings.save_quit", refusal)]
    );
}

#[test]
fn the_provider_rejection_comes_back_unchanged() {
    let rejection = Refusal::Rejected {
        code: "quit_settings_invalid".to_owned(),
    };
    let (grants, provider, refusals) = (
        grants(),
        CountingProvider::answering(Err(rejection.clone())),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &refusals);
    let providers = Providers {
        secrets: &UNUSED_SECRETS,
        settings: &provider,
    };
    assert_eq!(
        settings::save_quit(&Bound::new(gateway.bind("agent-quit"), &providers), quit()),
        Err(rejection)
    );
    assert_eq!(provider.calls.get(), 1);
    assert!(refusals.seen.borrow().is_empty());
}

#[test]
fn the_settings_provider_gets_the_bound_plugin_as_caller() {
    let (grants, provider, refusals) = (
        grants(),
        CountingProvider::answering(Ok(())),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &refusals);
    let providers = Providers {
        secrets: &UNUSED_SECRETS,
        settings: &provider,
    };
    assert_eq!(
        settings::save_quit(&Bound::new(gateway.bind("agent-quit"), &providers), quit()),
        Ok(())
    );
    assert_eq!(provider.caller.get(), Some("agent-quit"));
}

#[test]
fn an_ungranted_secrets_call_never_reaches_the_provider() {
    let (grants, provider, secrets, refusals) = (
        grants(),
        CountingProvider::answering(Ok(())),
        CountingSecrets::default(),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &refusals);
    let providers = Providers {
        secrets: &secrets,
        settings: &provider,
    };
    assert_eq!(
        secrets_calls(&gateway, &providers, "low"),
        [
            Err(secrets_not_granted("put_from_owner")),
            Err(secrets_not_granted("find")),
            Err(secrets_not_granted("forget")),
        ]
    );
    assert!(secrets.callers.borrow().is_empty());
    assert_eq!(refusals.seen.borrow().len(), 3);
}

#[test]
fn each_secrets_call_needs_its_own_grant() {
    let (grants, provider, secrets, refusals) = (
        grants(),
        CountingProvider::answering(Ok(())),
        CountingSecrets::default(),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &refusals);
    let providers = Providers {
        secrets: &secrets,
        settings: &provider,
    };
    assert_eq!(
        secrets_calls(&gateway, &providers, "finder"),
        [
            Err(secrets_not_granted("put_from_owner")),
            Ok(()),
            Err(secrets_not_granted("forget")),
        ]
    );
    assert_eq!(*secrets.callers.borrow(), vec![("find", "finder")]);
    assert_eq!(refusals.seen.borrow().len(), 2);
}

#[test]
fn the_caller_passed_to_the_secrets_provider_is_the_bound_plugin() {
    let (grants, provider, secrets, refusals) = (
        grants(),
        CountingProvider::answering(Ok(())),
        CountingSecrets::default(),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &refusals);
    let providers = Providers {
        secrets: &secrets,
        settings: &provider,
    };
    assert_eq!(
        secrets_calls(&gateway, &providers, "keeper"),
        [Ok(()), Ok(()), Ok(())]
    );
    assert_eq!(
        *secrets.callers.borrow(),
        vec![
            ("put_from_owner", "keeper"),
            ("find", "keeper"),
            ("forget", "keeper"),
        ]
    );
    assert!(refusals.seen.borrow().is_empty());
}

#[test]
fn a_secrets_input_over_its_bound_never_reaches_the_provider() {
    let (grants, provider, secrets, refusals) = (
        grants(),
        CountingProvider::answering(Ok(())),
        CountingSecrets::default(),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &refusals);
    let providers = Providers {
        secrets: &secrets,
        settings: &provider,
    };
    let host = Bound::new(gateway.bind("keeper"), &providers);
    let over = "n".repeat(TEXT_BYTES_MAX + 1);
    assert_eq!(
        secrets::put_from_owner(&host, over.clone()).map(drop),
        Err(input_too_large())
    );
    assert_eq!(secrets::find(&host, over).map(drop), Err(input_too_large()));
    assert!(secrets.callers.borrow().is_empty());
    let at = "n".repeat(TEXT_BYTES_MAX);
    assert!(secrets::find(&host, at).is_ok());
    assert_eq!(*secrets.callers.borrow(), vec![("find", "keeper")]);
    assert!(refusals.seen.borrow().is_empty());
}
