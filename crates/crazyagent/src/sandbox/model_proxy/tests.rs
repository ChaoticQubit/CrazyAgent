use super::*;

/// Builds one TLS record holding a ClientHello as a client sends it, with a server_name
/// extension for `name` when given and one other extension before it. Input: the name.
/// Output: the record's bytes.
fn hello(name: Option<&str>) -> Vec<u8> {
    let mut extensions = vec![0x00, 0x0b, 0x00, 0x02, 0x01, 0x00];
    if let Some(name) = name {
        let size = u16::try_from(name.len()).unwrap();
        extensions.extend([0x00, 0x00]);
        extensions.extend((size + 5).to_be_bytes());
        extensions.extend((size + 3).to_be_bytes());
        extensions.push(0x00);
        extensions.extend(size.to_be_bytes());
        extensions.extend(name.as_bytes());
    }
    let mut body = vec![0x03, 0x03];
    body.extend([0x5a; 32]);
    body.push(32);
    body.extend([0xa5; 32]);
    body.extend([0x00, 0x04, 0x13, 0x01, 0x13, 0x02]);
    body.extend([0x01, 0x00]);
    body.extend(u16::try_from(extensions.len()).unwrap().to_be_bytes());
    body.extend(extensions);
    let mut message = vec![0x01];
    message.extend(&u32::try_from(body.len()).unwrap().to_be_bytes()[1..]);
    message.extend(body);
    let mut record = vec![0x16, 0x03, 0x01];
    record.extend(u16::try_from(message.len()).unwrap().to_be_bytes());
    record.extend(message);
    record
}

fn host(name: &str, port: u16) -> ModelHost {
    ModelHost::new(name, port).unwrap()
}

/// Connects to the proxy and sends `request`. Inputs: the proxy and the bytes. Output: the
/// connection, with a read bound so a test never hangs.
fn ask(proxy: &ModelProxy, request: &[u8]) -> TcpStream {
    let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, proxy.port())).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    client.write_all(request).unwrap();
    client
}

/// Reads what the proxy answers until it closes. Input: the connection. Output: the text.
fn answer(mut client: TcpStream) -> String {
    let mut text = Vec::new();
    let _ = client.read_to_end(&mut text);
    String::from_utf8_lossy(&text).into_owned()
}

/// Reads the proxy's reply head, up to its blank line. Input: the connection. Output: the
/// head.
fn head(client: &mut TcpStream) -> String {
    let mut text = Vec::new();
    let mut byte = [0u8; 1];
    while !text.ends_with(b"\r\n\r\n") {
        assert_eq!(client.read(&mut byte).unwrap(), 1, "the proxy closed early");
        text.push(byte[0]);
        assert!(text.len() < 1024);
    }
    String::from_utf8(text).unwrap()
}

fn connect(target: &str) -> Vec<u8> {
    format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\nUser-Agent: test\r\n\r\n").into_bytes()
}

#[test]
fn a_client_hello_names_its_server() {
    let named = hello(Some("api.anthropic.com"));
    assert_eq!(server_name(&named), Some("api.anthropic.com"));
    assert_eq!(server_name(&hello(None)), None);
    assert_eq!(server_name(&named[..named.len() - 1]), None);
    let mut alert = named.clone();
    alert[0] = 0x15;
    assert_eq!(server_name(&alert), None);
    let mut server = named.clone();
    server[5] = 0x02;
    assert_eq!(server_name(&server), None);
}

#[test]
fn model_hosts_are_dns_names_never_addresses() {
    let named = host("API.Anthropic.com", 443);
    assert_eq!((named.name(), named.port()), ("api.anthropic.com", 443));
    for refused in [
        "127.0.0.1",
        "::1",
        "[::1]",
        "0x7f000001",
        "0x7f.0.0.1",
        "127.1",
        "2130706433",
        "api.anthropic.com.",
        "a..b",
        "-a.com",
        "a_b.com",
        "",
    ] {
        assert_eq!(ModelHost::new(refused, 443), None, "{refused}");
    }
    assert_eq!(ModelHost::new("api.anthropic.com", 0), None);
    assert_eq!(
        connect_target("CONNECT api.anthropic.com:443 HTTP/1.1"),
        Some(host("api.anthropic.com", 443))
    );
    for line in [
        "CONNECT api.anthropic.com HTTP/1.1",
        "CONNECT api.anthropic.com:443 HTTP/2",
        "CONNECT api.anthropic.com:443 HTTP/1.1 extra",
        "GET http://api.anthropic.com/ HTTP/1.1",
        "connect api.anthropic.com:443 HTTP/1.1",
        "CONNECT api.anthropic.com:+443 HTTP/1.1",
        "CONNECT api.anthropic.com:65536 HTTP/1.1",
        "CONNECT 127.0.0.1:443 HTTP/1.1",
    ] {
        assert_eq!(connect_target(line), None, "{line}");
    }
}

#[test]
fn the_proxy_variables_name_the_proxy_and_exempt_no_host() {
    let variables = variables(41_234);
    for name in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        assert!(
            variables.contains(&(name, "http://127.0.0.1:41234".to_owned())),
            "{name}"
        );
    }
    for name in ["NO_PROXY", "no_proxy"] {
        assert!(variables.contains(&(name, String::new())), "{name}");
    }
}

#[test]
fn a_host_or_port_off_the_list_and_an_address_are_refused_with_403() {
    let proxy = ModelProxy::bind(7, vec![host("api.anthropic.com", 443)], None).unwrap();
    for target in [
        "example.com:443",
        "api.anthropic.com:80",
        "api.anthropic.com:8443",
        "api.anthropic.com.evil.com:443",
        "93.184.216.34:443",
        "[::1]:443",
    ] {
        let said = answer(ask(&proxy, &connect(target)));
        assert!(
            said.starts_with("HTTP/1.1 403 Forbidden\r\n"),
            "{target}: {said}"
        );
    }
    let said = answer(ask(
        &proxy,
        b"GET http://api.anthropic.com/ HTTP/1.1\r\nHost: api.anthropic.com\r\n\r\n",
    ));
    assert!(said.starts_with("HTTP/1.1 403 Forbidden\r\n"), "{said}");
}

#[test]
fn a_request_head_past_its_bound_is_refused_with_403() {
    let proxy = ModelProxy::bind(7, vec![host("api.anthropic.com", 443)], None).unwrap();
    let mut request = b"CONNECT api.anthropic.com:443 HTTP/1.1\r\nX-Padding: ".to_vec();
    request.resize(HEADER_BYTES_MAX + 1, b'a');
    let said = answer(ask(&proxy, &request));
    assert!(said.starts_with("HTTP/1.1 403 Forbidden\r\n"), "{said}");
}

#[test]
fn an_allowed_host_gets_the_client_hello_and_both_ways_untouched() {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = upstream.local_addr().unwrap().port();
    let proxy = ModelProxy::bind(7, vec![host("localhost", port)], None).unwrap();
    let mut sent = hello(Some("localhost"));
    sent.extend(b"after the hello");
    let mut client = ask(&proxy, &connect(&format!("localhost:{port}")));
    assert_eq!(
        head(&mut client),
        "HTTP/1.1 200 Connection Established\r\n\r\n"
    );
    client.write_all(&sent).unwrap();
    let (mut server, _) = upstream.accept().unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let mut got = vec![0u8; sent.len()];
    server.read_exact(&mut got).unwrap();
    assert_eq!(got, sent);
    server.write_all(b"from the model host").unwrap();
    let mut back = [0u8; 19];
    client.read_exact(&mut back).unwrap();
    assert_eq!(&back, b"from the model host");
    drop(server);
    let mut rest = Vec::new();
    assert_eq!(client.read_to_end(&mut rest).unwrap(), 0);
}

#[test]
fn a_client_hello_naming_another_host_or_none_never_reaches_the_host() {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = upstream.local_addr().unwrap().port();
    upstream.set_nonblocking(true).unwrap();
    let proxy = ModelProxy::bind(7, vec![host("localhost", port)], None).unwrap();
    for sent in [
        hello(Some("example.com")),
        hello(None),
        b"GET / HTTP/1.1\r\n\r\n".to_vec(),
    ] {
        let mut client = ask(&proxy, &connect(&format!("localhost:{port}")));
        assert_eq!(
            head(&mut client),
            "HTTP/1.1 200 Connection Established\r\n\r\n"
        );
        client.write_all(&sent).unwrap();
        let mut rest = Vec::new();
        assert_eq!(client.read_to_end(&mut rest).unwrap_or(0), 0);
        assert!(rest.is_empty());
    }
    let reached = upstream.accept();
    assert!(
        reached
            .as_ref()
            .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock),
        "a tunnel reached the host: {reached:?}"
    );
}

/// Connects to the proxy and sends nothing, so a refusal it sends at once is read whole.
/// Inputs: the proxy and how long to wait for a reply. Output: what the proxy said before
/// the wait ran out, empty while it waits for a request.
fn silent(proxy: &ModelProxy, wait: Duration) -> String {
    let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, proxy.port())).unwrap();
    client.set_read_timeout(Some(wait)).unwrap();
    let mut text = Vec::new();
    let _ = client.read_to_end(&mut text);
    String::from_utf8_lossy(&text).into_owned()
}

#[test]
fn a_tunnel_past_the_bound_is_refused_with_503() {
    let proxy = ModelProxy::bind(7, vec![host("api.anthropic.com", 443)], None).unwrap();
    let held: Vec<TcpStream> = (0..TUNNELS_MAX)
        .map(|_| TcpStream::connect((Ipv4Addr::LOCALHOST, proxy.port())).unwrap())
        .collect();
    let said = silent(&proxy, Duration::from_secs(5));
    assert!(
        said.starts_with("HTTP/1.1 503 Service Unavailable\r\n"),
        "{said}"
    );
    drop(held);
    let deadline = Instant::now() + Duration::from_secs(10);
    while silent(&proxy, Duration::from_secs(1)).starts_with("HTTP/1.1 503 ") {
        assert!(Instant::now() < deadline, "closed tunnels were not let go");
    }
}

#[test]
fn a_saved_port_another_process_holds_is_refused_and_a_free_one_is_kept() {
    let taken = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = taken.local_addr().unwrap().port();
    let refused = ModelProxy::bind(7, vec![host("api.anthropic.com", 443)], Some(port));
    assert!(refused.is_err(), "{refused:?}");
    drop(taken);
    let kept = ModelProxy::bind(7, vec![host("api.anthropic.com", 443)], Some(port)).unwrap();
    assert_eq!(kept.port(), port);
}

#[test]
fn dropping_the_proxy_closes_its_port_and_its_open_tunnels() {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = upstream.local_addr().unwrap().port();
    let proxy = ModelProxy::bind(7, vec![host("localhost", port)], None).unwrap();
    let listening = proxy.port();
    let mut client = ask(&proxy, &connect(&format!("localhost:{port}")));
    assert!(head(&mut client).starts_with("HTTP/1.1 200 "));
    client.write_all(&hello(Some("localhost"))).unwrap();
    let (_server, _) = upstream.accept().unwrap();
    let started = Instant::now();
    drop(proxy);
    let mut rest = Vec::new();
    assert_eq!(client.read_to_end(&mut rest).unwrap_or(0), 0);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, listening)).is_err());
}
