use super::*;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::Instant;

const STATE: &str = "Zx9-state_0123456789abcdef";
const WAIT: Duration = Duration::from_secs(30);
const CLIENT_METADATA: &str = include_str!("../../assets/oauth/client-metadata.json");

/// Sends one raw request to the callback and reads its whole answer. Inputs: the port and
/// the request. Output: the answer's status code, 0 when none came, and its body.
fn ask(port: u16, request: &[u8]) -> (u16, String) {
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream.write_all(request).unwrap();
    let mut answer = Vec::new();
    let _ = stream.read_to_end(&mut answer);
    let answer = String::from_utf8_lossy(&answer).into_owned();
    let status = answer
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = answer.split_once("\r\n\r\n").map_or("", |(_, body)| body);
    (status, body.to_owned())
}

/// One request with no body. Inputs: the method and the target. Output: its bytes.
fn sent(method: &str, target: &str) -> Vec<u8> {
    let text = format!("{method} {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    text.into_bytes()
}

fn get(target: &str) -> Vec<u8> {
    sent("GET", target)
}

/// A callback waiting for `STATE` on a thread of its own. Input: the limit. Output: its
/// port, its stop flag and the thread, which returns how the wait ended.
fn waiting(limit: Duration) -> (u16, Arc<AtomicBool>, JoinHandle<Waited>) {
    let callback = Callback::open().unwrap();
    let port = callback.redirect().port().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let told = stop.clone();
    let thread = std::thread::spawn(move || callback.wait(STATE, limit, &told));
    (port, stop, thread)
}

fn closed(port: u16) -> bool {
    TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err()
}

/// A connection that sends one byte of a request head every 100 ms for `lasting`, or until
/// the callback closes it, on a thread of its own. Inputs: the port and how long. Output:
/// the thread.
fn dripping(port: u16, lasting: Duration) -> JoinHandle<()> {
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    std::thread::spawn(move || {
        let began = Instant::now();
        for byte in b"GET /callback?state=".iter().cycle() {
            if began.elapsed() >= lasting || stream.write_all(&[*byte]).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })
}

/// DEC-168 Task 10: the callback listens on loopback at a port of its own under
/// `/callback`. Every request that is not a GET of that path with the sign-in's one state
/// and a code is refused without ending the wait; the one that is ends it with its code and
/// issuer, the browser reads that it can close the tab, and the port closes.
#[test]
fn the_callback_accepts_one_matching_state_and_refuses_everything_else() {
    let callback = Callback::open().unwrap();
    let redirect = callback.redirect().clone();
    assert_eq!(redirect.scheme(), "http");
    assert_eq!(redirect.host_str(), Some("127.0.0.1"));
    assert_eq!(redirect.path(), CALLBACK_PATH);
    assert!(redirect.port().is_some_and(|port| port != 0));
    assert!(redirect.query().is_none());
    drop(callback);
    let (port, _stop, thread) = waiting(WAIT);
    let oversized = format!(
        "GET /callback?state={STATE}&code=c1 HTTP/1.1\r\nX: {}\r\n\r\n",
        "x".repeat(9 * 1024)
    );
    let tabbed = format!("/callback?state={}\t{}&code=c1", &STATE[..3], &STATE[3..]);
    let refused = [
        get(&tabbed),
        sent("POST", &format!("/callback?state={STATE}&code=c1")),
        get(&format!("/other?state={STATE}&code=c1")),
        get(&format!("//callback?state={STATE}&code=c1")),
        get(&format!("/./callback?state={STATE}&code=c1")),
        get(&format!("/x/../callback?state={STATE}&code=c1")),
        get(&format!("/call\tback?state={STATE}&code=c1")),
        get(&format!("/callback/?state={STATE}&code=c1")),
        get(&format!("/%63allback?state={STATE}&code=c1")),
        get("/callback?state=wrong&code=c1"),
        get(&format!("/callback?state={STATE}x&code=c1")),
        get("/callback?code=c1"),
        get(&format!("/callback?state={STATE}&state={STATE}&code=c1")),
        get(&format!("/callback?state={STATE}")),
        get(&format!("/callback?state={STATE}&code=")),
        oversized.into_bytes(),
        b"nonsense\r\n\r\n".to_vec(),
    ];
    for request in &refused {
        let cut = &request[..request.len().min(60)];
        let shown = String::from_utf8_lossy(cut);
        assert_eq!(ask(port, request).0, 400, "{shown}");
        assert!(
            !thread.is_finished(),
            "a refused request ended the wait: {shown}"
        );
    }
    let target = format!(
        "/callback?state={STATE}&code=c%2B1&iss=https%3A%2F%2Fauth.example"
    );
    let (status, body) = ask(port, &get(&target));
    assert_eq!((status, body.as_str()), (200, DONE_PAGE));
    let iss = Some("https://auth.example".to_owned());
    let code = "c+1".to_owned();
    assert_eq!(thread.join().unwrap(), Waited::Code { code, iss });
    assert!(closed(port), "the port closes once the answer came");
}

/// DEC-168 Task 10: a callback no answer reaches closes at its limit, five minutes for a
/// sign-in, even while a connection sits open saying nothing.
#[test]
fn the_callback_closes_after_five_minutes() {
    assert_eq!(CALLBACK_WAIT, Duration::from_secs(5 * 60));
    let started = Instant::now();
    let (port, _stop, thread) = waiting(Duration::from_millis(300));
    let idle = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    assert_eq!(thread.join().unwrap(), Waited::Expired);
    let took = started.elapsed();
    assert!(took < Duration::from_secs(3), "{took:?}");
    drop(idle);
    assert!(closed(port));
}

/// rv-code2 CTA10-1: a connection that sends a byte now and then has one budget for its
/// head and the drain after its answer, never more than the time left, so the callback
/// still closes at its limit while it drips.
#[test]
fn a_connection_that_drips_bytes_is_closed_at_its_budget_and_the_wait_still_expires() {
    let started = Instant::now();
    let (port, _stop, thread) = waiting(Duration::from_millis(300));
    let drip = dripping(port, Duration::from_secs(3));
    assert_eq!(thread.join().unwrap(), Waited::Expired);
    let took = started.elapsed();
    assert!(took < Duration::from_secs(2), "{took:?}");
    assert!(closed(port));
    drip.join().unwrap();
}

/// rv-code2 CTA10-1: a cancel ends the wait at once even while a connection drips bytes, so
/// a local program cannot hold a cancelled sign-in's thread and port.
#[test]
fn a_cancel_ends_the_wait_while_a_connection_drips_bytes() {
    let (port, stop, thread) = waiting(WAIT);
    let drip = dripping(port, Duration::from_secs(10));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!thread.is_finished(), "the drip ended the wait");
    let stopped = Instant::now();
    stop.store(true, Ordering::SeqCst);
    assert_eq!(thread.join().unwrap(), Waited::Stopped);
    let took = stopped.elapsed();
    assert!(took < Duration::from_secs(2), "{took:?}");
    assert!(closed(port));
    drip.join().unwrap();
}

/// DEC-168 addendum 30 decision 6: an answer with the sign-in's state and an `error` ends
/// the wait as refused, and the browser reads that the sign-in did not finish.
#[test]
fn an_answer_with_the_state_and_an_error_ends_the_wait_refused() {
    let (port, _stop, thread) = waiting(WAIT);
    let target = format!("/callback?state={STATE}&error=access_denied&code=c1");
    let (status, body) = ask(port, &get(&target));
    assert_eq!((status, body.as_str()), (200, FAILED_PAGE));
    assert_eq!(thread.join().unwrap(), Waited::Refused);
    assert!(closed(port));
}

/// DEC-168 addendum 30 decision 6: `REFUSED_MAX` refused requests close the callback, so a
/// local program can keep it busy for no longer than that.
#[test]
fn a_flood_of_refused_requests_closes_the_callback() {
    let (port, _stop, thread) = waiting(WAIT);
    for _ in 0..REFUSED_MAX {
        assert_eq!(ask(port, &get("/callback?state=wrong&code=c1")).0, 400);
    }
    assert_eq!(thread.join().unwrap(), Waited::Flooded);
    assert!(closed(port));
}

/// DEC-168 addendum 30, update 22:58: a connection that sends nothing and closes gets no
/// answer at all, so a browser's spare connection never reads a refusal, and the wait goes
/// on for the real answer.
#[test]
fn a_connection_that_sends_nothing_gets_no_answer_and_the_wait_goes_on() {
    let (port, _stop, thread) = waiting(WAIT);
    let mut silent = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    silent.set_read_timeout(Some(WAIT)).unwrap();
    silent.shutdown(std::net::Shutdown::Write).unwrap();
    let mut answer = Vec::new();
    let _ = silent.read_to_end(&mut answer);
    assert!(answer.is_empty(), "{}", String::from_utf8_lossy(&answer));
    assert!(!thread.is_finished(), "a silent connection ended the wait");
    let target = format!("/callback?state={STATE}&code=c1");
    assert_eq!(ask(port, &get(&target)), (200, DONE_PAGE.to_owned()));
    let code = "c1".to_owned();
    assert_eq!(thread.join().unwrap(), Waited::Code { code, iss: None });
}

/// DEC-168 addendum 30 decision 5: a cancelled sign-in stops its wait at once and closes the
/// port, so a later answer reaches nothing.
#[test]
fn a_stopped_wait_ends_at_once_and_closes_the_port() {
    let (port, stop, thread) = waiting(WAIT);
    let started = Instant::now();
    stop.store(true, Ordering::SeqCst);
    assert_eq!(thread.join().unwrap(), Waited::Stopped);
    let took = started.elapsed();
    assert!(took < Duration::from_secs(2), "{took:?}");
    assert!(closed(port));
}

/// rv-ds241 23:13 nit: an answer's code is a one-time secret, so the debug text of a wait
/// names its issuer and never its code, and a log or a failed row cannot hold it.
#[test]
fn the_debug_text_of_an_answer_never_holds_its_code() {
    let iss = Some("https://auth.example".to_owned());
    let code = "code-c0ffee-5".to_owned();
    let shown = format!("{:?}", Waited::Code { code, iss });
    assert!(!shown.contains("c0ffee"), "{shown}");
    assert!(shown.contains("Code") && shown.contains("https://auth.example"), "{shown}");
    assert_eq!(format!("{:?}", Waited::Expired), "Expired");
}

/// DEC-168 addendum 30 decision 1: the app's client metadata document names the app, the
/// loopback callback with no port, the code and refresh grants and no client secret. It
/// carries no client id until the build has the address it is served at, then that one.
#[test]
fn the_client_metadata_document_matches_the_sign_in_client() {
    let document: Value = serde_json::from_str(CLIENT_METADATA).unwrap();
    assert_eq!(document["client_name"], json!("CrazyAgent"));
    let redirects = json!(["http://127.0.0.1/callback"]);
    assert_eq!(document["redirect_uris"], redirects);
    let grants = json!(["authorization_code", "refresh_token"]);
    assert_eq!(document["grant_types"], grants);
    assert_eq!(document["response_types"], json!(["code"]));
    assert_eq!(document["token_endpoint_auth_method"], json!("none"));
    let redirect = url::Url::parse(document["redirect_uris"][0].as_str().unwrap()).unwrap();
    assert_eq!(redirect.path(), CALLBACK_PATH);
    assert_eq!(redirect.port(), None);
    match crate::connectors::oauth::CLIENT_METADATA_URL {
        None => assert!(document.get("client_id").is_none(), "{document}"),
        Some(url) => assert_eq!(document["client_id"], json!(url)),
    }
    let fields = document.as_object().unwrap().len();
    let expected = 5 + usize::from(crate::connectors::oauth::CLIENT_METADATA_URL.is_some());
    assert_eq!(fields, expected, "{document}");
}
