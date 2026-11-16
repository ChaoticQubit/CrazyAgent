use super::*;
use crate::connectors::callback::{CALLBACK_PATH, DONE_PAGE, FAILED_PAGE, REFUSED_MAX};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const WAIT: Duration = Duration::from_secs(30);
const STATE: &str = "Sx7-state_0123456789abcdef";
const AUTHORIZE: &str = "https://auth.example/authorize?client_id=app&state=Sx7";
const SIGNED_IN: SignInState = SignInState::SignedIn;

/// What a recording finish was asked to redeem: each state, code and issuer.
type Redeemed = Arc<Mutex<Vec<(String, String, Option<String>)>>>;

/// An engine root of its own, so rows running at once never share a sign-in. Output: the
/// temporary folder and the root.
fn engine_root() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    (tmp, root)
}

/// A finish that records what it redeems and answers `answer`. Input: the answer. Output:
/// the record and the finish.
fn redeeming(
    answer: Result<(), OAuthError>,
) -> (
    Redeemed,
    impl FnOnce(&str, &str, Option<&str>) -> Result<(), OAuthError> + Send + 'static,
) {
    let redeemed: Redeemed = Arc::default();
    let kept = redeemed.clone();
    let finish = move |state: &str, code: &str, iss: Option<&str>| {
        let seen = (state.to_owned(), code.to_owned(), iss.map(str::to_owned));
        kept.lock().unwrap().push(seen);
        answer
    };
    (redeemed, finish)
}

/// Starts a sign-in of `name` whose begin answers `AUTHORIZE` with `state` and keeps the
/// loopback address it was given. Inputs: the root, the connector, the state, the wait and
/// the finish. Output: the address for the browser and the loopback address.
fn started(
    root: &Path,
    name: &str,
    state: &str,
    limit: Duration,
    finish: impl FnOnce(&str, &str, Option<&str>) -> Result<(), OAuthError> + Send + 'static,
) -> (url::Url, url::Url) {
    let given = Arc::new(Mutex::new(None));
    let seen = given.clone();
    let pending = Pending {
        authorize_url: url::Url::parse(AUTHORIZE).unwrap(),
        state: state.to_owned(),
    };
    let begin = move |redirect: &url::Url| {
        *seen.lock().unwrap() = Some(redirect.clone());
        Ok(pending)
    };
    let opened = start_with(root, name, limit, begin, finish).unwrap();
    let redirect = given.lock().unwrap().clone().unwrap();
    (opened, redirect)
}

/// Sends the browser's answer to a loopback address. Inputs: the address and the query.
/// Output: the status, 0 when none came, and the page.
fn answered(redirect: &url::Url, query: &str) -> (u16, String) {
    let port = redirect.port().unwrap();
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    let request = format!(
        "GET {CALLBACK_PATH}?{query} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut reply = Vec::new();
    let _ = stream.read_to_end(&mut reply);
    let reply = String::from_utf8_lossy(&reply).into_owned();
    let status = reply
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let page = reply.split_once("\r\n\r\n").map_or("", |(_, page)| page);
    (status, page.to_owned())
}

fn closed(redirect: &url::Url) -> bool {
    let port = redirect.port().unwrap();
    TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err()
}

/// Waits until `check` holds, at most `WAIT`. Inputs: what is waited for and the check.
fn until(what: &str, check: impl Fn() -> bool) {
    let began = Instant::now();
    while !check() {
        assert!(began.elapsed() < WAIT, "{what} never happened");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn failed(code: &str) -> SignInState {
    SignInState::Failed {
        code: code.to_owned(),
    }
}

/// DEC-168 Task 10: a sign-in opens a loopback callback, starts with its address, answers
/// the authorization address for the owner's browser and waits. The browser's answer with
/// its state is redeemed once with its code and issuer, the sign-in reads signed in and the
/// port closes; no state the pages read carries the code.
#[test]
fn a_sign_in_ends_signed_in_once_the_browser_brings_its_state_back() {
    let (_tmp, root) = engine_root();
    let (redeemed, finish) = redeeming(Ok(()));
    let (opened, redirect) = started(&root, "board", STATE, WAIT, finish);
    assert_eq!(opened.as_str(), AUTHORIZE);
    assert_eq!(redirect.scheme(), "http");
    assert_eq!(redirect.host_str(), Some("127.0.0.1"));
    assert_eq!(redirect.path(), CALLBACK_PATH);
    assert!(redirect.port().is_some_and(|port| port != 0));
    assert!(redirect.query().is_none());
    assert_eq!(state_of(&root, "board"), SignInState::Waiting);
    assert_eq!(state_of(&root, "desk"), SignInState::Idle);
    let query = format!(
        "state={STATE}&code=code-c0ffee-1&iss=https%3A%2F%2Fauth.example"
    );
    assert_eq!(answered(&redirect, &query), (200, DONE_PAGE.to_owned()));
    until("signed in", || state_of(&root, "board") == SIGNED_IN);
    let iss = Some("https://auth.example".to_owned());
    let once = vec![(STATE.to_owned(), "code-c0ffee-1".to_owned(), iss)];
    assert_eq!(*redeemed.lock().unwrap(), once);
    assert!(closed(&redirect));
    let shown = serde_json::to_string(&state_of(&root, "board")).unwrap();
    assert_eq!(shown, r#"{"state":"signed_in"}"#);
}

/// DEC-168 addendum 30 decision 5: a second sign-in of one connector closes the first one's
/// callback, so only the new one's answer is redeemed.
#[test]
fn a_new_sign_in_closes_the_waiting_one_and_only_its_answer_counts() {
    let (_tmp, root) = engine_root();
    let (first_redeemed, finish) = redeeming(Ok(()));
    let (_, first) = started(&root, "board", "first-state-0123456789", WAIT, finish);
    let (redeemed, finish) = redeeming(Ok(()));
    let (_, second) = started(&root, "board", STATE, WAIT, finish);
    assert_ne!(first.port(), second.port());
    until("the first callback closed", || closed(&first));
    assert_eq!(state_of(&root, "board"), SignInState::Waiting);
    let query = format!("state={STATE}&code=c2");
    assert_eq!(answered(&second, &query).0, 200);
    until("signed in", || state_of(&root, "board") == SIGNED_IN);
    assert!(first_redeemed.lock().unwrap().is_empty());
    assert_eq!(redeemed.lock().unwrap().len(), 1);
}

/// DEC-168 addendum 30 decision 5: a cancelled sign-in closes its callback at once and reads
/// cancelled, and nothing is redeemed; a connector with no sign-in stays as it was.
#[test]
fn a_cancelled_sign_in_closes_its_callback_and_reads_cancelled() {
    let (_tmp, root) = engine_root();
    let (redeemed, finish) = redeeming(Ok(()));
    let (_, redirect) = started(&root, "board", STATE, WAIT, finish);
    assert_eq!(cancel(&root, "board"), SignInState::Cancelled);
    until("the callback closed", || closed(&redirect));
    assert_eq!(state_of(&root, "board"), SignInState::Cancelled);
    assert!(redeemed.lock().unwrap().is_empty());
    assert_eq!(cancel(&root, "desk"), SignInState::Idle);
}

/// DEC-168 addendum 30, update 23:07: once the browser's answer is claimed, its redeeming
/// runs to its end and a cancel no longer ends the sign-in, so the page never reads
/// cancelled for tokens that were saved.
#[test]
fn a_cancel_after_the_answer_came_leaves_the_sign_in_to_finish() {
    let (_tmp, root) = engine_root();
    let (enter, entered) = mpsc::channel::<()>();
    let (release, hold) = mpsc::channel::<()>();
    let finish = move |_: &str, _: &str, _: Option<&str>| {
        enter.send(()).unwrap();
        hold.recv().unwrap();
        Ok(())
    };
    let (_, redirect) = started(&root, "board", STATE, WAIT, finish);
    let query = format!("state={STATE}&code=c3");
    assert_eq!(answered(&redirect, &query).0, 200);
    entered.recv_timeout(WAIT).unwrap();
    assert_eq!(cancel(&root, "board"), SignInState::Waiting);
    assert_eq!(state_of(&root, "board"), SignInState::Waiting);
    release.send(()).unwrap();
    until("signed in", || state_of(&root, "board") == SIGNED_IN);
}

/// DEC-168 addendum 30 decision 6: a sign-in that does not finish reads failed with its
/// code: the vendor's refusal, the end of the wait, a flood of wrong answers, or what the
/// redeeming answered. No failed state carries the code the browser brought.
#[test]
fn a_sign_in_that_does_not_finish_reads_failed_with_its_code() {
    let (_tmp, root) = engine_root();
    let (redeemed, finish) = redeeming(Ok(()));
    let (_, redirect) = started(&root, "board", STATE, WAIT, finish);
    let query = format!("state={STATE}&error=access_denied");
    assert_eq!(answered(&redirect, &query), (200, FAILED_PAGE.to_owned()));
    let refused = failed("connector_oauth_refused");
    until("refused", || state_of(&root, "board") == refused);
    assert!(redeemed.lock().unwrap().is_empty());

    let (_, finish) = redeeming(Ok(()));
    let short = Duration::from_millis(300);
    let (_, redirect) = started(&root, "board", STATE, short, finish);
    let expired = failed("connector_oauth_callback_expired");
    until("expired", || state_of(&root, "board") == expired);
    assert!(closed(&redirect));

    let (_, finish) = redeeming(Ok(()));
    let (_, redirect) = started(&root, "board", STATE, WAIT, finish);
    for _ in 0..REFUSED_MAX {
        assert_eq!(answered(&redirect, "state=wrong&code=c1").0, 400);
    }
    let flooded = failed("connector_oauth_callback_flooded");
    until("flooded", || state_of(&root, "board") == flooded);

    let (_, finish) = redeeming(Err(OAuthError("connector_oauth_token_invalid")));
    let (_, redirect) = started(&root, "board", STATE, WAIT, finish);
    let query = format!("state={STATE}&code=code-c0ffee-4");
    assert_eq!(answered(&redirect, &query).0, 200);
    let invalid = failed("connector_oauth_token_invalid");
    until("token refused", || state_of(&root, "board") == invalid);
    let shown = serde_json::to_string(&state_of(&root, "board")).unwrap();
    let said = r#"{"state":"failed","code":"connector_oauth_token_invalid"}"#;
    assert_eq!(shown, said);
    assert!(!shown.contains("c0ffee"), "{shown}");
}

/// DEC-168 Task 10: a sign-in whose start is refused answers that refusal at once and leaves
/// nothing waiting, its callback closed; so does an authorization address that is not
/// https, which the owner's browser would open in the clear. A waiting sign-in of the
/// connector stays as it was.
#[test]
fn a_refused_start_answers_at_once_and_leaves_nothing_waiting() {
    let (_tmp, root) = engine_root();
    let given = Arc::new(Mutex::new(None));
    let seen = given.clone();
    let (_, finish) = redeeming(Ok(()));
    let refused = start_with(
        &root,
        "board",
        WAIT,
        move |redirect: &url::Url| {
            *seen.lock().unwrap() = Some(redirect.clone());
            Err(OAuthError("connector_oauth_not_offered"))
        },
        finish,
    );
    assert_eq!(refused.unwrap_err().0, "connector_oauth_not_offered");
    assert_eq!(state_of(&root, "board"), SignInState::Idle);
    let redirect = given.lock().unwrap().clone().unwrap();
    assert!(closed(&redirect));

    let (_, finish) = redeeming(Ok(()));
    let (_, waiting) = started(&root, "board", STATE, WAIT, finish);
    let plain = Pending {
        authorize_url: url::Url::parse("http://auth.example/authorize").unwrap(),
        state: STATE.to_owned(),
    };
    let (_, finish) = redeeming(Ok(()));
    let refused = start_with(&root, "board", WAIT, move |_: &url::Url| Ok(plain), finish);
    assert_eq!(refused.unwrap_err().0, "connector_address_refused");
    assert_eq!(state_of(&root, "board"), SignInState::Waiting);
    assert!(!closed(&waiting), "a refused start left the waiting one alone");
    assert_eq!(cancel(&root, "board"), SignInState::Cancelled);
}

/// DEC-168 addendum 30, update 23:07: at most `WAITING_MAX` sign-ins wait per engine root;
/// the next one cancels the oldest, and another root's sign-ins are not counted.
#[test]
fn at_most_eight_sign_ins_wait_per_root_and_the_oldest_is_cancelled() {
    assert_eq!(WAITING_MAX, 8);
    let (_tmp, root) = engine_root();
    let mut redirects = Vec::new();
    for n in 0..=WAITING_MAX {
        let (_, finish) = redeeming(Ok(()));
        let state = format!("state-{n:020}");
        let (_, redirect) = started(&root, &format!("c{n}"), &state, WAIT, finish);
        redirects.push(redirect);
    }
    until("the oldest callback closed", || closed(&redirects[0]));
    assert_eq!(state_of(&root, "c0"), SignInState::Cancelled);
    for n in 1..=WAITING_MAX {
        let name = format!("c{n}");
        assert_eq!(state_of(&root, &name), SignInState::Waiting, "{name}");
    }
    let (_other_tmp, other) = engine_root();
    let (_, finish) = redeeming(Ok(()));
    let (_, elsewhere) = started(&other, "c1", STATE, WAIT, finish);
    assert_eq!(state_of(&root, "c1"), SignInState::Waiting);
    assert_eq!(state_of(&other, "c1"), SignInState::Waiting);
    for n in 1..=WAITING_MAX {
        assert_eq!(cancel(&root, &format!("c{n}")), SignInState::Cancelled);
    }
    assert_eq!(cancel(&other, "c1"), SignInState::Cancelled);
    until("every callback closed", || {
        redirects.iter().all(closed) && closed(&elsewhere)
    });
}
