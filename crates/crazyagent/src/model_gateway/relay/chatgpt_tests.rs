use super::chatgpt::*;
use crate::credentials::SecretString;
use crate::engine::EngineError;
use crate::protocol::CredentialRef;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now() -> Duration {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap()
}

fn tokens(access: &str, account_id: Option<&str>, expires: Duration) -> ChatGptTokens {
    ChatGptTokens {
        access: SecretString::bearer(access.as_bytes().to_vec()).unwrap(),
        account_id: account_id.map(str::to_owned),
        expires,
    }
}

fn profile() -> CredentialRef {
    CredentialRef::new("row-profile").unwrap()
}

fn never() -> bool {
    false
}

/// A renewal for these rows that counts its calls, records the expiry each call saw and
/// answers with what `answer` makes. Input: the answer. Output: the call count, the seen
/// expiries and the renewal.
fn counted(
    answer: impl Fn() -> Result<ChatGptTokens, EngineError> + Send + Sync + 'static,
) -> (Arc<AtomicUsize>, Arc<Mutex<Vec<Duration>>>, Renew) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (counting, recording) = (Arc::clone(&calls), Arc::clone(&seen));
    let renew: Renew = Box::new(move |stale: Duration, _halted: &dyn Fn() -> bool| {
        counting.fetch_add(1, Ordering::SeqCst);
        recording.lock().unwrap().push(stale);
        answer()
    });
    (calls, seen, renew)
}

fn handed(source: &ChatGptSource) -> Option<(String, Option<String>)> {
    source
        .current(&never)
        .ok()
        .map(|(access, account_id)| (access.expose().to_owned(), account_id))
}

fn refusal(source: &ChatGptSource) -> Option<String> {
    source.current(&never).err().map(|error| error.0)
}

/// T22 R3: a token with more than the renewal margin left is handed out as it is, with no
/// renewal.
#[test]
fn a_token_with_time_left_is_handed_out_without_a_renewal() {
    let (calls, _, renew) = counted(|| Ok(tokens("fresh", Some("acct-2"), now() * 2)));
    let source = ChatGptSource::new(
        profile(),
        tokens("held", Some("acct-1"), now() + Duration::from_secs(3600)),
        renew,
    );
    assert_eq!(
        handed(&source),
        Some(("held".to_owned(), Some("acct-1".to_owned())))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(source.failed(), None);
}

/// T22 R3: a token that lapses within the renewal margin is renewed once before it is
/// handed out, the renewal is told the expiry the source saw, and the fresh token is kept
/// for the requests after it.
#[test]
fn a_token_lapsing_within_the_margin_is_renewed_once_before_it_is_handed_out() {
    let held = now() + RENEW_MARGIN - Duration::from_secs(100);
    let (calls, seen, renew) = counted(|| {
        Ok(tokens(
            "fresh",
            Some("acct-2"),
            now() + Duration::from_secs(3600),
        ))
    });
    let source = ChatGptSource::new(profile(), tokens("held", Some("acct-1"), held), renew);
    assert_eq!(
        handed(&source),
        Some(("fresh".to_owned(), Some("acct-2".to_owned())))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(*seen.lock().unwrap(), [held]);
    assert_eq!(
        handed(&source),
        Some(("fresh".to_owned(), Some("acct-2".to_owned())))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// T22 R3: ten requests that all find the token lapsing cause one renewal, and every one
/// of them is handed the fresh token.
#[test]
fn ten_requests_at_once_cause_one_renewal() {
    let (calls, _, renew) = counted(|| {
        std::thread::sleep(Duration::from_millis(200));
        Ok(tokens("fresh", None, now() + Duration::from_secs(3600)))
    });
    let source = Arc::new(ChatGptSource::new(
        profile(),
        tokens("held", None, now() + Duration::from_secs(10)),
        renew,
    ));
    let start = Arc::new(Barrier::new(10));
    let requests: Vec<_> = (0..10)
        .map(|_| {
            let (source, start) = (Arc::clone(&source), Arc::clone(&start));
            std::thread::spawn(move || {
                start.wait();
                handed(&source)
            })
        })
        .collect();
    for request in requests {
        assert_eq!(request.join().unwrap(), Some(("fresh".to_owned(), None)));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// T22 R3: a renewal that leaves a token with time left as it was is not a failure: the
/// token is forwarded, and no new renewal starts before the backoff ends, so each request
/// does not start a Codex renewal of its own.
#[test]
fn a_renewal_that_leaves_the_token_as_it_was_is_not_retried_before_the_backoff_ends() {
    let held = now() + Duration::from_secs(200);
    let (calls, _, renew) = counted(move || Ok(tokens("held", Some("acct-1"), held)));
    let source = ChatGptSource::new(profile(), tokens("held", Some("acct-1"), held), renew);
    for _ in 0..3 {
        assert_eq!(
            handed(&source),
            Some(("held".to_owned(), Some("acct-1".to_owned())))
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(source.failed(), None);
}

/// T22 R3: a renewal that fails (busy, no network) while the token still has more than a
/// minute left forwards the token it had, and the run goes on.
#[test]
fn a_failed_renewal_while_the_token_still_has_time_forwards_the_token_it_had() {
    let held = now() + Duration::from_secs(200);
    let (calls, _, renew) = counted(|| Err(EngineError("native_profile_renewal_busy".into())));
    let source = ChatGptSource::new(profile(), tokens("held", None, held), renew);
    assert_eq!(handed(&source), Some(("held".to_owned(), None)));
    assert_eq!(handed(&source), Some(("held".to_owned(), None)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(source.failed(), None);
}

/// T22 R4: a renewal that leaves the sign-in lapsed refuses the request with
/// `chatgpt_sign_in_expired`, which has its plain sentence, and the source keeps that
/// failure: later requests are refused without another renewal.
#[test]
fn a_renewal_that_leaves_the_sign_in_lapsed_stops_the_source_for_good() {
    let lapsed = now() - Duration::from_secs(10);
    let (calls, _, renew) = counted(move || Ok(tokens("held", None, lapsed)));
    let source = ChatGptSource::new(profile(), tokens("held", None, lapsed), renew);
    assert_eq!(refusal(&source), Some(SIGN_IN_EXPIRED.to_owned()));
    assert_eq!(source.failed(), Some(SIGN_IN_EXPIRED));
    assert_eq!(refusal(&source), Some(SIGN_IN_EXPIRED.to_owned()));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        crate::config::plain_problem(SIGN_IN_EXPIRED),
        Some("This ChatGPT sign-in has expired. Sign in again in Settings, Accounts.")
    );
}

/// T22 R4: a renewal refused for a token with less than a minute left (Codex ended the
/// sign-in) stops the source the same way.
#[test]
fn a_refused_renewal_of_a_token_about_to_lapse_stops_the_source() {
    let (calls, _, renew) = counted(|| Err(EngineError("native_profile_sign_in_revoked".into())));
    let source = ChatGptSource::new(
        profile(),
        tokens("held", None, now() + Duration::from_secs(30)),
        renew,
    );
    assert_eq!(refusal(&source), Some(SIGN_IN_EXPIRED.to_owned()));
    assert_eq!(source.failed(), Some(SIGN_IN_EXPIRED));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// T22 R3: the relay's stop test reaches the renewal, so a renewal waiting for the
/// account's renewal lock leaves the wait once the relay stops.
#[test]
fn the_relay_stop_test_reaches_the_renewal() {
    let told = Arc::new(AtomicBool::new(false));
    let telling = Arc::clone(&told);
    let renew: Renew = Box::new(move |_: Duration, halted: &dyn Fn() -> bool| {
        telling.store(halted(), Ordering::SeqCst);
        Err(EngineError("native_profile_renewal_stopped".into()))
    });
    let source = ChatGptSource::new(
        profile(),
        tokens("held", None, now() + Duration::from_secs(200)),
        renew,
    );
    let _ = source.current(&|| true);
    assert!(told.load(Ordering::SeqCst));
}

/// T22: a renewal its relay stopped is no sign that the sign-in lapsed. A source may be
/// shared by several relays of one account, so stopping one run neither stops the source
/// nor tells the others to sign in again: the next request renews and is served.
#[test]
fn a_renewal_its_relay_stopped_does_not_stop_the_source() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&calls);
    let renew: Renew = Box::new(move |_: Duration, halted: &dyn Fn() -> bool| {
        counting.fetch_add(1, Ordering::SeqCst);
        if halted() {
            return Err(EngineError("native_profile_renewal_stopped".into()));
        }
        Ok(tokens("fresh", None, now() + Duration::from_secs(3600)))
    });
    let source = ChatGptSource::new(
        profile(),
        tokens("held", None, now() + Duration::from_secs(30)),
        renew,
    );
    assert_ne!(
        source.current(&|| true).err().map(|problem| problem.0),
        None
    );
    assert_eq!(source.failed(), None);
    assert_eq!(handed(&source), Some(("fresh".to_owned(), None)));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
