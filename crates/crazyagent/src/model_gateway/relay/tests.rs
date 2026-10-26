use super::*;
use std::ffi::OsStr;
use std::io::Cursor;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Instant;

const KEY: &str = "sk-test-relay-0123456789abcdef";
const WAIT: Duration = Duration::from_secs(20);

fn key() -> SecretString {
    SecretString::new(KEY.as_bytes().to_vec()).unwrap()
}

/// Reads one whole HTTP/1.x request the way the relay does, head then body.
/// Input: the reader positioned at the request line.
/// Output: the request, or the HTTP status the relay would refuse it with.
fn read_request(reader: &mut impl BufRead) -> Result<Incoming, u16> {
    let mut incoming = read_head(reader)?;
    incoming.body = read_body(reader, &incoming.headers)?;
    Ok(incoming)
}

/// A real HTTP/1.1 provider stand-in on 127.0.0.1 that records every request it gets,
/// parsed with the relay's own request reader, and answers through the given handler.
struct Provider {
    port: u16,
    seen: Arc<Mutex<Vec<Incoming>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Provider {
    fn start(mut answer: impl FnMut(&Incoming, &mut TcpStream) + Send + 'static) -> Self {
        let listener = crate::spawn_gate::bind_tcp_std(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (recorded, stopping) = (Arc::clone(&seen), Arc::clone(&stop));
        let thread = std::thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = crate::spawn_gate::accept_tcp_std(&listener) else {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(WAIT)).unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let incoming = read_request(&mut reader).unwrap();
                answer(&incoming, &mut stream);
                recorded.lock().unwrap().push(incoming);
                let _ = stream.shutdown(Shutdown::Both);
            }
        });
        Self {
            port,
            seen,
            stop,
            thread: Some(thread),
        }
    }

    fn json() -> Self {
        Self::start(|_, stream| {
            let body = br#"{"ok":true}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        })
    }

    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn seen(&self) -> std::sync::MutexGuard<'_, Vec<Incoming>> {
        self.seen.lock().unwrap()
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn header<'a>(incoming: &'a Incoming, name: &str) -> Option<&'a str> {
    incoming
        .headers
        .iter()
        .find(|(found, _)| found == name)
        .map(|(_, value)| value.as_str())
}

/// Sends one raw request to the relay and reads the whole reply.
/// Output: the status code, the header block and the body.
fn send(port: u16, raw: &[u8]) -> (u16, String, Vec<u8>) {
    let mut stream = crate::spawn_gate::connect_tcp_std(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream.write_all(raw).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    let split = reply
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap();
    let head = String::from_utf8(reply[..split].to_vec()).unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, head, reply[split + 4..].to_vec())
}

fn post(target: &str, auth: &str, body: &str) -> Vec<u8> {
    format!(
        "POST {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}\r\nanthropic-version: 2023-06-01\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

#[test]
fn placeholders_are_the_local_value_or_a_relay_token() {
    assert!(is_placeholder(LOCAL_PLACEHOLDER));
    assert!(is_placeholder(&format!(
        "{TOKEN_PREFIX}{}",
        "a1".repeat(32)
    )));
    assert!(!is_placeholder(&format!(
        "{TOKEN_PREFIX}{}",
        "A1".repeat(32)
    )));
    assert!(!is_placeholder(&format!(
        "{TOKEN_PREFIX}{}",
        "a1".repeat(31)
    )));
    assert!(!is_placeholder(&format!(
        "{TOKEN_PREFIX}{}g",
        "a1".repeat(31)
    )));
    assert!(!is_placeholder(KEY));
    assert!(!is_placeholder(""));
}

#[test]
fn upstreams_are_https_or_loopback_http_without_credentials() {
    let openai = upstream("https://api.openai.com/v1/").unwrap();
    assert_eq!(openai.origin, "https://api.openai.com");
    assert_eq!(openai.path, "/v1");
    assert_eq!(openai.scheme, "https");
    let anthropic = upstream("https://api.anthropic.com").unwrap();
    assert_eq!(anthropic.path, "");
    assert_eq!(upstream("http://127.0.0.1:8080/v1").unwrap().scheme, "http");
    assert_eq!(upstream("http://localhost:8080").unwrap().scheme, "http");
    for refused in [
        "http://api.openai.com/v1",
        "http://192.168.1.10:8080",
        "https://user:pass@api.openai.com/v1",
        "https://api.openai.com/v1?key=x",
        "https://api.openai.com/v1#x",
        "ftp://127.0.0.1/v1",
        "file:///etc/passwd",
        "not a url",
    ] {
        assert_eq!(
            upstream(refused).unwrap_err().0,
            "model_relay_upstream_invalid",
            "{refused}"
        );
    }
    assert!(Relay::start("http://api.openai.com/v1", key()).is_err());
}

#[test]
fn x_api_key_placeholder_is_replaced_by_the_real_key() {
    let provider = Provider::json();
    let relay = Relay::start(&provider.origin(), key()).unwrap();
    assert!(is_placeholder(relay.token()));
    assert_eq!(
        relay.base_url(),
        format!("http://127.0.0.1:{}", relay.port())
    );
    let (status, head, body) = send(
        relay.port(),
        &post(
            "/v1/messages",
            &format!("x-api-key: {}", relay.token()),
            r#"{"model":"m"}"#,
        ),
    );
    assert_eq!(status, 200);
    assert!(head.contains("Connection: close"), "{head}");
    assert_eq!(body, br#"{"ok":true}"#);
    let seen = provider.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].target, "/v1/messages");
    assert_eq!(header(&seen[0], "x-api-key"), Some(KEY));
    assert_eq!(header(&seen[0], "authorization"), None);
    assert_eq!(header(&seen[0], "anthropic-version"), Some("2023-06-01"));
    assert_eq!(seen[0].body, br#"{"model":"m"}"#);
    assert!(
        seen[0]
            .headers
            .iter()
            .all(|(_, value)| !value.contains(relay.token()))
    );
}

#[test]
fn bearer_placeholder_is_replaced_in_the_same_form() {
    let provider = Provider::json();
    let relay = Relay::start(&format!("{}/v1", provider.origin()), key()).unwrap();
    assert_eq!(
        relay.base_url(),
        format!("http://127.0.0.1:{}/v1", relay.port())
    );
    let request = format!(
        "GET /v1/models HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\n\r\n",
        relay.token()
    );
    let (status, _, _) = send(relay.port(), request.as_bytes());
    assert_eq!(status, 200);
    let seen = provider.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "GET");
    assert_eq!(seen[0].target, "/v1/models");
    assert_eq!(
        header(&seen[0], "authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    assert_eq!(header(&seen[0], "x-api-key"), None);
}

#[test]
fn both_headers_from_a_key_helper_forward_only_the_real_x_api_key() {
    let provider = Provider::json();
    let relay = Relay::start(&provider.origin(), key()).unwrap();
    let auth = format!(
        "Authorization: Bearer {token}\r\nX-Api-Key: {token}",
        token = relay.token()
    );
    assert_eq!(
        send(relay.port(), &post("/v1/messages", &auth, "{}")).0,
        200
    );
    let seen = provider.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(header(&seen[0], "x-api-key"), Some(KEY));
    assert_eq!(header(&seen[0], "authorization"), None);
}

#[test]
fn wrong_or_missing_token_is_refused_without_reaching_the_provider() {
    let provider = Provider::json();
    let relay = Relay::start(&provider.origin(), key()).unwrap();
    let other = format!("{TOKEN_PREFIX}{}", "0".repeat(64));
    for auth in [
        "accept: */*".to_owned(),
        format!("x-api-key: {other}"),
        format!("x-api-key: {KEY}"),
        format!("Authorization: Bearer {KEY}"),
        format!("Authorization: Basic {}", relay.token()),
    ] {
        let (status, _, body) = send(relay.port(), &post("/v1/messages", &auth, "{}"));
        assert_eq!(status, 401, "{auth}");
        assert!(!String::from_utf8_lossy(&body).contains(KEY));
    }
    assert!(provider.seen().is_empty());
}

#[test]
fn a_request_without_the_token_is_refused_before_its_body_is_read() {
    let provider = Provider::json();
    let relay = Relay::start(&provider.origin(), key()).unwrap();
    let answer = Duration::from_secs(10);
    let announced = [
        format!("Content-Length: {BODY_BYTES_MAX}"),
        "Transfer-Encoding: chunked".to_owned(),
    ];
    for framing in &announced {
        let mut stream = crate::spawn_gate::connect_tcp_std(("127.0.0.1", relay.port())).unwrap();
        stream.set_read_timeout(Some(answer)).unwrap();
        let head = format!("POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\n{framing}\r\n\r\n");
        stream.write_all(head.as_bytes()).unwrap();
        let started = Instant::now();
        let mut reply = Vec::new();
        let read = stream.read_to_end(&mut reply);
        assert!(
            read.is_ok(),
            "{framing}: the relay must answer before any body byte arrives, got {read:?} after {:?}",
            started.elapsed()
        );
        let reply = String::from_utf8_lossy(&reply);
        assert!(reply.starts_with("HTTP/1.1 401 "), "{framing}: {reply}");
    }

    let mut stream = crate::spawn_gate::connect_tcp_std(("127.0.0.1", relay.port())).unwrap();
    stream.set_read_timeout(Some(answer)).unwrap();
    let head = format!(
        "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nx-api-key: {}\r\nContent-Length: {}\r\n\r\n",
        relay.token(),
        BODY_BYTES_MAX + 1
    );
    stream.write_all(head.as_bytes()).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    let reply = String::from_utf8_lossy(&reply);
    assert!(
        reply.starts_with("HTTP/1.1 413 "),
        "an authenticated body keeps its bound: {reply}"
    );
    assert!(provider.seen().is_empty());
}

/// Callers without the token that fill every slot with request heads they never finish
/// must not starve the agent: its tokened request still reaches the provider, and
/// dropping the relay closes every connection still held open.
#[test]
fn callers_without_the_token_cannot_hold_the_slots_the_agent_needs() {
    let provider = Provider::json();
    let relay = Relay::start(&provider.origin(), key()).unwrap();
    let mut held: Vec<TcpStream> = (0..CONNECTIONS_MAX)
        .map(|_| {
            let mut stream =
                crate::spawn_gate::connect_tcp_std(("127.0.0.1", relay.port())).unwrap();
            stream
                .write_all(b"POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\n")
                .unwrap();
            stream
        })
        .collect();
    let (status, _, body) = send(
        relay.port(),
        &post(
            "/v1/messages",
            &format!("x-api-key: {}", relay.token()),
            "{}",
        ),
    );
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(provider.seen().len(), 1);
    drop(relay);
    for stream in &mut held {
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let read = stream.read_to_end(&mut Vec::new());
        assert!(
            !matches!(&read, Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)),
            "a held connection must close when the relay stops: {read:?}"
        );
    }
}

#[test]
fn paths_outside_the_provider_base_and_other_methods_are_refused() {
    let provider = Provider::json();
    let relay = Relay::start(&format!("{}/v1", provider.origin()), key()).unwrap();
    let auth = format!("Authorization: Bearer {}", relay.token());
    for target in ["/v2/models", "/v1x", "/v1/../admin", "/v1/./models", "/"] {
        let (status, _, _) = send(relay.port(), &post(target, &auth, "{}"));
        assert_eq!(status, 404, "{target}");
    }
    let delete = format!("DELETE /v1/files/a HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}\r\n\r\n");
    assert_eq!(send(relay.port(), delete.as_bytes()).0, 405);
    assert!(provider.seen().is_empty());
}

#[test]
fn percent_encoded_dot_segments_are_refused() {
    let provider = Provider::json();
    let relay = Relay::start(&format!("{}/v1", provider.origin()), key()).unwrap();
    let auth = format!("Authorization: Bearer {}", relay.token());
    for target in [
        "/v1/%2e%2e/admin",
        "/v1/%2E%2E/admin",
        "/v1/%2E%2e/admin",
        "/v1/.%2e/admin",
        "/v1/%2e./admin",
        "/v1/%2e/models",
        "/v1/x/%2E%2e%2fadmin",
        "/v1/x%2f%2e%2e%2f%2e%2e/admin",
        "/v1/x/..%5cadmin",
        "/v1/%252e%252e/admin",
        "/v1/%25252525252e/admin",
    ] {
        let (status, _, _) = send(relay.port(), &post(target, &auth, "{}"));
        assert_eq!(status, 404, "{target}");
    }
    assert!(provider.seen().is_empty());
    let (status, _, _) = send(relay.port(), &post("/v1/models/a%2Db.c", &auth, "{}"));
    assert_eq!(status, 200);
    assert_eq!(provider.seen().len(), 1);
}

#[test]
fn chunked_request_bodies_reach_the_provider_whole() {
    let provider = Provider::json();
    let relay = Relay::start(&provider.origin(), key()).unwrap();
    let request = format!(
        "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nx-api-key: {}\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n7;ext=1\r\n, world\r\n0\r\n\r\n",
        relay.token()
    );
    assert_eq!(send(relay.port(), request.as_bytes()).0, 200);
    let seen = provider.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].body, b"hello, world");
    assert_eq!(header(&seen[0], "transfer-encoding"), None);
}

#[test]
fn streamed_replies_reach_the_harness_before_the_provider_finishes() {
    let (release, released) = mpsc::channel::<()>();
    let waited = Arc::new(AtomicBool::new(false));
    let waiting = Arc::clone(&waited);
    let provider = Provider::start(move |_, stream| {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: one\n\n")
            .unwrap();
        stream.flush().unwrap();
        if released.recv_timeout(WAIT).is_ok() {
            waiting.store(true, Ordering::Release);
        }
        stream.write_all(b"data: two\n\n").unwrap();
    });
    let relay = Relay::start(&provider.origin(), key()).unwrap();
    let mut stream = crate::spawn_gate::connect_tcp_std(("127.0.0.1", relay.port())).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream
        .write_all(&post(
            "/v1/messages",
            &format!("x-api-key: {}", relay.token()),
            r#"{"stream":true}"#,
        ))
        .unwrap();
    let started = Instant::now();
    let mut reply = Vec::new();
    let mut buffer = [0_u8; 4096];
    while !String::from_utf8_lossy(&reply).contains("data: one") {
        assert!(started.elapsed() < WAIT, "first event never arrived");
        let read = stream.read(&mut buffer).unwrap();
        assert_ne!(read, 0, "relay closed before the first event");
        reply.extend_from_slice(&buffer[..read]);
    }
    let first = String::from_utf8_lossy(&reply).into_owned();
    assert!(first.starts_with("HTTP/1.1 200 Relay\r\n"), "{first}");
    assert!(first.contains("text/event-stream"), "{first}");
    assert!(!first.contains("data: two"), "{first}");
    release.send(()).unwrap();
    stream.read_to_end(&mut reply).unwrap();
    assert!(String::from_utf8_lossy(&reply).ends_with("data: one\n\ndata: two\n\n"));
    drop(provider);
    assert!(
        waited.load(Ordering::Acquire),
        "provider finished before the harness saw the first event"
    );
}

#[test]
fn curl_arguments_and_environment_hold_no_key_url_or_token() {
    let target = upstream("https://api.example.test/v1").unwrap();
    let token = format!("{TOKEN_PREFIX}{}", "b".repeat(64));
    let incoming = Incoming {
        method: "POST".into(),
        target: "/v1/responses".into(),
        headers: vec![
            ("host".into(), "127.0.0.1:9".into()),
            ("authorization".into(), format!("Bearer {token}")),
            ("accept-encoding".into(), "gzip".into()),
            ("openai-beta".into(), "responses=v1".into()),
        ],
        body: b"{}".to_vec(),
    };
    let config = upstream_config(
        &target,
        &incoming,
        KeyHeader::Bearer,
        &key(),
        Some(Path::new("/tmp/crazyagent-relay-x/request-1")),
    )
    .unwrap();
    let config = String::from_utf8(config).unwrap();
    assert!(config.contains("url = \"https://api.example.test/v1/responses\"\n"));
    assert!(config.contains("request = \"POST\"\n"));
    assert!(config.contains(&format!("header = \"Authorization: Bearer {KEY}\"\n")));
    assert!(config.contains("header = \"openai-beta: responses=v1\"\n"));
    assert!(config.contains("data-binary = \"@/tmp/crazyagent-relay-x/request-1\"\n"));
    assert!(!config.contains(&token));
    assert!(!config.contains("gzip"));
    assert!(!config.contains("127.0.0.1:9"));
    let command = upstream_command(&target);
    assert_eq!(command.get_program(), CURL);
    let arguments: Vec<String> = command
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    assert!(
        arguments
            .windows(2)
            .any(|pair| pair == ["--proto", "=https"])
    );
    assert!(
        arguments
            .windows(2)
            .any(|pair| pair == ["--max-redirs", "0"])
    );
    assert!(arguments.windows(2).any(|pair| pair == ["--config", "-"]));
    assert_eq!(arguments[0], "-q");
    for argument in &arguments {
        assert!(!argument.contains(KEY) && !argument.contains("example"));
    }
    let environment: Vec<_> = command.get_envs().collect();
    assert_eq!(
        environment,
        [(OsStr::new("PATH"), Some(OsStr::new("/usr/bin:/bin")))]
    );
}

#[test]
fn request_limits_refuse_oversized_or_malformed_requests() {
    let read = |raw: &[u8]| read_request(&mut Cursor::new(raw.to_vec())).unwrap_err();
    assert_eq!(
        read(b"POST /v1 HTTP/1.1\r\nContent-Length: 67108865\r\n\r\n"),
        413
    );
    assert_eq!(
        read(b"POST /v1 HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n4000001\r\n"),
        413
    );
    assert_eq!(
        read(b"POST /v1 HTTP/1.1\r\nTransfer-Encoding: gzip\r\n\r\n"),
        501
    );
    assert_eq!(read(b"PUT /v1 HTTP/1.1\r\n\r\n"), 405);
    assert_eq!(read(b"GET http://evil.test/ HTTP/1.1\r\n\r\n"), 400);
    assert_eq!(read(b"GET /v1 HTTP/2\r\n\r\n"), 505);
    assert_eq!(read(b"GET /v1 HTTP/1.1\r\nBad Name: x\r\n\r\n"), 400);
    let mut many = b"GET /v1 HTTP/1.1\r\n".to_vec();
    for index in 0..=HEADERS_MAX {
        many.extend_from_slice(format!("x-h{index}: v\r\n").as_bytes());
    }
    many.extend_from_slice(b"\r\n");
    assert_eq!(read(&many), 400);
    let mut huge = b"GET /v1 HTTP/1.1\r\n".to_vec();
    for index in 0..8 {
        huge.extend_from_slice(format!("x-h{index}: {}\r\n", "v".repeat(10_000)).as_bytes());
    }
    huge.extend_from_slice(b"\r\n");
    assert_eq!(read(&huge), 431);
    let mut chunked = b"POST /v1 HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    for _ in 0..20_000 {
        chunked.extend_from_slice(b"1\r\na\r\n");
    }
    chunked.extend_from_slice(b"0\r\n\r\n");
    let incoming = read_request(&mut Cursor::new(chunked)).unwrap();
    assert_eq!(incoming.body.len(), 20_000);
}

#[test]
fn dropping_the_relay_stops_it_and_removes_its_scratch() {
    let provider = Provider::json();
    let relay = Relay::start(&provider.origin(), key()).unwrap();
    let scratch = relay.shared.scratch.clone();
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(&scratch).unwrap().permissions(),
    );
    assert_eq!(mode & 0o777, 0o700);
    let (status, _, _) = send(
        relay.port(),
        &post(
            "/v1/messages",
            &format!("x-api-key: {}", relay.token()),
            r#"{"body":"kept in a private file"}"#,
        ),
    );
    assert_eq!(status, 200);
    assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
    drop(relay);
    assert!(!scratch.exists());
}

#[test]
fn dropping_the_relay_aborts_a_provider_request_still_in_flight() {
    let (reached, arrived) = mpsc::channel::<()>();
    let (ended, fate) = mpsc::channel::<(bool, Instant)>();
    let provider = Provider::start(move |_, stream| {
        reached.send(()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut byte = [0_u8; 1];
        let closed = match stream.read(&mut byte) {
            Ok(read) => read == 0,
            Err(error) => !matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ),
        };
        ended.send((closed, Instant::now())).unwrap();
    });
    let relay = Relay::start(&provider.origin(), key()).unwrap();
    let shared = Arc::clone(&relay.shared);
    let mut harness = crate::spawn_gate::connect_tcp_std(("127.0.0.1", relay.port())).unwrap();
    harness.set_read_timeout(Some(WAIT)).unwrap();
    harness
        .write_all(&post(
            "/v1/messages",
            &format!("x-api-key: {}", relay.token()),
            r#"{"stream":true}"#,
        ))
        .unwrap();
    arrived
        .recv_timeout(WAIT)
        .expect("the provider never saw the relayed request");
    let cancelled = Instant::now();
    drop(relay);
    let stopped = Instant::now();
    assert!(stopped - cancelled < WAIT, "stopping the relay hung");
    let (closed, at) = fate.recv_timeout(WAIT).unwrap();
    assert!(
        closed,
        "the provider request outlived the stopped relay and still holds the key"
    );
    assert!(at <= stopped + Duration::from_millis(500));
    assert_eq!(
        Arc::strong_count(&shared),
        1,
        "a relay request thread outlived the stopped relay"
    );
    assert!(shared.open().is_empty());
    let mut rest = Vec::new();
    let _ = harness.read_to_end(&mut rest);
    assert!(!String::from_utf8_lossy(&rest).contains("200"));
    drop(provider);
}
