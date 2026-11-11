use super::*;

/// Builds one TLS record holding a ClientHello as a client sends it, with a server_name
/// extension for `name` when given and one other extension before it. Input: the name.
/// Output: the record's bytes.
fn hello(name: Option<&str>) -> Vec<u8> {
    hello_with(name, &[], &[])
}

/// One TLS extension: its type, its length and its data. Inputs: the type and the data.
/// Output: the extension's bytes.
fn extension(kind: u16, data: &[u8]) -> Vec<u8> {
    let mut bytes = kind.to_be_bytes().to_vec();
    bytes.extend(u16::try_from(data.len()).unwrap().to_be_bytes());
    bytes.extend(data);
    bytes
}

/// The data of a server_name extension naming one host. Input: the host. Output: the data.
fn named(name: &str) -> Vec<u8> {
    let size = u16::try_from(name.len()).unwrap();
    let mut data = (size + 3).to_be_bytes().to_vec();
    data.push(0x00);
    data.extend(size.to_be_bytes());
    data.extend(name.as_bytes());
    data
}

/// Builds a ClientHello record like [`hello`], with `extra` extensions after the server
/// name and `after` bytes past the extension block, both counted in the message and record
/// lengths. Inputs: the name, the extra extensions and the trailing bytes. Output: the record.
fn hello_with(name: Option<&str>, extra: &[u8], after: &[u8]) -> Vec<u8> {
    let mut extensions = vec![0x00, 0x0b, 0x00, 0x02, 0x01, 0x00];
    if let Some(name) = name {
        extensions.extend(extension(0x0000, &named(name)));
    }
    extensions.extend(extra);
    let mut body = vec![0x03, 0x03];
    body.extend([0x5a; 32]);
    body.push(32);
    body.extend([0xa5; 32]);
    body.extend([0x00, 0x04, 0x13, 0x01, 0x13, 0x02]);
    body.extend([0x01, 0x00]);
    body.extend(u16::try_from(extensions.len()).unwrap().to_be_bytes());
    body.extend(extensions);
    body.extend(after);
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

/// A port of [`PROXY_PORTS`] that a proxy held and let go a moment ago, so a row can use it
/// as a saved port. The next proxies of this process start looking past it. Output: the
/// port.
fn let_go_block_port() -> u16 {
    let proxy = ModelProxy::bind(7, vec![host("api.anthropic.com", 443)], None, &[]).unwrap();
    let port = proxy.port();
    drop(proxy);
    assert!(PROXY_PORTS.contains(&port));
    port
}

/// Connects to the proxy on 127.0.0.1 and sends `request`. Inputs: the proxy and the bytes.
/// Output: the connection, with a read bound so a test never hangs.
fn ask(proxy: &ModelProxy, request: &[u8]) -> TcpStream {
    ask_at(IpAddr::V4(Ipv4Addr::LOCALHOST), proxy.port(), request)
}

/// Connects to `address` on `port` and sends `request`. Inputs: the address, the port and
/// the bytes. Output: the connection, with a read bound so a test never hangs.
fn ask_at(address: IpAddr, port: u16, request: &[u8]) -> TcpStream {
    let mut client = spawn_gate::connect_tcp_std((address, port)).unwrap();
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

/// S4B-2W1: a proxy started without a saved port takes a port of the fixed block on both
/// loopback addresses, and a second proxy started while the first holds its port takes
/// another port of the block.
#[test]
fn a_proxy_takes_a_port_of_the_fixed_block_and_skips_a_taken_one() {
    let first = ModelProxy::bind(7, vec![host("api.anthropic.com", 443)], None, &[]).unwrap();
    let second = ModelProxy::bind(8, vec![host("api.anthropic.com", 443)], None, &[]).unwrap();
    for proxy in [&first, &second] {
        assert!(PROXY_PORTS.contains(&proxy.port()), "{}", proxy.port());
    }
    assert_ne!(first.port(), second.port());
    for address in ["127.0.0.1", "[::1]"] {
        let taken = spawn_gate::bind_tcp_std(format!("{address}:{}", first.port()));
        assert_eq!(
            taken.map(|_| ()).map_err(|error| error.kind()),
            Err(io::ErrorKind::AddrInUse),
            "{address}"
        );
    }
}

/// S4B-2W1: a port search whose every port is held, one on 127.0.0.1 and one on ::1 only,
/// skips both and refuses plainly with `AddrInUse`, from any start.
#[test]
fn a_port_search_with_every_port_taken_refuses_plainly() {
    let v4 = spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let v6 = spawn_gate::bind_tcp_std("[::1]:0").unwrap();
    let taken = [
        v4.local_addr().unwrap().port(),
        v6.local_addr().unwrap().port(),
    ];
    for start in [0, 1, usize::MAX] {
        let refused = free_pair(&taken, &[], start).map(|_| ()).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::AddrInUse, "{start}");
        assert_eq!(refused.to_string(), "every model proxy port is taken");
    }
}

/// A port the system just handed out on 127.0.0.1 and freed again, other than any of `not`,
/// for a port search that needs a free port. Input: the ports it must not be. Output: the
/// port.
fn loose_port(not: &[u16]) -> u16 {
    loop {
        let listener = spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        if !not.contains(&port) {
            return port;
        }
    }
}

/// S4B-2W1 r1 (rv-code L1, rv-secreview L2): a port search skips a port this user holds on
/// 127.0.0.1, ::1, 0.0.0.0 or ::, each bound as `std` binds, with `SO_REUSEADDR`, and takes
/// the next one, so a proxy never sits beside an owner's wildcard listener and takes its
/// loopback connections.
#[test]
fn a_port_search_skips_a_port_held_on_any_address_of_this_user() {
    for holder in ["127.0.0.1:0", "[::1]:0", "0.0.0.0:0", "[::]:0"] {
        let held = spawn_gate::bind_tcp_std(holder).unwrap();
        let taken = held.local_addr().unwrap().port();
        let free = loose_port(&[taken]);
        let bound = free_pair(&[taken, free], &[], 0).unwrap();
        for (listener, address) in bound.iter().zip(["127.0.0.1", "::1"]) {
            let local = listener.local_addr().unwrap();
            assert_eq!(local.port(), free, "{holder}");
            assert_eq!(local.ip().to_string(), address, "{holder}");
        }
    }
}

/// S4B-2W1 r1 (rv-secreview L1): a port search leaves the owner's configured model ports
/// alone even while they are free, so a proxy never takes the port the owner's model
/// server starts on, and refuses plainly when they are the only ports it has.
#[test]
fn a_port_search_leaves_the_owner_model_ports_alone_even_while_free() {
    let kept = loose_port(&[]);
    let free = loose_port(&[kept]);
    let bound = free_pair(&[kept, free], &[kept], 0).unwrap();
    assert_eq!(bound[0].local_addr().unwrap().port(), free);
    drop(bound);
    let refused = free_pair(&[kept], &[kept], 0).map(|_| ()).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::AddrInUse);
    assert_eq!(refused.to_string(), "every model proxy port is taken");
}

#[test]
fn a_host_or_port_off_the_list_and_an_address_are_refused_with_403() {
    let proxy = ModelProxy::bind(7, vec![host("api.anthropic.com", 443)], None, &[]).unwrap();
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
    let proxy = ModelProxy::bind(7, vec![host("api.anthropic.com", 443)], None, &[]).unwrap();
    let mut request = b"CONNECT api.anthropic.com:443 HTTP/1.1\r\nX-Padding: ".to_vec();
    request.resize(HEADER_BYTES_MAX + 1, b'a');
    let started = Instant::now();
    let said = answer(ask(&proxy, &request));
    assert!(said.starts_with("HTTP/1.1 403 Forbidden\r\n"), "{said}");
    assert!(
        started.elapsed() < HANDSHAKE_TIME / 2,
        "a head past its bound waited {:?} instead of being refused at the bound",
        started.elapsed()
    );
}

#[test]
fn an_allowed_host_gets_the_client_hello_and_both_ways_untouched() {
    let upstream = spawn_gate::bind_tcp_std((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = upstream.local_addr().unwrap().port();
    let proxy = ModelProxy::bind_local(7, vec![host("localhost", port)], None, &[]).unwrap();
    let mut sent = hello(Some("localhost"));
    sent.extend(b"after the hello");
    let mut client = ask(&proxy, &connect(&format!("localhost:{port}")));
    assert_eq!(
        head(&mut client),
        "HTTP/1.1 200 Connection Established\r\n\r\n"
    );
    client.write_all(&sent).unwrap();
    let (mut server, _) = spawn_gate::accept_tcp_std(&upstream).unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let mut got = vec![0u8; sent.len()];
    let read = server.read_exact(&mut got);
    assert!(
        read.is_ok(),
        "the host did not get the client's bytes: {read:?}"
    );
    assert_eq!(got, sent);
    server.write_all(b"from the model host").unwrap();
    let mut back = [0u8; 19];
    let read = client.read_exact(&mut back);
    assert!(
        read.is_ok(),
        "the client did not get the host's bytes: {read:?}"
    );
    assert_eq!(&back, b"from the model host");
    drop(server);
    let mut rest = Vec::new();
    assert_eq!(client.read_to_end(&mut rest).unwrap(), 0);
}

#[test]
fn a_client_hello_naming_another_host_or_none_never_reaches_the_host() {
    let upstream = spawn_gate::bind_tcp_std((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = upstream.local_addr().unwrap().port();
    upstream.set_nonblocking(true).unwrap();
    let proxy = ModelProxy::bind_local(7, vec![host("localhost", port)], None, &[]).unwrap();
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
    let reached = spawn_gate::accept_tcp_std(&upstream);
    assert!(
        reached
            .as_ref()
            .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock),
        "a tunnel reached the host: {reached:?}"
    );
}

/// A model host whose name resolves only to loopback is never dialled: the engine opens
/// tunnels outside the sandbox, so a host's DNS must not steer it to the engine's own
/// machine. The tunnel is cut and nothing reaches the local listener (CS-4).
#[test]
fn a_model_host_that_resolves_to_loopback_is_never_dialled() {
    let upstream = spawn_gate::bind_tcp_std((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = upstream.local_addr().unwrap().port();
    upstream.set_nonblocking(true).unwrap();
    let proxy = ModelProxy::bind(7, vec![host("localhost", port)], None, &[]).unwrap();
    let mut client = ask(&proxy, &connect(&format!("localhost:{port}")));
    assert_eq!(
        head(&mut client),
        "HTTP/1.1 200 Connection Established\r\n\r\n"
    );
    client.write_all(&hello(Some("localhost"))).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut rest = Vec::new();
    assert_eq!(client.read_to_end(&mut rest).unwrap_or(0), 0);
    let reached = spawn_gate::accept_tcp_std(&upstream);
    assert!(
        reached
            .as_ref()
            .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock),
        "a tunnel reached a loopback host: {reached:?}"
    );
}

/// Connects to the proxy and sends nothing, so a refusal it sends at once is read whole.
/// Inputs: the proxy and how long to wait for a reply. Output: what the proxy said before
/// the wait ran out, empty while it waits for a request.
fn silent(proxy: &ModelProxy, wait: Duration) -> String {
    let mut client = spawn_gate::connect_tcp_std((Ipv4Addr::LOCALHOST, proxy.port())).unwrap();
    client.set_read_timeout(Some(wait)).unwrap();
    let mut text = Vec::new();
    let _ = client.read_to_end(&mut text);
    String::from_utf8_lossy(&text).into_owned()
}

#[test]
fn a_tunnel_past_the_bound_is_refused_with_503() {
    let proxy = ModelProxy::bind(7, vec![host("api.anthropic.com", 443)], None, &[]).unwrap();
    let held: Vec<TcpStream> = (0..TUNNELS_MAX)
        .map(|_| spawn_gate::connect_tcp_std((Ipv4Addr::LOCALHOST, proxy.port())).unwrap())
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
    let port = let_go_block_port();
    let taken = spawn_gate::bind_tcp_std((Ipv4Addr::LOCALHOST, port)).unwrap();
    let refused = ModelProxy::bind(7, vec![host("api.anthropic.com", 443)], Some(port), &[]);
    assert_eq!(
        refused.as_ref().err().map(io::Error::kind),
        Some(io::ErrorKind::AddrInUse),
        "{refused:?}"
    );
    drop(taken);
    let kept = ModelProxy::bind(7, vec![host("api.anthropic.com", 443)], Some(port), &[]).unwrap();
    assert_eq!(kept.port(), port);
}

#[test]
fn a_saved_port_outside_the_proxy_ports_is_refused_and_never_bound() {
    let free = spawn_gate::bind_tcp_std((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let picked = free.local_addr().unwrap().port();
    drop(free);
    assert!(!PROXY_PORTS.contains(&picked), "{picked}");
    for port in [picked, PROXY_PORTS.start() - 1, PROXY_PORTS.end() + 1] {
        let refused = ModelProxy::bind(7, vec![host("api.anthropic.com", 443)], Some(port), &[]);
        assert_eq!(
            refused.as_ref().err().map(io::Error::kind),
            Some(io::ErrorKind::InvalidInput),
            "a saved port {port} outside the proxy ports: {refused:?}"
        );
    }
    assert!(
        spawn_gate::connect_tcp_std((Ipv4Addr::LOCALHOST, picked)).is_err(),
        "a proxy listens on {picked}"
    );
    let first = *PROXY_PORTS.start();
    let other_block = if first == 41_300 { 29_300 } else { 41_300 };
    let refused = ModelProxy::bind(
        7,
        vec![host("api.anthropic.com", 443)],
        Some(other_block),
        &[],
    );
    assert_eq!(
        refused.as_ref().err().map(io::Error::kind),
        Some(io::ErrorKind::InvalidInput),
        "a port of the other platform's proxy ports: {refused:?}"
    );
}

#[test]
fn dropping_the_proxy_closes_its_port_and_its_open_tunnels() {
    let upstream = spawn_gate::bind_tcp_std((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = upstream.local_addr().unwrap().port();
    let proxy = ModelProxy::bind_local(7, vec![host("localhost", port)], None, &[]).unwrap();
    let listening = proxy.port();
    let mut client = ask(&proxy, &connect(&format!("localhost:{port}")));
    assert!(head(&mut client).starts_with("HTTP/1.1 200 "));
    client.write_all(&hello(Some("localhost"))).unwrap();
    let (_server, _) = spawn_gate::accept_tcp_std(&upstream).unwrap();
    let started = Instant::now();
    drop(proxy);
    let mut rest = Vec::new();
    assert_eq!(client.read_to_end(&mut rest).unwrap_or(0), 0);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(spawn_gate::connect_tcp_std((Ipv4Addr::LOCALHOST, listening)).is_err());
    assert!(spawn_gate::connect_tcp_std((Ipv6Addr::LOCALHOST, listening)).is_err());
}

#[test]
fn a_client_hello_with_an_encrypted_name_trailing_bytes_or_two_names_never_reaches_the_host() {
    let upstream = spawn_gate::bind_tcp_std((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = upstream.local_addr().unwrap().port();
    upstream.set_nonblocking(true).unwrap();
    let proxy = ModelProxy::bind_local(7, vec![host("localhost", port)], None, &[]).unwrap();
    let versions = extension(0x002b, &[0x02, 0x03, 0x04]);
    assert_eq!(
        server_name(&hello_with(Some("localhost"), &versions, &[])),
        Some("localhost")
    );
    for (what, sent) in [
        (
            "an Encrypted Client Hello",
            hello_with(
                Some("localhost"),
                &extension(0xfe0d, &[0x00, 0x00, 0x01]),
                &[],
            ),
        ),
        (
            "an encrypted server name",
            hello_with(Some("localhost"), &extension(0xffce, &[0x13, 0x01]), &[]),
        ),
        (
            "a second server_name extension",
            hello_with(
                Some("localhost"),
                &extension(0x0000, &named("example.com")),
                &[],
            ),
        ),
        (
            "bytes past the extensions",
            hello_with(Some("localhost"), &[], &[0x00, 0x00]),
        ),
    ] {
        assert_eq!(server_name(&sent), None, "{what}");
        let mut client = ask(&proxy, &connect(&format!("localhost:{port}")));
        assert_eq!(
            head(&mut client),
            "HTTP/1.1 200 Connection Established\r\n\r\n"
        );
        client.write_all(&sent).unwrap();
        let mut rest = Vec::new();
        assert_eq!(client.read_to_end(&mut rest).unwrap_or(0), 0, "{what}");
    }
    let reached = spawn_gate::accept_tcp_std(&upstream);
    assert!(
        reached
            .as_ref()
            .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock),
        "a tunnel reached the host: {reached:?}"
    );
}

#[test]
fn the_proxy_holds_its_port_on_both_loopback_addresses_so_no_other_listener_stands_in() {
    let proxy = ModelProxy::bind(7, vec![host("api.anthropic.com", 443)], None, &[]).unwrap();
    let port = proxy.port();
    let stand_in = spawn_gate::bind_tcp_std((Ipv6Addr::LOCALHOST, port));
    assert!(
        stand_in.is_err(),
        "another listener took [::1]:{port} beside the proxy: {stand_in:?}"
    );
    let said = answer(ask_at(
        IpAddr::V6(Ipv6Addr::LOCALHOST),
        port,
        &connect("example.com:443"),
    ));
    assert!(said.starts_with("HTTP/1.1 403 Forbidden\r\n"), "{said}");
    let held = let_go_block_port();
    let holder = spawn_gate::bind_tcp_std((Ipv6Addr::LOCALHOST, held)).unwrap();
    let refused = ModelProxy::bind(7, vec![host("api.anthropic.com", 443)], Some(held), &[]);
    assert_eq!(
        refused.as_ref().err().map(io::Error::kind),
        Some(io::ErrorKind::AddrInUse),
        "a saved port whose ::1 side another listener holds was taken: {refused:?}"
    );
    drop(holder);
}

#[test]
fn a_refusal_line_names_the_method_and_host_but_no_user_info_path_or_query() {
    for (request, shown) in [
        (
            "GET http://user:pass@api.example.com:8080/v1/x?token=secret HTTP/1.1",
            "method=GET host=api.example.com:8080",
        ),
        (
            "POST https://api.example.com/v1?key=secret HTTP/1.1",
            "method=POST host=api.example.com:443",
        ),
        (
            "CONNECT example.com:443 HTTP/1.1",
            "method=CONNECT host=example.com:443",
        ),
        (
            "CONNECT user:pass@example.com:443 HTTP/1.1",
            "method=CONNECT host=-",
        ),
        ("GET /v1?token=secret HTTP/1.1", "method=GET host=-"),
        ("token=secret", "method=- host=-"),
    ] {
        let line = refusal(7, request);
        assert_eq!(
            line,
            format!("model_proxy_refused agent=7 {shown}"),
            "{request}"
        );
        assert!(
            !line.contains("secret") && !line.contains("pass"),
            "{request}: {line}"
        );
    }
}

#[test]
fn a_tunnel_dials_no_address_on_this_machine_but_keeps_the_local_network() {
    for address in [
        "127.0.0.1",
        "127.8.9.10",
        "0.0.0.0",
        "0.1.2.3",
        "169.254.169.254",
        "::1",
        "::",
        "fe80::1",
        "::ffff:127.0.0.1",
        "::ffff:169.254.169.254",
        "::ffff:0.0.0.0",
        "::127.0.0.1",
    ] {
        assert!(!dialable(address.parse().unwrap()), "{address}");
    }
    for address in [
        "10.0.0.7",
        "172.16.4.2",
        "192.168.1.20",
        "100.64.0.1",
        "8.8.8.8",
        "fd12::7",
        "2001:db8::1",
        "::ffff:192.168.1.20",
    ] {
        assert!(dialable(address.parse().unwrap()), "{address}");
    }
}

/// The address this machine sends from toward the documentation network, found with a UDP
/// connect, which only picks a route and sends nothing. Output: the address, or none on a
/// machine with no such route.
fn routed_address() -> Option<Ipv4Addr> {
    let socket = spawn_gate::bind_udp_std((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// A host name may resolve to an address one of this machine's interfaces holds, such as its
/// own LAN address, which reaches the owner's services from outside the sandbox as loopback
/// does. The tunnel never dials one, also inside an IPv6 address, and an address no
/// interface holds stays dialable (S4B-I1).
#[test]
fn a_tunnel_dials_no_address_this_machines_interfaces_hold() {
    let mut held: Vec<IpAddr> = vec![Ipv4Addr::LOCALHOST.into(), Ipv6Addr::LOCALHOST.into()];
    if let Some(routed) = routed_address() {
        held.extend([IpAddr::V4(routed), IpAddr::V6(routed.to_ipv6_mapped())]);
    }
    for address in held {
        assert!(on_this_machine(address), "{address}");
    }
    for address in [
        "192.0.2.1",
        "198.51.100.7",
        "2001:db8::1",
        "::ffff:192.0.2.1",
    ] {
        assert!(!on_this_machine(address.parse().unwrap()), "{address}");
    }
}

#[test]
fn refusal_lines_show_the_first_ones_then_a_total_at_most_once_a_period() {
    assert_eq!(shown_refusal(1, Duration::ZERO), Shown::Line);
    assert_eq!(shown_refusal(REFUSALS_SHOWN, Duration::ZERO), Shown::Line);
    assert_eq!(
        shown_refusal(
            REFUSALS_SHOWN + 1,
            REFUSAL_TOTAL_EVERY - Duration::from_millis(1)
        ),
        Shown::Quiet
    );
    assert_eq!(
        shown_refusal(REFUSALS_SHOWN + 1, REFUSAL_TOTAL_EVERY),
        Shown::Total
    );
    assert_eq!(shown_refusal(1_000_000, Duration::ZERO), Shown::Quiet);
}

#[test]
fn the_proxy_counts_every_refused_request_and_cut_tunnel() {
    let proxy = ModelProxy::bind(7, vec![host("localhost", 4_433)], None, &[]).unwrap();
    let refused = REFUSALS_SHOWN + 5;
    for _ in 0..refused {
        let said = answer(ask(&proxy, &connect("example.com:443")));
        assert!(said.starts_with("HTTP/1.1 403 "), "{said}");
    }
    let mut client = ask(&proxy, &connect("localhost:4433"));
    assert_eq!(
        head(&mut client),
        "HTTP/1.1 200 Connection Established\r\n\r\n"
    );
    client.write_all(&hello(Some("example.com"))).unwrap();
    assert!(answer(client).is_empty());
    assert_eq!(proxy.refusals(), refused + 1);
}

/// Where a Linux sysctl lets programs bind any address, every address counts as held, so a
/// cut says the proxy cannot tell its own addresses rather than that the host is this
/// machine (S4B-I1-L1). The sysctl values are given; the real ones are never read here.
#[test]
fn a_cut_names_the_sysctl_that_lets_any_address_bind() {
    let reason = |values: [Option<&str>; 2]| {
        own_address_reason(&values.map(|value| value.map(str::to_owned)))
    };
    let named = "this machine lets programs bind any address, so CrazyAgent cannot tell which addresses are its own";
    assert_eq!(reason([None, None]), "resolves to this machine");
    assert_eq!(
        reason([Some("0\n"), Some("0\n")]),
        "resolves to this machine"
    );
    assert_eq!(reason([Some("x\n"), None]), "resolves to this machine");
    assert_eq!(reason([Some("1\n"), Some("0\n")]), named);
    assert_eq!(reason([None, Some("1\n")]), named);
}
