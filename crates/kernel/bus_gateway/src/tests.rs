use std::cell::{Cell, RefCell};

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

#[test]
fn a_granted_call_reaches_the_provider_once() {
    let (grants, provider, refusals) = (
        grants(),
        CountingProvider::answering(Ok(())),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &provider, &UNUSED_SECRETS, &refusals);
    assert_eq!(
        settings::save_quit(&gateway.bind("agent-quit"), quit()),
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
    let gateway = Gateway::new(&grants, &provider, &UNUSED_SECRETS, &refusals);
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
    let (grants, provider, refusals) = (
        grants(),
        CountingProvider::answering(Ok(())),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &provider, &UNUSED_SECRETS, &refusals);
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
    let (grants, provider, refusals) = (
        grants(),
        CountingProvider::answering(Ok(())),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &provider, &UNUSED_SECRETS, &refusals);
    let refusal = Refusal::TierTooHigh {
        capability: "test.t2_only",
    };
    assert_eq!(gateway.admit("low", &T2_ONLY), Err(refusal.clone()));
    assert_eq!(gateway.admit("low", &T1_SAME), Ok(()));
    let ran = Cell::new(false);
    assert_eq!(
        gateway.call(
            "low",
            &T2_ONLY,
            (),
            |_: &()| Ok(()),
            |()| {
                ran.set(true);
                Ok(())
            },
            |_: &()| Ok(())
        ),
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
    let (grants, provider, refusals) = (
        grants(),
        CountingProvider::answering(Err(rejection.clone())),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &provider, &UNUSED_SECRETS, &refusals);
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
    let entries: Vec<(&'static str, Tier, &[&'static str])> = names
        .iter()
        .map(|name| (*name, Tier::T2, &[][..]))
        .collect();
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

#[test]
fn the_settings_provider_gets_the_bound_plugin_as_caller() {
    let (grants, provider, refusals) = (
        grants(),
        CountingProvider::answering(Ok(())),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &provider, &UNUSED_SECRETS, &refusals);
    assert_eq!(
        settings::save_quit(&gateway.bind("agent-quit"), quit()),
        Ok(())
    );
    assert_eq!(provider.caller.get(), Some("agent-quit"));
}

/// A capability the bound rows call through `Gateway::call`, granted to `low`.
const BOUNDED: Capability = Capability {
    name: "test.bounded",
    tier: Tier::T1,
    needs: "test.use",
};

/// The bound the rows' checks hold a text to.
const ROW_BYTES_MAX: usize = 8;

/// The bound check of the rows: a text at most `ROW_BYTES_MAX` bytes. Inputs: the text and
/// the refusal. Output: nothing, or the refusal.
fn within(text: &str, refusal: fn() -> Refusal) -> Result<(), Refusal> {
    if text.len() <= ROW_BYTES_MAX {
        Ok(())
    } else {
        Err(refusal())
    }
}

/// The refusal of an input over its bound.
fn input_too_large() -> Refusal {
    Refusal::Rejected {
        code: "cap_input_too_large".to_owned(),
    }
}

/// The refusal of an output over its bound.
fn output_too_large() -> Refusal {
    Refusal::Failed {
        code: "cap_output_too_large".to_owned(),
    }
}

/// Calls `BOUNDED` as `low` with one input and the output the provider answers.
/// Inputs: the gateway, the input, the output and the provider's call counter.
/// Output: the call's result.
fn bounded_call(
    gateway: &Gateway<'_>,
    input: &str,
    output: &str,
    ran: &Cell<u32>,
) -> Result<String, Refusal> {
    gateway.call(
        "low",
        &BOUNDED,
        input.to_owned(),
        |input: &String| within(input, input_too_large),
        |_input: String| {
            ran.set(ran.get() + 1);
            Ok(output.to_owned())
        },
        |answer: &String| within(answer, output_too_large),
    )
}

#[test]
fn an_input_over_its_bound_never_reaches_the_provider() {
    let (grants, provider, refusals) = (
        grants(),
        CountingProvider::answering(Ok(())),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &provider, &UNUSED_SECRETS, &refusals);
    let ran = Cell::new(0);
    let over = "i".repeat(ROW_BYTES_MAX + 1);
    assert_eq!(
        bounded_call(&gateway, &over, "ok", &ran),
        Err(input_too_large())
    );
    assert_eq!(ran.get(), 0);
    assert!(refusals.seen.borrow().is_empty());
}

#[test]
fn an_input_at_its_bound_reaches_the_provider() {
    let (grants, provider, refusals) = (
        grants(),
        CountingProvider::answering(Ok(())),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &provider, &UNUSED_SECRETS, &refusals);
    let ran = Cell::new(0);
    let at = "i".repeat(ROW_BYTES_MAX);
    assert_eq!(bounded_call(&gateway, &at, "ok", &ran), Ok("ok".to_owned()));
    assert_eq!(ran.get(), 1);
    assert!(refusals.seen.borrow().is_empty());
}

#[test]
fn an_output_over_its_bound_fails_with_cap_output_too_large() {
    let (grants, provider, refusals) = (
        grants(),
        CountingProvider::answering(Ok(())),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &provider, &UNUSED_SECRETS, &refusals);
    let ran = Cell::new(0);
    let over = "o".repeat(ROW_BYTES_MAX + 1);
    assert_eq!(
        bounded_call(&gateway, "in", &over, &ran),
        Err(output_too_large())
    );
    assert_eq!(ran.get(), 1);
    let at = "o".repeat(ROW_BYTES_MAX);
    assert_eq!(bounded_call(&gateway, "in", &at, &ran), Ok(at.clone()));
    assert_eq!(ran.get(), 2);
    assert!(refusals.seen.borrow().is_empty());
}

#[test]
fn an_ungranted_bounded_call_runs_no_check() {
    let (grants, provider, refusals) = (
        grants(),
        CountingProvider::answering(Ok(())),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &provider, &UNUSED_SECRETS, &refusals);
    let checked = Cell::new(false);
    let refusal = Refusal::NotGranted {
        capability: "test.bounded",
        needs: "test.use",
    };
    assert_eq!(
        gateway.call(
            "reader",
            &BOUNDED,
            (),
            |_: &()| {
                checked.set(true);
                Ok(())
            },
            |()| Ok(()),
            |_: &()| Ok(())
        ),
        Err(refusal.clone())
    );
    assert!(!checked.get());
    assert_eq!(
        *refusals.seen.borrow(),
        vec![("reader", "test.bounded", refusal)]
    );
}

/// The owned input of the SA1 rows: never `Copy`, so a check before the provider holds the
/// very value the provider then gets.
struct Request {
    url: String,
}

/// Calls `BOUNDED` as `plugin` with an owned `Request`, recording the address of the url
/// buffer the check saw and the one the provider got. Inputs: the gateway, the plugin, the
/// url and the provider's call counter. Output: the call's result and both addresses.
fn owned_call(
    gateway: &Gateway<'_>,
    plugin: &'static str,
    url: String,
    ran: &Cell<u32>,
) -> (Result<(), Refusal>, Option<usize>, Option<usize>) {
    let (checked, provided) = (Cell::new(None), Cell::new(None));
    let result = gateway.call(
        plugin,
        &BOUNDED,
        Request { url },
        |request: &Request| {
            checked.set(Some(request.url.as_ptr() as usize));
            within(&request.url, input_too_large)
        },
        |request: Request| {
            ran.set(ran.get() + 1);
            provided.set(Some(request.url.as_ptr() as usize));
            Ok(())
        },
        |_: &()| Ok(()),
    );
    (result, checked.get(), provided.get())
}

#[test]
fn an_owned_input_over_its_bound_never_reaches_the_provider() {
    let (grants, provider, refusals) = (
        grants(),
        CountingProvider::answering(Ok(())),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &provider, &UNUSED_SECRETS, &refusals);
    let ran = Cell::new(0);
    let (result, checked, provided) =
        owned_call(&gateway, "low", "u".repeat(ROW_BYTES_MAX + 1), &ran);
    assert_eq!(result, Err(input_too_large()));
    assert!(checked.is_some());
    assert_eq!((ran.get(), provided), (0, None));
    assert!(refusals.seen.borrow().is_empty());
}

#[test]
fn an_owned_input_at_its_bound_reaches_the_provider_as_the_value_checked() {
    let (grants, provider, refusals) = (
        grants(),
        CountingProvider::answering(Ok(())),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &provider, &UNUSED_SECRETS, &refusals);
    let ran = Cell::new(0);
    let url = "u".repeat(ROW_BYTES_MAX);
    let sent = url.as_ptr() as usize;
    let (result, checked, provided) = owned_call(&gateway, "low", url, &ran);
    assert_eq!(result, Ok(()));
    assert_eq!(ran.get(), 1);
    assert_eq!(checked, Some(sent));
    assert_eq!(provided, Some(sent));
}

#[test]
fn an_ungranted_plugin_with_an_oversized_input_is_refused_not_granted() {
    let (grants, provider, refusals) = (
        grants(),
        CountingProvider::answering(Ok(())),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &provider, &UNUSED_SECRETS, &refusals);
    let ran = Cell::new(0);
    let refusal = Refusal::NotGranted {
        capability: "test.bounded",
        needs: "test.use",
    };
    let (result, checked, provided) =
        owned_call(&gateway, "reader", "u".repeat(ROW_BYTES_MAX + 1), &ran);
    assert_eq!(result, Err(refusal.clone()));
    assert_eq!((checked, provided, ran.get()), (None, None, 0));
    assert_eq!(
        *refusals.seen.borrow(),
        vec![("reader", "test.bounded", refusal)]
    );
}

/// The three secrets calls of one bound plugin, in order. Inputs: the gateway and the
/// plugin. Output: each call's result, as a refusal or `()`.
fn secrets_calls(gateway: &Gateway<'_>, plugin: &'static str) -> [Result<(), Refusal>; 3] {
    let host = gateway.bind(plugin);
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
fn an_ungranted_secrets_call_never_reaches_the_provider() {
    let (grants, provider, secrets, refusals) = (
        grants(),
        CountingProvider::answering(Ok(())),
        CountingSecrets::default(),
        RecordingRefusals::default(),
    );
    let gateway = Gateway::new(&grants, &provider, &secrets, &refusals);
    assert_eq!(
        secrets_calls(&gateway, "low"),
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
    let gateway = Gateway::new(&grants, &provider, &secrets, &refusals);
    assert_eq!(
        secrets_calls(&gateway, "finder"),
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
    let gateway = Gateway::new(&grants, &provider, &secrets, &refusals);
    assert_eq!(secrets_calls(&gateway, "keeper"), [Ok(()), Ok(()), Ok(())]);
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
    let gateway = Gateway::new(&grants, &provider, &secrets, &refusals);
    let host = gateway.bind("keeper");
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

#[test]
fn the_settings_checks_are_the_generated_ones() {
    assert_eq!(settings::check_save_quit_input(&quit()), Ok(()));
    assert_eq!(settings::check_save_quit_output(&()), Ok(()));
}
