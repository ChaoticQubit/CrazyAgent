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
fn every_placeholder_fits_the_key_file_limit_and_a_relay_token_fills_it() {
    let longest = format!(
        "{TOKEN_PREFIX}{}",
        "f".repeat(KEY_BYTES_MAX - TOKEN_PREFIX.len())
    );
    assert!(is_placeholder(&longest));
    assert!(!is_placeholder(&format!("{longest}f")));
    assert!(LOCAL_PLACEHOLDER.len() <= KEY_BYTES_MAX);
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

/// Task 17 (DEC-180 addendum 2, answer 3): a harness on a Gemini API key account presents
/// the placeholder in `x-goog-api-key`, as Google's SDK does; the provider gets the real
/// key in that same header, once, beside no other key header, and a wrong token there is
/// refused before the provider is reached.
#[test]
fn a_gemini_placeholder_in_x_goog_api_key_is_replaced_by_the_real_key_in_the_same_header() {
    let provider = Provider::json();
    let relay = Relay::start(&provider.origin(), key()).unwrap();
    let target = "/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse";
    let request = format!(
        "POST {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nx-goog-api-key: {}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}",
        relay.token()
    );
    let (status, _, _) = send(relay.port(), request.as_bytes());
    assert_eq!(status, 200);
    let seen = provider.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].target, target);
    let keys: Vec<&str> = seen[0]
        .headers
        .iter()
        .filter(|(name, _)| name == "x-goog-api-key")
        .map(|(_, value)| value.as_str())
        .collect();
    assert_eq!(keys, [KEY]);
    assert_eq!(header(&seen[0], "x-api-key"), None);
    assert_eq!(header(&seen[0], "authorization"), None);
    assert!(
        seen[0]
            .headers
            .iter()
            .all(|(_, value)| !value.contains(relay.token()))
    );
    let other = format!("{TOKEN_PREFIX}{}", "0".repeat(64));
    for wrong in [other.as_str(), KEY] {
        let request = format!(
            "GET /v1beta/models HTTP/1.1\r\nHost: 127.0.0.1\r\nx-goog-api-key: {wrong}\r\n\r\n"
        );
        assert_eq!(send(relay.port(), request.as_bytes()).0, 401, "{wrong}");
    }
    assert_eq!(provider.seen().len(), 1);
}

/// Task 17: when a harness sends the placeholder in `x-goog-api-key` beside the header the
/// relay keys the request by, the placeholder in `x-goog-api-key` is dropped, never
/// forwarded to the provider next to the real key; `x-api-key` and Bearer keep their order.
#[test]
fn a_placeholder_in_x_goog_api_key_beside_another_key_header_is_never_forwarded() {
    let provider = Provider::json();
    let relay = Relay::start(&provider.origin(), key()).unwrap();
    for (auth, kept, value) in [
        (
            format!(
                "X-Api-Key: {token}\r\nx-goog-api-key: {token}",
                token = relay.token()
            ),
            "x-api-key",
            KEY.to_owned(),
        ),
        (
            format!(
                "Authorization: Bearer {token}\r\nx-goog-api-key: {token}",
                token = relay.token()
            ),
            "authorization",
            format!("Bearer {KEY}"),
        ),
    ] {
        assert_eq!(
            send(relay.port(), &post("/v1/messages", &auth, "{}")).0,
            200,
            "{auth}"
        );
        let seen = provider.seen();
        let last = seen.last().unwrap();
        assert_eq!(header(last, kept), Some(value.as_str()), "{auth}");
        assert_eq!(header(last, "x-goog-api-key"), None, "{auth}");
        assert!(
            last.headers
                .iter()
                .all(|(_, value)| !value.contains(relay.token()))
        );
    }
    assert_eq!(provider.seen().len(), 2);
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
        Some(Path::new("/dev/fd/7")),
    )
    .unwrap();
    let config = String::from_utf8(config).unwrap();
    assert!(config.contains("url = \"https://api.example.test/v1/responses\"\n"));
    assert!(config.contains("request = \"POST\"\n"));
    assert!(config.contains(&format!("header = \"Authorization: Bearer {KEY}\"\n")));
    assert!(config.contains("header = \"openai-beta: responses=v1\"\n"));
    assert!(config.contains("data-binary = \"@/dev/fd/7\"\n"));
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
fn dropping_the_relay_after_a_request_stops_it() {
    let provider = Provider::json();
    let relay = Relay::start(&provider.origin(), key()).unwrap();
    let (status, _, _) = send(
        relay.port(),
        &post(
            "/v1/messages",
            &format!("x-api-key: {}", relay.token()),
            r#"{"body":"handed to curl on a pipe"}"#,
        ),
    );
    assert_eq!(status, 200);
    assert_eq!(
        provider.seen()[0].body,
        br#"{"body":"handed to curl on a pipe"}"#
    );
    drop(relay);
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

/// SA241 vid 9: a relay listens on a port of the reserved relay block, so the port it
/// leaves when it ends is one no sandboxed agent can bind; relays running at once each hold
/// a port of their own there.
#[test]
fn a_relay_listens_only_on_a_reserved_relay_port() {
    let provider = Provider::json();
    let relays: Vec<Relay> = (0..3)
        .map(|_| Relay::start(&provider.origin(), key()).unwrap())
        .collect();
    let mut ports: Vec<u16> = relays.iter().map(Relay::port).collect();
    for port in &ports {
        assert!(
            crate::sandbox::model_proxy::RELAY_PORTS.contains(port),
            "{port}"
        );
    }
    ports.sort_unstable();
    ports.dedup();
    assert_eq!(ports.len(), relays.len());
}

/// SA241 vid 9: no Open-network agent listens on a port of the relay block, at either end
/// of it, on IPv4 or IPv6 loopback or any address, so none can take a relay's port after
/// the relay ended and answer a kept pane's next model request.
#[cfg(target_os = "macos")]
#[test]
fn an_open_agent_never_listens_on_a_model_relay_port() {
    let agent = crate::sandbox::test_agent::TestAgent::open();
    let block = crate::sandbox::model_proxy::RELAY_PORTS;
    for port in [*block.start(), *block.end()] {
        for host in ["127.0.0.1", "::1", "0.0.0.0", "::"] {
            let output = agent.run(&format!(
                "/usr/bin/nc -l {host} {port} 2>&1 & pid=$!; /bin/sleep 1; kill $pid 2>/dev/null; wait $pid; exit 0"
            ));
            let said = String::from_utf8_lossy(&output.stdout);
            assert!(
                said.contains("Operation not permitted"),
                "an agent cannot take {host}:{port} from a model relay: {said}"
            );
        }
    }
}

/// A relay port the system keeps from this user, as a Windows excluded port range answers
/// a bind with `PermissionDenied`, or one it cannot give (`AddrNotAvailable`), is skipped
/// as a taken one is, so the relay binds the next port of its block; any other bind error
/// still ends the start, and a block with no port left says every port is taken (rv-code4
/// relay-port r1 F1).
#[test]
fn a_relay_port_the_system_refuses_is_skipped_for_the_next_one() {
    use std::io::{Error, ErrorKind};
    let ports = [41_428, 41_429, 41_430, 41_431];
    let asked = Mutex::new(Vec::new());
    let refusing = |address: SocketAddr| -> std::io::Result<TcpListener> {
        asked.lock().unwrap().push(address.port());
        match address.port() {
            41_428 => Err(Error::from(ErrorKind::PermissionDenied)),
            41_429 => Err(Error::from(ErrorKind::AddrNotAvailable)),
            41_430 => Err(Error::from(ErrorKind::AddrInUse)),
            _ => crate::spawn_gate::bind_tcp_std("127.0.0.1:0"),
        }
    };
    let bound = first_bound(&ports, 0, refusing).unwrap();
    assert!(bound.local_addr().unwrap().ip().is_loopback());
    assert_eq!(*asked.lock().unwrap(), ports.to_vec());
    let excluded = first_bound(&ports, 1, |_| Err(Error::from(ErrorKind::PermissionDenied)));
    assert_eq!(excluded.unwrap_err().kind(), ErrorKind::AddrInUse);
    let broken = first_bound(&ports, 2, |_| Err(Error::other("no sockets")));
    assert_eq!(broken.unwrap_err().kind(), ErrorKind::Other);
}

/// The files a request body could have been left in where an agent's temp folder grant
/// reaches: every file directly in the temp folder or one folder below it, changed at or
/// after `since`, that holds `marker`. Inputs: when the row started and the body's marker.
/// Output: the paths.
fn temp_files_holding(since: std::time::SystemTime, marker: &[u8]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut folders = vec![(std::env::temp_dir(), 0)];
    while let Some((folder, depth)) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = path.symlink_metadata() else {
                continue;
            };
            if meta.is_dir() && depth == 0 {
                folders.push((path, depth + 1));
            } else if meta.is_file()
                && meta.len() <= 1 << 20
                && meta.modified().is_ok_and(|at| at >= since)
                && std::fs::read(&path)
                    .is_ok_and(|bytes| bytes.windows(marker.len()).any(|seen| seen == marker))
            {
                found.push(path);
            }
        }
    }
    found
}

/// SA vid 201: a keyed agent's request body never lands in the temp folder, which every
/// agent of a project holding the owner's temp folder grant reads and writes, so no agent
/// of another project can read or rewrite that model request: while the provider holds the
/// request, no file in the temp folder holds its body.
#[test]
fn a_request_body_never_lands_in_the_temp_folder_an_agent_grant_reaches() {
    let marker = format!("vid201-{}", random_hex(16).unwrap());
    let since = std::time::SystemTime::now();
    let during = Arc::new(Mutex::new(None));
    let (looked, probe) = (Arc::clone(&during), marker.clone());
    let provider = Provider::start(move |_, stream| {
        *looked.lock().unwrap() = Some(temp_files_holding(since, probe.as_bytes()));
        let body = br#"{"ok":true}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
    });
    let relay = Relay::start(&provider.origin(), key()).unwrap();
    let body = format!(r#"{{"messages":[{{"role":"user","content":"{marker}"}}]}}"#);
    let (status, _, _) = send(
        relay.port(),
        &post(
            "/v1/messages",
            &format!("x-api-key: {}", relay.token()),
            &body,
        ),
    );
    assert_eq!(status, 200);
    assert_eq!(provider.seen()[0].body, body.as_bytes());
    let held_in = during
        .lock()
        .unwrap()
        .take()
        .expect("the provider got the request");
    assert!(held_in.is_empty(), "the body sat in {held_in:?}");
}

/// A body longer than the longest line curl reads from its config (10 MB) still reaches the
/// provider byte for byte, so the body is never written into curl's config.
#[test]
fn a_body_longer_than_a_curl_config_line_reaches_the_provider_whole() {
    let provider = Provider::json();
    let relay = Relay::start(&provider.origin(), key()).unwrap();
    let body = format!(r#"{{"text":"{}"}}"#, "a\\nb".repeat(3 << 20));
    assert!(body.len() > 10 << 20);
    let (status, _, _) = send(
        relay.port(),
        &post(
            "/v1/messages",
            &format!("x-api-key: {}", relay.token()),
            &body,
        ),
    );
    assert_eq!(status, 200);
    let seen = provider.seen();
    assert!(
        seen[0].body == body.as_bytes(),
        "the provider got {} of {} bytes, or changed ones",
        seen[0].body.len(),
        body.len()
    );
}

const ACCESS: &str = "chatgpt-access-0123456789abcdef";
const FRESH: &str = "chatgpt-fresh-0123456789abcdef";
const ACCOUNT: &str = "acct-0123456789";
const CODEX_PATH: &str = "/backend-api/codex";

fn sign_in(access: &str, account_id: Option<&str>, expires: Duration) -> chatgpt::ChatGptTokens {
    chatgpt::ChatGptTokens {
        access: SecretString::bearer(access.as_bytes().to_vec()).unwrap(),
        account_id: account_id.map(str::to_owned),
        expires,
    }
}

fn epoch_now() -> Duration {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
}

/// A ChatGPT source for the relay rows whose renewal counts its calls and answers with what
/// `answer` makes. Inputs: the held sign-in and the answer. Output: the call count and the
/// source.
fn counted_source(
    held: chatgpt::ChatGptTokens,
    answer: impl Fn() -> Result<chatgpt::ChatGptTokens, EngineError> + Send + Sync + 'static,
) -> (Arc<AtomicUsize>, Arc<chatgpt::ChatGptSource>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&calls);
    let renew: chatgpt::Renew = Box::new(move |_: Duration, _: &dyn Fn() -> bool| {
        counting.fetch_add(1, Ordering::SeqCst);
        answer()
    });
    (calls, Arc::new(chatgpt::ChatGptSource::new(held, renew)))
}

fn lasting() -> Duration {
    epoch_now() + Duration::from_secs(3600)
}

fn chatgpt_relay(provider: &Provider, source: Arc<chatgpt::ChatGptSource>) -> Relay {
    Relay::start_chatgpt_at(&format!("{}{CODEX_PATH}", provider.origin()), source).unwrap()
}

fn responses(auth: &str, extra: &str, body: &str) -> Vec<u8> {
    format!(
        "POST {CODEX_PATH}/responses HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}\r\n{extra}Accept: text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn named<'a>(incoming: &'a Incoming, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    incoming
        .headers
        .iter()
        .filter(move |(found, _)| found == name)
        .map(|(_, value)| value.as_str())
}

/// T22 R1: a ChatGPT relay forwards a Responses request with the sign-in's access token as
/// `Authorization: Bearer` and its account id as `chatgpt-account-id`, whichever header
/// carried the placeholder; the account id, cookies, organization, project and every
/// `chatgpt-` or `x-openai-` header the harness sent never reach the backend, while its
/// other headers and the body pass unchanged.
#[test]
fn a_chatgpt_relay_forwards_with_the_sign_in_token_and_account_id_in_place_of_the_placeholder() {
    let provider = Provider::json();
    let (_, source) = counted_source(sign_in(ACCESS, Some(ACCOUNT), lasting()), || {
        Err(EngineError("no renewal expected".into()))
    });
    let relay = chatgpt_relay(&provider, source);
    assert!(relay.is_chatgpt());
    assert_eq!(
        relay.base_url(),
        format!("http://127.0.0.1:{}{CODEX_PATH}", relay.port())
    );
    let body = r#"{"model":"gpt-5","stream":true,"store":false,"input":[]}"#;
    let extra = concat!(
        "chatgpt-account-id: acct-harness\r\n",
        "Cookie: session=harness\r\n",
        "OpenAI-Organization: org-harness\r\n",
        "OpenAI-Project: proj-harness\r\n",
        "x-openai-account-routing-override: other-workspace\r\n",
        "X-OpenAI-Fedramp: true\r\n",
        "chatgpt-other: harness\r\n",
        "originator: row-harness\r\n",
    );
    for auth in [
        format!("Authorization: Bearer {}", relay.token()),
        format!("x-api-key: {}", relay.token()),
    ] {
        let (status, _, reply) = send(relay.port(), &responses(&auth, extra, body));
        assert_eq!(status, 200, "{auth}");
        assert_eq!(reply, br#"{"ok":true}"#);
    }
    let seen = provider.seen();
    assert_eq!(seen.len(), 2);
    for request in seen.iter() {
        assert_eq!(request.method, "POST");
        assert_eq!(request.target, format!("{CODEX_PATH}/responses"));
        assert_eq!(
            named(request, "authorization").collect::<Vec<_>>(),
            [format!("Bearer {ACCESS}").as_str()]
        );
        assert_eq!(
            named(request, "chatgpt-account-id").collect::<Vec<_>>(),
            [ACCOUNT]
        );
        for dropped in [
            "x-api-key",
            "cookie",
            "openai-organization",
            "openai-project",
            "x-openai-account-routing-override",
            "x-openai-fedramp",
            "chatgpt-other",
        ] {
            assert_eq!(header(request, dropped), None, "{dropped}");
        }
        assert_eq!(header(request, "originator"), Some("row-harness"));
        assert_eq!(header(request, "accept"), Some("text/event-stream"));
        assert_eq!(request.body, body.as_bytes());
        assert!(
            request
                .headers
                .iter()
                .all(|(_, value)| !value.contains(relay.token()))
        );
    }
}

/// T22 R1: a sign-in whose record has no account id sends no `chatgpt-account-id`, and the
/// one the harness sent is still dropped.
#[test]
fn a_chatgpt_sign_in_without_an_account_id_sends_no_account_id_header() {
    let provider = Provider::json();
    let (_, source) = counted_source(sign_in(ACCESS, None, lasting()), || {
        Err(EngineError("no renewal expected".into()))
    });
    let relay = chatgpt_relay(&provider, source);
    let auth = format!("Authorization: Bearer {}", relay.token());
    let (status, _, _) = send(
        relay.port(),
        &responses(&auth, "chatgpt-account-id: acct-harness\r\n", "{}"),
    );
    assert_eq!(status, 200);
    let seen = provider.seen();
    assert_eq!(header(&seen[0], "chatgpt-account-id"), None);
    assert_eq!(
        header(&seen[0], "authorization"),
        Some(format!("Bearer {ACCESS}").as_str())
    );
}

/// T22 R2 and R3: the access token opens the whole ChatGPT account, so a ChatGPT relay
/// forwards only `POST <base>/responses`, with no query. Every other path or method under
/// the backend, as written or percent-encoded, and a wrong token are refused before any
/// renewal and reach no upstream; the allowed request renews the lapsing token once and
/// goes up with the fresh one.
#[test]
fn a_chatgpt_relay_forwards_only_post_responses_and_renews_for_no_refused_request() {
    let provider = Provider::json();
    let (calls, source) = counted_source(
        sign_in(
            ACCESS,
            Some(ACCOUNT),
            epoch_now() + Duration::from_secs(100),
        ),
        || Ok(sign_in(FRESH, Some(ACCOUNT), lasting())),
    );
    let relay = chatgpt_relay(&provider, source);
    let auth = format!("Authorization: Bearer {}", relay.token());
    for target in [
        "/backend-api/codex/models",
        "/backend-api/codex/models?client_version=1",
        "/backend-api/codex",
        "/backend-api/codex/",
        "/backend-api/codex/responses/",
        "/backend-api/codex/responses/compact",
        "/backend-api/codex/responses?stream=true",
        "/backend-api/codex//responses",
        "/backend-api/codex/Responses",
        "/backend-api/codex/./responses",
        "/backend-api/codex/x/../responses",
        "/backend-api/codex/%2e%2e/conversations",
        "/backend-api/codex/responses/%2e%2e/%2e%2e/wham/usage",
        "/backend-api/conversations",
        "/backend-api/wham/usage",
        "/backend-api/me",
        "/v1/responses",
        "/",
    ] {
        let (status, _, _) = send(relay.port(), &post(target, &auth, "{}"));
        assert_eq!(status, 404, "{target}");
    }
    let get = format!("GET {CODEX_PATH}/responses HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}\r\n\r\n");
    assert_eq!(send(relay.port(), get.as_bytes()).0, 404);
    let wrong = format!("Authorization: Bearer {TOKEN_PREFIX}{}", "0".repeat(64));
    assert_eq!(send(relay.port(), &responses(&wrong, "", "{}")).0, 401);
    assert!(provider.seen().is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let (status, _, _) = send(relay.port(), &responses(&auth, "", "{}"));
    assert_eq!(status, 200);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let seen = provider.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        header(&seen[0], "authorization"),
        Some(format!("Bearer {FRESH}").as_str())
    );
}

/// T22 R4: once a renewal leaves the sign-in lapsed, the relay answers the harness 401 with
/// an OpenAI error body carrying `chatgpt_sign_in_expired` and its plain sentence, reaches
/// no upstream, and reports the code the run ends with.
#[test]
fn a_chatgpt_relay_answers_a_lapsed_sign_in_with_an_openai_error_and_reports_it() {
    let provider = Provider::json();
    let lapsed = epoch_now() - Duration::from_secs(10);
    let (calls, source) = counted_source(sign_in(ACCESS, Some(ACCOUNT), lapsed), move || {
        Ok(sign_in(ACCESS, Some(ACCOUNT), lapsed))
    });
    let relay = chatgpt_relay(&provider, source);
    assert_eq!(relay.failure(), None);
    let auth = format!("Authorization: Bearer {}", relay.token());
    let (status, head, body) = send(relay.port(), &responses(&auth, "", "{}"));
    assert_eq!(status, 401);
    assert!(head.contains("Content-Type: application/json"), "{head}");
    let error: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(error["error"]["code"], chatgpt::SIGN_IN_EXPIRED);
    assert_eq!(error["error"]["type"], "invalid_request_error");
    assert_eq!(
        error["error"]["message"],
        crate::config::plain_problem(chatgpt::SIGN_IN_EXPIRED).unwrap_or("missing")
    );
    assert!(provider.seen().is_empty());
    assert_eq!(relay.failure(), Some(chatgpt::SIGN_IN_EXPIRED));
    assert_eq!(send(relay.port(), &responses(&auth, "", "{}")).0, 401);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// T22 R5: the access token and account id go only into the curl config on stdin, in the
/// `Authorization` and `chatgpt-account-id` lines the engine writes; the harness's own
/// account id and cookies are not in it, and curl's arguments and environment hold neither
/// value.
#[test]
fn a_chatgpt_curl_config_alone_holds_the_token_and_account_id() {
    let target = upstream(CHATGPT_CODEX_BASE_URL).unwrap();
    let token = format!("{TOKEN_PREFIX}{}", "c".repeat(64));
    let incoming = Incoming {
        method: "POST".into(),
        target: format!("{CODEX_PATH}/responses"),
        headers: vec![
            ("host".into(), "127.0.0.1:9".into()),
            ("authorization".into(), format!("Bearer {token}")),
            ("chatgpt-account-id".into(), "acct-harness".into()),
            ("cookie".into(), "session=harness".into()),
            ("accept".into(), "text/event-stream".into()),
        ],
        body: b"{}".to_vec(),
    };
    let access = SecretString::bearer(ACCESS.as_bytes().to_vec()).unwrap();
    let config = chatgpt_config(
        &target,
        &incoming,
        &access,
        Some(ACCOUNT),
        Some(Path::new("/dev/fd/7")),
    )
    .unwrap();
    let config = String::from_utf8(config).unwrap();
    assert!(config.contains("url = \"https://chatgpt.com/backend-api/codex/responses\"\n"));
    assert!(config.contains(&format!("header = \"Authorization: Bearer {ACCESS}\"\n")));
    assert!(config.contains(&format!("header = \"chatgpt-account-id: {ACCOUNT}\"\n")));
    assert!(config.contains("header = \"accept: text/event-stream\"\n"));
    assert_eq!(config.matches(ACCESS).count(), 1);
    assert_eq!(config.matches(ACCOUNT).count(), 1);
    assert!(!config.contains(&token));
    assert!(!config.contains("acct-harness"));
    assert!(!config.contains("session=harness"));
    let command = upstream_command(&target);
    for argument in command.get_args() {
        let argument = argument.to_string_lossy();
        assert!(!argument.contains(ACCESS) && !argument.contains(ACCOUNT));
        assert!(!argument.contains("chatgpt.com"));
    }
    for (name, value) in command.get_envs() {
        let pair = format!("{name:?}={value:?}");
        assert!(!pair.contains(ACCESS) && !pair.contains(ACCOUNT), "{pair}");
    }
}

/// T22: the public ChatGPT relay forwards only to the ChatGPT backend Codex uses; no caller
/// can point a ChatGPT sign-in's token at another host.
#[test]
fn the_public_chatgpt_relay_forwards_only_to_the_chatgpt_backend() {
    assert_eq!(
        chatgpt::CHATGPT_CODEX_BASE_URL,
        "https://chatgpt.com/backend-api/codex"
    );
    let (_, source) = counted_source(sign_in(ACCESS, Some(ACCOUNT), lasting()), || {
        Err(EngineError("no renewal expected".into()))
    });
    let relay = Relay::start_chatgpt(source).unwrap();
    assert!(relay.is_chatgpt());
    assert_eq!(
        relay.shared.upstream,
        upstream(chatgpt::CHATGPT_CODEX_BASE_URL).unwrap()
    );
    assert_eq!(relay.shared.upstream.origin, "https://chatgpt.com");
    assert_eq!(
        relay.base_url(),
        format!("http://127.0.0.1:{}{CODEX_PATH}", relay.port())
    );
}

/// T22: a request waiting on a renewal does not hold the relay open: dropping the relay
/// tells the renewal to stop, so the drop returns promptly and nothing reaches upstream.
#[test]
fn dropping_a_chatgpt_relay_during_a_renewal_returns_promptly() {
    let provider = Provider::json();
    let calls = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&calls);
    let renew: chatgpt::Renew = Box::new(move |_: Duration, halted: &dyn Fn() -> bool| {
        counting.fetch_add(1, Ordering::SeqCst);
        let waited = Instant::now();
        while !halted() && waited.elapsed() < WAIT {
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(EngineError("native_profile_renewal_stopped".into()))
    });
    let source = Arc::new(chatgpt::ChatGptSource::new(
        sign_in(ACCESS, None, epoch_now() + Duration::from_secs(10)),
        renew,
    ));
    let relay = chatgpt_relay(&provider, source);
    let request = responses(
        &format!("Authorization: Bearer {}", relay.token()),
        "",
        "{}",
    );
    let port = relay.port();
    let client = std::thread::spawn(move || {
        let mut stream = crate::spawn_gate::connect_tcp_std(("127.0.0.1", port)).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        stream.write_all(&request).unwrap();
        let mut reply = Vec::new();
        let _ = stream.read_to_end(&mut reply);
    });
    let started = Instant::now();
    while calls.load(Ordering::SeqCst) == 0 && started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let dropping = Instant::now();
    drop(relay);
    assert!(
        dropping.elapsed() < Duration::from_secs(5),
        "the drop took {:?}",
        dropping.elapsed()
    );
    let _ = client.join();
    assert!(provider.seen().is_empty());
}
