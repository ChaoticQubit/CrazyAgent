use super::*;

const PORT: u16 = 47_933;
const LABEL: &str = "0123456789abcdefghijklmnopqrstuv0123456789abcdefghij";
const WORKSPACE: &str = "/private/tmp/ide/p q/crazyagent.code-workspace";
const PINNED: &str = "/?workspace=/private/tmp/ide/p%20q/crazyagent.code-workspace";
const SERVER_POLICY: &str = "default-src 'self'; img-src 'self' https: data: blob:; media-src 'self'; script-src 'self' 'unsafe-eval' 'nonce-abc123' 'sha256-x'; child-src 'self'; frame-src 'self' https://*.vscode-cdn.net data:; worker-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; connect-src 'self' ws: wss: https:; font-src 'self' blob:; manifest-src 'self';";

fn token() -> String {
    "ab".repeat(super::super::token::TOKEN_BYTES)
}

fn routed(head: &str) -> Action {
    let request = parse(head.as_bytes()).expect(head);
    decide(&request, PORT, &token(), WORKSPACE)
}

fn unescaped(attribute: &str) -> String {
    attribute
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn settings(page: &str) -> serde_json::Value {
    let start = page.find(MARKER).unwrap() + MARKER.len();
    let end = start + page[start..].find('"').unwrap();
    serde_json::from_str(&unescaped(&page[start..end])).unwrap()
}

#[test]
fn webview_hosts_are_exactly_52_base32_hex_characters_under_localhost() {
    assert_eq!(LABEL.len(), WEBVIEW_LABEL_BYTES);
    assert!(webview_host(&format!("{LABEL}.localhost:{PORT}"), PORT));
    for host in [
        format!("{}.localhost:{PORT}", &LABEL[1..]),
        format!("{LABEL}0.localhost:{PORT}"),
        format!("{}w.localhost:{PORT}", &LABEL[1..]),
        format!("{}.localhost:{PORT}", LABEL.to_ascii_uppercase()),
        format!("{LABEL}.localhost:{}", PORT + 1),
        format!("x.{LABEL}.localhost:{PORT}"),
        format!("{LABEL}.localhost"),
        format!("{LABEL}.localhost.evil.example:{PORT}"),
        format!("127.0.0.1:{PORT}"),
        format!("localhost:{PORT}"),
    ] {
        assert!(!webview_host(&host, PORT), "{host}");
    }
}

#[test]
fn webview_hosts_get_only_the_three_pre_files_without_the_token() {
    let host = format!("Host: {LABEL}.localhost:{PORT}\r\n");
    assert_eq!(
        routed(&format!(
            "GET /index.html?id=1&swVersion=4 HTTP/1.1\r\n{host}\r\n"
        )),
        Action::Pre {
            name: "index.html",
            kind: "text/html; charset=utf-8",
            head_only: false
        }
    );
    assert_eq!(
        routed(&format!("GET /fake.html HTTP/1.1\r\n{host}\r\n")),
        Action::Pre {
            name: "fake.html",
            kind: "text/html; charset=utf-8",
            head_only: false
        }
    );
    assert_eq!(
        routed(&format!(
            "HEAD /service-worker.js?v=4 HTTP/1.1\r\n{host}\r\n"
        )),
        Action::Pre {
            name: "service-worker.js",
            kind: "text/javascript; charset=utf-8",
            head_only: true
        }
    );
    for path in [
        "/",
        "/host.js",
        "//index.html",
        "/../index.html",
        "/index.html/",
        "/%69ndex.html",
        "/out/server-main.js",
    ] {
        assert_eq!(
            routed(&format!("GET {path} HTTP/1.1\r\n{host}\r\n")),
            Action::Refuse("404 Not Found"),
            "{path}"
        );
    }
    assert_eq!(
        routed(&format!(
            "POST /index.html HTTP/1.1\r\n{host}Content-Length: 0\r\n\r\n"
        )),
        Action::Refuse("405 Method Not Allowed")
    );
}

#[test]
fn the_workbench_host_needs_the_token_in_the_query_or_the_cookie() {
    let token = token();
    let host = format!("Host: 127.0.0.1:{PORT}\r\n");
    assert_eq!(
        routed(&format!("GET / HTTP/1.1\r\n{host}\r\n")),
        Action::Refuse("403 Forbidden")
    );
    assert_eq!(
        routed(&format!(
            "GET {PINNED} HTTP/1.1\r\n{host}Cookie: a=b; {COOKIE}={token}; c=d\r\n\r\n"
        )),
        Action::Page
    );
    assert_eq!(
        routed(&format!("GET {PINNED}&tkn={token} HTTP/1.1\r\n{host}\r\n")),
        Action::Page
    );
    assert_eq!(
        routed(&format!(
            "GET /?folder=/p&tkn={token}#x HTTP/1.1\r\nHost: localhost:{PORT}\r\n\r\n"
        )),
        Action::Pin
    );
    let wrong = "cd".repeat(super::super::token::TOKEN_BYTES);
    for credential in [
        format!("Cookie: {COOKIE}={wrong}\r\n"),
        format!("Cookie: {COOKIE}={}\r\n", &token[2..]),
        format!("Cookie: {COOKIE}={token}0\r\n"),
        format!("Cookie: x{COOKIE}={token}\r\n"),
        format!("Cookie: {}={token}\r\n", COOKIE.to_ascii_uppercase()),
        format!("Authorization: Bearer {token}\r\n"),
    ] {
        assert_eq!(
            routed(&format!("GET / HTTP/1.1\r\n{host}{credential}\r\n")),
            Action::Refuse("403 Forbidden"),
            "{credential}"
        );
    }
    assert_eq!(
        routed(&format!("GET /?xtkn={token} HTTP/1.1\r\n{host}\r\n")),
        Action::Refuse("403 Forbidden")
    );
    assert_eq!(
        routed(&format!("GET /#tkn={token} HTTP/1.1\r\n{host}\r\n")),
        Action::Refuse("403 Forbidden")
    );
    for other in [
        format!("evil.example:{PORT}"),
        format!("127.0.0.1:{}", PORT + 1),
        "127.0.0.1".to_owned(),
        format!("[::1]:{PORT}"),
        format!("abc.localhost:{PORT}"),
    ] {
        assert_eq!(
            routed(&format!(
                "GET / HTTP/1.1\r\nHost: {other}\r\nCookie: {COOKIE}={token}\r\n\r\n"
            )),
            Action::Refuse("403 Forbidden"),
            "{other}"
        );
    }
    assert_eq!(
        routed(&format!(
            "GET / HTTP/1.1\r\n{host}Host: evil.example:{PORT}\r\nCookie: {COOKIE}={token}\r\n\r\n"
        )),
        Action::Refuse("400 Bad Request")
    );
    assert_eq!(
        routed(&format!(
            "GET / HTTP/1.1\r\nCookie: {COOKIE}={token}\r\n\r\n"
        )),
        Action::Refuse("400 Bad Request")
    );
}

#[test]
fn authorized_requests_are_forwarded_upgraded_or_refused_by_their_framing() {
    let token = token();
    let head = |start: &str, extra: &str| {
        format!(
            "{start} HTTP/1.1\r\nHost: 127.0.0.1:{PORT}\r\nCookie: {COOKIE}={token}\r\n{extra}\r\n"
        )
    };
    assert_eq!(
        routed(&head("GET /stable-x/static/out/vs/workbench.js", "")),
        Action::Forward { body: 0 }
    );
    assert_eq!(
        routed(&head(
            "POST /vscode-remote-resource",
            "Content-Length: 5\r\n"
        )),
        Action::Forward { body: 5 }
    );
    assert_eq!(
        routed(&head("POST /x", "Transfer-Encoding: chunked\r\n")),
        Action::Refuse("411 Length Required")
    );
    assert_eq!(
        routed(&head(
            "POST /x",
            "Content-Length: 5\r\nContent-Length: 5\r\n"
        )),
        Action::Refuse("400 Bad Request")
    );
    for length in ["+5", "-1", "5 5", "0x5", ""] {
        let extra = format!("Content-Length: {length}\r\n");
        assert_eq!(
            routed(&head("POST /x", &extra)),
            Action::Refuse("400 Bad Request"),
            "{length:?}"
        );
    }
    assert_eq!(
        routed(&head(
            "POST /x",
            &format!("Content-Length: {}\r\n", BODY_BYTES_MAX + 1)
        )),
        Action::Refuse("413 Content Too Large")
    );
    let socket = "Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n";
    assert_eq!(
        routed(&head(
            "GET /?reconnectionToken=r&skipWebSocketFrames=false",
            socket
        )),
        Action::Upgrade
    );
    assert_eq!(
        routed(&head("GET /", &socket.replace("websocket", "WebSocket"))),
        Action::Upgrade
    );
    assert_eq!(
        routed(&head("POST /", socket)),
        Action::Refuse("400 Bad Request")
    );
    assert_eq!(
        routed(&format!(
            "GET / HTTP/1.1\r\nHost: 127.0.0.1:{PORT}\r\n{socket}\r\n"
        )),
        Action::Refuse("403 Forbidden")
    );
    assert_eq!(
        routed(&head("GET /", "Content-Length: 3\r\n")),
        Action::Refuse("400 Bad Request")
    );
    assert_eq!(
        routed(&head(&format!("GET {PINNED}"), "Upgrade: h2c\r\n")),
        Action::Page
    );
}

#[test]
fn malformed_heads_are_not_parsed() {
    let host = format!("Host: 127.0.0.1:{PORT}\r\n");
    let too_many: String = (0..=HEADERS_MAX)
        .map(|index| format!("X-{index}: v\r\n"))
        .collect();
    for head in [
        format!("GET / HTTP/1.1\r\n{host}X: a\nInjected: b\r\n\r\n"),
        format!("GET / HTTP/1.1\r\n{host}X: a\rb\r\n\r\n"),
        format!("GET / HTTP/1.1\r\n{host}X: a\0b\r\n\r\n"),
        format!("GET / HTTP/1.1\r\n{host}Bad Name: v\r\n\r\n"),
        format!("GET / HTTP/1.1\r\n{host} folded: v\r\n\r\n"),
        format!("GET / HTTP/1.1\r\n{host}NoColon\r\n\r\n"),
        format!("GET / HTTP/2.0\r\n{host}\r\n"),
        format!("GET / HTTP/1.1 extra\r\n{host}\r\n"),
        format!("get / HTTP/1.1\r\n{host}\r\n"),
        format!("GET http://evil.example/ HTTP/1.1\r\n{host}\r\n"),
        format!("GET * HTTP/1.1\r\n{host}\r\n"),
        format!("GET /\t HTTP/1.1\r\n{host}\r\n"),
        format!("GET / HTTP/1.1\r\n{host}"),
        format!("GET / HTTP/1.1\r\n{host}{too_many}\r\n"),
    ] {
        assert!(parse(head.as_bytes()).is_none(), "{head:?}");
    }
    assert!(parse(&[0xff, b'\r', b'\n', b'\r', b'\n']).is_none());
    let tabbed = format!("GET /a?b HTTP/1.0\r\n{host}X-Tab:\tv \r\n\r\n");
    let request = parse(tabbed.as_bytes()).unwrap();
    assert_eq!(
        (request.method, request.target, request.version),
        ("GET", "/a?b", "HTTP/1.0")
    );
    assert_eq!(request.headers[1], ("X-Tab", "v"));
}

#[test]
fn forwarded_heads_carry_one_request_and_close() {
    let token = token();
    let head = format!(
        "GET /?tkn={token} HTTP/1.1\r\nHost: 127.0.0.1:{PORT}\r\nConnection: keep-alive, Upgrade\r\nKeep-Alive: timeout=5\r\nProxy-Connection: keep-alive\r\nTE: trailers\r\nUpgrade: h2c\r\nAccept-Encoding: gzip, br\r\nIf-None-Match: \"x\"\r\nCookie: {COOKIE}={token}\r\n\r\n"
    );
    let request = parse(head.as_bytes()).unwrap();
    let plain = String::from_utf8(forwarded(&request, request.version, &[])).unwrap();
    assert_eq!(
        plain,
        format!(
            "GET /?tkn={token} HTTP/1.1\r\nHost: 127.0.0.1:{PORT}\r\nAccept-Encoding: gzip, br\r\nIf-None-Match: \"x\"\r\nCookie: {COOKIE}={token}\r\nConnection: close\r\n\r\n"
        )
    );
    let page = String::from_utf8(forwarded(&request, "HTTP/1.0", &PAGE_DROPPED)).unwrap();
    assert_eq!(
        page,
        format!(
            "GET /?tkn={token} HTTP/1.0\r\nHost: 127.0.0.1:{PORT}\r\nCookie: {COOKIE}={token}\r\nConnection: close\r\n\r\n"
        )
    );
}

#[test]
fn the_server_policy_is_closed_to_the_internet() {
    let host = format!("127.0.0.1:{PORT}");
    assert_eq!(
        tighten(SERVER_POLICY, PORT, &host),
        format!(
            "default-src 'self'; img-src 'self' data: blob:; media-src 'self'; script-src 'self' 'unsafe-eval' 'nonce-abc123' 'sha256-x'; child-src 'self'; frame-src 'self' http://*.localhost:{PORT} data:; worker-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; connect-src 'self' ws://{host}; font-src 'self' blob:; manifest-src 'self';"
        )
    );
    assert_eq!(
        tighten("", PORT, &host),
        format!(
            "frame-src 'self' http://*.localhost:{PORT} data:; img-src 'self' data: blob:; connect-src 'self' ws://{host};"
        )
    );
    assert_eq!(
        tighten(
            "CONNECT-SRC https:; connect-src *; default-src 'none'",
            PORT,
            &host
        ),
        format!(
            "connect-src 'self' ws://{host}; default-src 'none'; frame-src 'self' http://*.localhost:{PORT} data:; img-src 'self' data: blob:;"
        )
    );
}

#[test]
fn the_page_configuration_gets_the_webview_endpoint_and_the_defaults_last() {
    let page = format!(
        "<!DOCTYPE html><html><head><meta {MARKER}{{&quot;remoteAuthority&quot;:&quot;127.0.0.1:{PORT}&quot;,&quot;webviewEndpoint&quot;:&quot;https://cdn.example/&quot;}}\"><meta id=\"vscode-workbench-auth-session\" data-settings=\"\"></head></html>"
    );
    let injected = inject(&page, PORT).unwrap();
    let decoded = settings(&injected);
    assert_eq!(decoded["remoteAuthority"], format!("127.0.0.1:{PORT}"));
    assert_eq!(decoded["webviewEndpoint"], template(PORT));
    assert_eq!(decoded["configurationDefaults"], configuration_defaults());
    assert_eq!(
        configuration_defaults(),
        serde_json::Value::Object(crate::ide::settings::defaults())
    );
    for (key, value) in [
        ("workbench.startupEditor", serde_json::json!("none")),
        (
            "workbench.welcomePage.extraAnnouncements",
            serde_json::json!(false),
        ),
        ("task.allowAutomaticTasks", serde_json::json!("off")),
        ("chat.disableAIFeatures", serde_json::json!(true)),
    ] {
        assert_eq!(decoded["configurationDefaults"][key], value, "{key}");
    }
    let attribute = &injected[injected.find(MARKER).unwrap() + MARKER.len()..];
    let attribute = &attribute[..attribute.find('"').unwrap()];
    assert!(
        attribute.ends_with(&format!(
            "&quot;webviewEndpoint&quot;:&quot;http://{{{{uuid}}}}.localhost:{PORT}/&quot;}}"
        )),
        "{attribute}"
    );
    assert!(
        injected.ends_with(
            "<meta id=\"vscode-workbench-auth-session\" data-settings=\"\"></head></html>"
        )
    );
    let empty = inject(&format!("<meta {MARKER}{{}}\">"), PORT).unwrap();
    assert_eq!(settings(&empty)["webviewEndpoint"], template(PORT));
    for refused in [
        format!("<meta {MARKER}{{{{WORKBENCH_WEB_CONFIGURATION}}}}\">"),
        format!("<meta {MARKER}{{}}\"><meta {MARKER}{{}}\">"),
        format!("<meta {MARKER}[]\">"),
        format!("<meta {MARKER}{{}}"),
        "<html></html>".to_owned(),
    ] {
        assert!(inject(&refused, PORT).is_none(), "{refused}");
    }
    assert_eq!(
        template(PORT),
        format!("http://{{{{uuid}}}}.localhost:{PORT}/")
    );
}

#[test]
fn only_a_whole_html_200_page_is_rewritten() {
    let host = format!("127.0.0.1:{PORT}");
    let body = format!("<meta {MARKER}{{&quot;a&quot;:1}}\">");
    let response = |extra: &str, body: &str| {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    };
    let raw = response(
        &format!("Content-Security-Policy: {SERVER_POLICY}\r\n"),
        &body,
    );
    let reply = String::from_utf8(page_reply(raw.as_bytes(), PORT, &host)).unwrap();
    let (head, page) = reply.split_once("\r\n\r\n").unwrap();
    assert_eq!(page, inject(&body, PORT).unwrap());
    assert_eq!(
        head,
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Security-Policy: {}\r\nContent-Length: {}\r\nConnection: close",
            tighten(SERVER_POLICY, PORT, &host),
            page.len()
        )
    );
    let bare = String::from_utf8(page_reply(response("", &body).as_bytes(), PORT, &host)).unwrap();
    assert!(bare.contains(&format!(
        "\r\nContent-Security-Policy: {}\r\n",
        tighten("", PORT, &host)
    )));
    for passed in [
        "HTTP/1.1 302 Found\r\nLocation: /\r\nSet-Cookie: vscode-tkn=x\r\nContent-Length: 0\r\n\r\n".to_owned(),
        "HTTP/1.1 403 Forbidden\r\nContent-Type: text/html\r\nContent-Length: 10\r\n\r\nForbidden.".to_owned(),
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}".to_owned(),
    ] {
        assert_eq!(page_reply(passed.as_bytes(), PORT, &host), passed.as_bytes());
    }
    let gateway = refusal("502 Bad Gateway");
    for failed in [
        response("Transfer-Encoding: chunked\r\n", &body),
        response("Content-Encoding: gzip\r\n", &body),
        response("", "<html>no configuration</html>"),
        response("", &body).replace(
            &format!("Content-Length: {}", body.len()),
            "Content-Length: 1",
        ),
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n".to_owned(),
    ] {
        assert_eq!(
            page_reply(failed.as_bytes(), PORT, &host),
            gateway,
            "{failed}"
        );
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn the_proxy_serves_webview_files_without_the_token_and_nothing_else() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = tempfile::Builder::new()
        .prefix("ca-proxy-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    fs::set_permissions(scratch.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let pre = scratch.path().join("pre");
    fs::create_dir(&pre).unwrap();
    fs::write(pre.join("index.html"), "<!doctype html><p>host</p>").unwrap();
    fs::write(
        pre.join("service-worker.js"),
        "self.addEventListener('fetch', () => {});",
    )
    .unwrap();
    fs::write(pre.join("host.js"), "not served").unwrap();
    fs::write(scratch.path().join("secret.txt"), "engine secret").unwrap();
    std::os::unix::fs::symlink(scratch.path().join("secret.txt"), pre.join("fake.html")).unwrap();
    let closed = crate::spawn_gate::bind_tcp_std(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let (listener, port) = bind(None).await.unwrap();
    assert_ne!(port, closed);
    let token = token();
    let proxy = tokio::spawn(serve(
        listener,
        Site {
            port,
            upstream: closed,
            pre: pre.clone(),
            token: token.clone(),
            workspace: WORKSPACE.to_owned(),
        },
    ));
    assert_eq!(
        crate::ide::server::listeners(port).unwrap(),
        vec![(std::process::id() as i32, format!("127.0.0.1:{port}"))]
    );
    let exchange = move |head: String| async move {
        let mut stream = crate::spawn_gate::connect_tcp(("127.0.0.1", port))
            .await
            .unwrap();
        let _ = stream.write_all(head.as_bytes()).await;
        let mut reply = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(20), stream.read_to_end(&mut reply))
            .await
            .unwrap();
        String::from_utf8_lossy(&reply).into_owned()
    };
    let webview = format!("{LABEL}.localhost:{port}");
    let page = exchange(format!(
        "GET /index.html?id=1 HTTP/1.1\r\nHost: {webview}\r\n\r\n"
    ))
    .await;
    assert!(page.starts_with("HTTP/1.1 200 OK\r\n"), "{page}");
    assert!(page.contains("Content-Type: text/html; charset=utf-8\r\n"));
    assert!(page.contains("X-Content-Type-Options: nosniff\r\n"));
    assert!(page.ends_with("\r\n\r\n<!doctype html><p>host</p>"));
    let worker = exchange(format!(
        "HEAD /service-worker.js HTTP/1.1\r\nHost: {webview}\r\n\r\n"
    ))
    .await;
    assert!(worker.starts_with("HTTP/1.1 200 OK\r\n"), "{worker}");
    assert!(worker.contains("Content-Type: text/javascript; charset=utf-8\r\n"));
    assert!(worker.ends_with("\r\n\r\n"));
    for (head, status) in [
        (
            format!("GET /fake.html HTTP/1.1\r\nHost: {webview}\r\n\r\n"),
            "404 Not Found",
        ),
        (
            format!("GET /host.js HTTP/1.1\r\nHost: {webview}\r\n\r\n"),
            "404 Not Found",
        ),
        (
            format!("GET /../secret.txt HTTP/1.1\r\nHost: {webview}\r\n\r\n"),
            "404 Not Found",
        ),
        (
            format!("GET /index.html HTTP/1.1\r\nHost: abc.localhost:{port}\r\n\r\n"),
            "403 Forbidden",
        ),
        (
            format!("GET /index.html HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
            "403 Forbidden",
        ),
        (
            format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
            "403 Forbidden",
        ),
        (
            format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: {COOKIE}={token}\r\n\r\n"),
            "302 Found",
        ),
        (
            format!(
                "GET {PINNED} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: {COOKIE}={token}\r\n\r\n"
            ),
            "502 Bad Gateway",
        ),
        (
            format!(
                "GET /x.js HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: {COOKIE}={token}\r\n\r\n"
            ),
            "502 Bad Gateway",
        ),
        (
            format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX: a\nY: b\r\n\r\n"),
            "400 Bad Request",
        ),
    ] {
        let reply = exchange(head.clone()).await;
        assert!(
            reply.starts_with(&format!("HTTP/1.1 {status}\r\n")),
            "{head:?}: {reply}"
        );
        assert!(!reply.contains("engine secret") && !reply.contains("not served"));
    }
    let long = exchange(format!(
        "GET /index.html HTTP/1.1\r\nHost: {webview}\r\nX-Long: {}\r\n\r\n",
        "a".repeat(HEAD_BYTES_MAX)
    ))
    .await;
    assert!(long.is_empty(), "{long}");
    proxy.abort();
    let _ = proxy.await;
    assert!(crate::ide::server::listeners(port).unwrap().is_empty());
}

#[test]
fn the_workbench_opens_only_the_engine_workspace() {
    let token = token();
    let page = |target: &str| {
        format!(
            "GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{PORT}\r\nCookie: {COOKIE}={token}\r\n\r\n"
        )
    };
    for target in [
        PINNED.to_owned(),
        format!("{PINNED}#frag"),
        "/?workspace=/private/tmp/ide/p+q/crazyagent.code-workspace".to_owned(),
        "/?workspace=%2Fprivate%2Ftmp%2Fide%2Fp%20q%2Fcrazyagent.code-workspace".to_owned(),
        format!("/?tkn={token}&workspace=/private/tmp/ide/p%20q/crazyagent.code-workspace"),
    ] {
        assert_eq!(routed(&page(&target)), Action::Page, "{target}");
    }
    for target in [
        "/".to_owned(),
        "/?".to_owned(),
        "/?folder=/private/tmp/ide/p%20q".to_owned(),
        "/?workspace=/private/tmp/other.code-workspace".to_owned(),
        "/?workspace=/private/tmp/ide/p%20q/crazyagent.code-workspace.x".to_owned(),
        format!("{PINNED}&workspace=/private/tmp/other.code-workspace"),
        format!("{PINNED}&{}", &PINNED[2..]),
        format!("{PINNED}&folder=/private/tmp"),
        format!("{PINNED}&ew=true"),
        format!("{PINNED}&payload=%5B%5D"),
        "/?workspace=/private/tmp/ide/p%ZZq/crazyagent.code-workspace".to_owned(),
        "/?workspace=/private/tmp/ide/p%FFq/crazyagent.code-workspace".to_owned(),
        "/?ew=true".to_owned(),
    ] {
        assert_eq!(routed(&page(&target)), Action::Pin, "{target}");
    }
    let head = page("/?folder=/elsewhere");
    let request = parse(head.as_bytes()).unwrap();
    assert_eq!(
        String::from_utf8(pin_reply(&request, WORKSPACE)).unwrap(),
        format!(
            "HTTP/1.1 302 Found\r\nLocation: {PINNED}\r\nContent-Length: 0\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
        )
    );
    let first = format!("GET /?tkn={token}&folder=/x HTTP/1.1\r\nHost: 127.0.0.1:{PORT}\r\n\r\n");
    let request = parse(first.as_bytes()).unwrap();
    assert_eq!(decide(&request, PORT, &token, WORKSPACE), Action::Pin);
    let reply = String::from_utf8(pin_reply(&request, WORKSPACE)).unwrap();
    assert!(
        reply.contains(&format!("\r\nLocation: /?tkn={token}&workspace=/private/tmp/ide/p%20q/crazyagent.code-workspace\r\n")),
        "{reply}"
    );
    assert_eq!(encoded("/a b/c%d+e~f"), "/a%20b/c%25d%2Be~f");
    assert_eq!(
        decoded("/a%20b/c%25d%2Be~f+g").as_deref(),
        Some("/a b/c%d+e~f g")
    );
    assert_eq!(decoded("%2"), None);
}
