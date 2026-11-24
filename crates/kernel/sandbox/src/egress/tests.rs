use super::*;
use kernel_security::error::StoreError;
use kernel_security::net::{CaptureRules, Egress, Method, Request, Requests};
use kernel_security::secret::SecretString;
use kernel_security::secrets::{Handles, Scopes, Secrets};
use kernel_security::store::{SecretRef, Store};
use std::sync::Mutex;

/// The host table of the allowlist rows.
const HOSTS: &[(&str, &[&str])] = &[
    (
        "github",
        &["github.com", "api.github.com", "ghe.example:8443"],
    ),
    ("local", &["127.0.0.1:8080", "[::1]:9000"]),
];

#[test]
fn a_host_outside_the_allowlist_is_refused() {
    let egress = Curl::with_hosts(HOSTS);
    let refused = Err(StoreError::code("net_host_refused"));
    assert_eq!(egress.allows("github", "https", "github.com", 443), Ok(()));
    assert_eq!(
        egress.allows("github", "https", "ghe.example", 8443),
        Ok(())
    );
    assert_eq!(egress.allows("local", "http", "127.0.0.1", 8080), Ok(()));
    assert_eq!(egress.allows("local", "http", "[::1]", 9000), Ok(()));
    assert_eq!(egress.allows("local", "http", "::1", 9000), refused);
    assert_eq!(
        egress.allows("github", "https", "evil.example", 443),
        refused
    );
    assert_eq!(
        egress.allows("github", "https", "github.com", 8443),
        refused
    );
    assert_eq!(
        egress.allows("github", "https", "ghe.example", 443),
        refused
    );
    assert_eq!(egress.allows("local", "https", "github.com", 443), refused);
    assert_eq!(egress.allows("notes", "https", "github.com", 443), refused);
    assert_eq!(egress.allows("local", "http", "127.0.0.1", 9090), refused);
}

/// A store the reply-header row never opens: the request it makes carries no secret.
struct Unopened;

impl Store for Unopened {
    fn put(&self, _: &str, _: &str, _: &SecretString) -> Result<SecretRef, StoreError> {
        unreachable!("a request with no secret needs no store call")
    }

    fn get(&self, _: &SecretRef) -> Result<SecretString, StoreError> {
        unreachable!("a request with no secret needs no store call")
    }

    fn delete(&self, _: &SecretRef) -> Result<(), StoreError> {
        unreachable!("a request with no secret needs no store call")
    }

    fn git_credential_helper(&self, _: &SecretRef, _: &str) -> Result<String, StoreError> {
        unreachable!("a request with no secret needs no store call")
    }
}

#[test]
fn a_reply_header_other_than_the_scopes_is_refused_before_curl() {
    let scopes = Scopes::new(&[("github", "github")]).unwrap();
    let handles = Mutex::new(Handles::new());
    let secrets = Secrets::for_request(
        &Unopened,
        "dev.crazyagent.e0123456789ab",
        &scopes,
        &handles,
        None,
    );
    let rules = CaptureRules::new(&[]).unwrap();
    let egress = Curl::with_hosts(&[("github", &["127.0.0.1:9"])]);
    let requests = Requests::new(&secrets, &egress, &rules);
    let refused = requests.request(
        "github",
        Request {
            method: Method::Get,
            url: "http://127.0.0.1:9/v1".to_owned(),
            headers: Vec::new(),
            bearer: None,
            form: Vec::new(),
            secret_form: Vec::new(),
            json: None,
            reply_headers: vec!["X-OAuth-Scopes".to_owned(), "set-cookie".to_owned()],
            capture: Vec::new(),
        },
    );
    assert_eq!(
        refused,
        Err(StoreError::code("net_reply_header_unsupported"))
    );
}

/// A stand-in for curl: a shell that reads its whole config from stdin, then prints the
/// given output and exits with the given code. Inputs: the output and the exit code.
/// Output: the command, piped as [`exchange`] expects.
#[cfg(unix)]
fn stand_in(output: &str, exit: u8) -> Command {
    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            "cat >/dev/null; printf '%s' \"$1\"; exit \"$2\"",
            "sh",
        ])
        .args([output, &exit.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

#[cfg(unix)]
#[test]
fn an_exchange_reads_the_body_scopes_and_status() {
    let reply = exchange(
        stand_in("hello\nrepo, read:org\n201", 0),
        b"url = x\n".to_vec(),
    )
    .unwrap();
    assert_eq!(reply.status, 201);
    assert_eq!(reply.body, b"hello");
    assert_eq!(
        reply.scopes,
        Some(vec!["repo".to_owned(), "read:org".to_owned()])
    );
}

#[cfg(unix)]
#[test]
fn an_exchange_whose_output_is_not_a_reply_is_reply_invalid() {
    for output in ["no status", "body\n\n99", "body\n\nabc"] {
        assert_eq!(
            exchange(stand_in(output, 0), b"url = x\n".to_vec()).err(),
            Some(ExchangeError::ReplyInvalid),
            "{output:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn an_exchange_that_does_not_finish_is_unreachable() {
    assert_eq!(
        exchange(stand_in("body\n\n200", 7), b"url = x\n".to_vec()).err(),
        Some(ExchangeError::Unreachable)
    );
    let mut missing = Command::new("/nonexistent/curl");
    missing.stdin(Stdio::piped()).stdout(Stdio::piped());
    assert_eq!(
        exchange(missing, b"url = x\n".to_vec()).err(),
        Some(ExchangeError::Unreachable)
    );
}

#[test]
fn each_exchange_failure_maps_to_its_net_code() {
    assert_eq!(net_code(ExchangeError::Unreachable), "net_unreachable");
    assert_eq!(net_code(ExchangeError::ReplyInvalid), "net_reply_invalid");
}

/// A plugin request's lookup that never answers is `net_host_unresolved` once the lookup
/// bound passes. The lookup runs on its own thread so a lookup with no bound fails this
/// row instead of hanging it.
#[test]
fn a_plugin_lookup_that_never_answers_is_unresolved_within_the_bound() {
    let (_held, never) = std::sync::mpsc::channel::<()>();
    let (done, finished) = std::sync::mpsc::channel();
    let started = Instant::now();
    std::thread::spawn(move || {
        let found = resolve_with("api.github.com", 443, move |_, _| {
            let _ = never.recv();
            Ok(Vec::new())
        });
        let _ = done.send(found.map_err(|error| error.0));
    });
    let margin = Duration::from_secs(5);
    let answer = finished.recv_timeout(LOOKUP_TIME + margin);
    assert_eq!(answer, Ok(Err("net_host_unresolved".to_owned())));
    assert!(started.elapsed() < LOOKUP_TIME + margin);
}

/// While as many lookups as the cap are still running past their bound, a new lookup is
/// refused at once as a timeout, and lookups answer again once those end.
#[test]
fn stalled_lookups_at_the_cap_refuse_a_new_lookup_until_they_end() {
    let public = |_: &str, _: u16| Ok(vec![IpAddr::from([140, 82, 112, 4])]);
    let mut held = Vec::new();
    let mut calls = Vec::new();
    for _ in 0..LOOKUPS_MAX {
        let (hold, never) = std::sync::mpsc::channel::<()>();
        held.push(hold);
        calls.push(std::thread::spawn(move || {
            lookup_within("stalled.example", 443, move |_, _| {
                let _ = never.recv();
                Ok(Vec::new())
            })
            .map_err(|error| error.kind())
        }));
    }
    for call in calls {
        assert_eq!(call.join().unwrap(), Err(std::io::ErrorKind::TimedOut));
    }
    let started = Instant::now();
    let refused = lookup_within("api.github.com", 443, public).map_err(|error| error.kind());
    assert_eq!(refused, Err(std::io::ErrorKind::TimedOut));
    assert!(started.elapsed() < LOOKUP_TIME);
    drop(held);
    let answered = loop {
        match lookup_within("api.github.com", 443, public) {
            Ok(found) => break found,
            Err(_) if started.elapsed() < LOOKUP_TIME * 2 => std::thread::yield_now(),
            Err(error) => panic!("{error}"),
        }
    };
    assert_eq!(answered, vec![IpAddr::from([140, 82, 112, 4])]);
}
