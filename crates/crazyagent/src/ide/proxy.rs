use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The folder below a VS Code build's root whose files every webview loads first: its host
/// page, its fallback page and its service worker.
pub const PRE: &str = "out/vs/workbench/contrib/webview/browser/pre";

/// The files of [`PRE`] a webview host is served, each with its type; nothing else is.
pub const PRE_FILES: [(&str, &str); 3] = [
    ("index.html", "text/html; charset=utf-8"),
    ("fake.html", "text/html; charset=utf-8"),
    ("service-worker.js", "text/javascript; charset=utf-8"),
];

/// The cookie the IDE server sets from `/?tkn=` and reads its connection token from.
pub const COOKIE: &str = "vscode-tkn";

const MARKER: &str = "id=\"vscode-workbench-web-configuration\" data-settings=\"";
const WEBVIEW_LABEL_BYTES: usize = 52;
const HEAD_BYTES_MAX: usize = 16 * 1024;
const HEADERS_MAX: usize = 128;
const CANDIDATES_MAX: usize = 16;
const PAGE_BYTES_MAX: usize = 4 * 1024 * 1024;
const FILE_BYTES_MAX: u64 = 1024 * 1024;
const BODY_BYTES_MAX: u64 = 64 * 1024 * 1024;
const CONNECTIONS_MAX: usize = 128;
const READ_TIME: Duration = Duration::from_secs(5);
const CONNECT_TIME: Duration = Duration::from_secs(5);
const PAGE_TIME: Duration = Duration::from_secs(30);
const SEND_TIME: Duration = Duration::from_secs(30);
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);
const HOP_HEADERS: [&str; 7] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "upgrade",
    "transfer-encoding",
];
const PAGE_DROPPED: [&str; 5] = [
    "accept-encoding",
    "if-none-match",
    "if-modified-since",
    "if-range",
    "range",
];

/// What one project's proxy serves: its own loopback port, the IDE server's port behind it,
/// the build's [`PRE`] folder, the launch's connection token, which never leaves the engine
/// except toward the server, and the engine's workspace file the workbench is pinned to.
pub struct Site {
    pub port: u16,
    pub upstream: u16,
    pub pre: PathBuf,
    pub token: String,
    pub workspace: String,
}

/// The template the workbench builds each webview's origin from: every webview gets its own
/// `<hash>.localhost` host on the proxy port, so webviews stay apart from the workbench and
/// from each other and nothing loads from the internet. Input: the proxy port. Output: the
/// template, with `{{uuid}}` left for the workbench to fill.
pub fn template(port: u16) -> String {
    format!("http://{{{{uuid}}}}.localhost:{port}/")
}

/// The settings the workbench starts from before any settings file loads, injected into its
/// page: [`super::settings::defaults`], every locked key and the themes, so no Welcome page,
/// announcements fetch or automatic task runs before Remote settings load. Output: the
/// object.
pub fn configuration_defaults() -> serde_json::Value {
    serde_json::Value::Object(super::settings::defaults())
}

/// Binds the proxy's loopback listener: on the recorded port while it is free, else on one
/// the system picks. Input: the recorded port, if any. Output: the listener and its port.
pub async fn bind(recorded: Option<u16>) -> io::Result<(TcpListener, u16)> {
    if let Some(port) = recorded.filter(|port| *port != 0)
        && let Ok(listener) = crate::spawn_gate::bind_tcp(("127.0.0.1", port)).await
    {
        return Ok((listener, port));
    }
    let listener = crate::spawn_gate::bind_tcp(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    assert_ne!(port, 0);
    Ok((listener, port))
}

/// Serves one project's IDE on its proxy port until the task is aborted. A webview host
/// `<52 base32 hex>.localhost:<port>` gets only the [`PRE_FILES`], without the token. The
/// workbench host `127.0.0.1:<port>` or `localhost:<port>` needs the token in the `tkn`
/// query or the [`COOKIE`]; its page opens only the engine's workspace ([`pinned`]) and gets
/// [`inject`] and [`tighten`], WebSocket upgrades pass through, and every other request goes
/// to the server one per connection. Any other host is refused. At most 128 connections are served at once; more are closed. Inputs:
/// the bound listener and the site.
pub async fn serve(listener: TcpListener, site: Site) {
    assert!(site.port != 0 && site.upstream != 0 && site.port != site.upstream);
    assert_eq!(site.token.len(), 2 * super::token::TOKEN_BYTES);
    assert!(site.workspace.starts_with('/'));
    let site = Arc::new(site);
    let mut connections = tokio::task::JoinSet::new();
    loop {
        while connections.try_join_next().is_some() {}
        let stream = match crate::spawn_gate::accept_tcp(&listener).await {
            Ok((stream, _)) => stream,
            Err(_) => {
                tokio::time::sleep(ACCEPT_BACKOFF).await;
                continue;
            }
        };
        if connections.len() >= CONNECTIONS_MAX {
            continue;
        }
        connections.spawn(connection(stream, site.clone()));
        assert!(connections.len() <= CONNECTIONS_MAX);
    }
}

/// One parsed request head: its start line and its header lines in order.
#[derive(Debug)]
pub(crate) struct Request<'a> {
    pub(crate) method: &'a str,
    pub(crate) target: &'a str,
    pub(crate) version: &'a str,
    pub(crate) headers: Vec<(&'a str, &'a str)>,
}

impl Request<'_> {
    fn values<'b>(&'b self, name: &'b str) -> impl Iterator<Item = &'b str> + 'b {
        self.headers
            .iter()
            .filter(move |(known, _)| known.eq_ignore_ascii_case(name))
            .map(|(_, value)| *value)
    }

    fn path(&self) -> &str {
        self.target.split(['?', '#']).next().unwrap_or_default()
    }
}

/// What the proxy does with one request.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Refuse(&'static str),
    Pre {
        name: &'static str,
        kind: &'static str,
        head_only: bool,
    },
    Page,
    Pin,
    Upgrade,
    Forward {
        body: u64,
    },
}

/// Parses a request head that ends with its blank line. Header lines must be plain names
/// and values without a bare line feed, so the head the server sees is the head checked
/// here. Input: the head bytes. Output: the request, or none for anything malformed.
pub(crate) fn parse(head: &[u8]) -> Option<Request<'_>> {
    let text = std::str::from_utf8(head).ok()?.strip_suffix("\r\n\r\n")?;
    let mut lines = text.split("\r\n");
    let start = lines.next()?;
    if start.bytes().any(|byte| byte.is_ascii_control()) {
        return None;
    }
    let mut parts = start.split(' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some()
        || method.is_empty()
        || !method.bytes().all(|byte| byte.is_ascii_uppercase())
        || !target.starts_with('/')
        || !matches!(version, "HTTP/1.0" | "HTTP/1.1")
    {
        return None;
    }
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line.split_once(':')?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
            || value
                .bytes()
                .any(|byte| byte.is_ascii_control() && byte != b'\t')
            || headers.len() == HEADERS_MAX
        {
            return None;
        }
        headers.push((name, value.trim_matches([' ', '\t'])));
    }
    Some(Request {
        method,
        target,
        version,
        headers,
    })
}

/// Decides what to do with one request on a proxy port. A workbench page that opens anything
/// but the engine's workspace is sent to it instead ([`pinned`]). Inputs: the request, the
/// proxy port, the launch token and the engine's workspace file. Output: the action; a
/// refusal names its status line.
pub(crate) fn decide(request: &Request<'_>, port: u16, token: &str, workspace: &str) -> Action {
    let hosts: Vec<&str> = request.values("host").collect();
    let [host] = hosts[..] else {
        return Action::Refuse("400 Bad Request");
    };
    if webview_host(host, port) {
        let head_only = request.method == "HEAD";
        if request.method != "GET" && !head_only {
            return Action::Refuse("405 Method Not Allowed");
        }
        let name = request.path().strip_prefix('/');
        return match PRE_FILES.iter().find(|(file, _)| Some(*file) == name) {
            Some(&(name, kind)) => Action::Pre {
                name,
                kind,
                head_only,
            },
            None => Action::Refuse("404 Not Found"),
        };
    }
    if !workbench_host(host, port) || !carries(request, token) {
        return Action::Refuse("403 Forbidden");
    }
    if request.values("transfer-encoding").next().is_some() {
        return Action::Refuse("411 Length Required");
    }
    let lengths: Vec<&str> = request.values("content-length").collect();
    let body = match lengths[..] {
        [] => 0,
        [length] if !length.is_empty() && length.bytes().all(|byte| byte.is_ascii_digit()) => {
            match length.parse::<u64>() {
                Ok(body) if body <= BODY_BYTES_MAX => body,
                _ => return Action::Refuse("413 Content Too Large"),
            }
        }
        _ => return Action::Refuse("400 Bad Request"),
    };
    let upgrade = request
        .values("upgrade")
        .any(|value| value.eq_ignore_ascii_case("websocket"));
    if upgrade || (request.method == "GET" && request.path() == "/") {
        if request.method != "GET" || body != 0 {
            return Action::Refuse("400 Bad Request");
        }
        return if upgrade {
            Action::Upgrade
        } else if pinned(request, workspace) {
            Action::Page
        } else {
            Action::Pin
        };
    }
    Action::Forward { body }
}

/// Tells whether a Host header names a webview origin on this proxy port: exactly 52 base32
/// hex characters, `0-9` and `a-v`, under `localhost`. Inputs: the header value and the
/// port. Output: the answer.
pub(crate) fn webview_host(host: &str, port: u16) -> bool {
    host.strip_suffix(&format!(".localhost:{port}"))
        .is_some_and(|label| {
            label.len() == WEBVIEW_LABEL_BYTES
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'v').contains(&byte))
        })
}

fn workbench_host(host: &str, port: u16) -> bool {
    host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
}

/// Tells whether a request carries the launch token, in its `tkn` query parameter or its
/// [`COOKIE`], compared in constant time; at most 16 candidates are tried. Inputs: the
/// request and the token. Output: the answer.
pub(crate) fn carries(request: &Request<'_>, token: &str) -> bool {
    assert!(!token.is_empty());
    let query = request
        .target
        .split('#')
        .next()
        .and_then(|target| target.split_once('?'))
        .map(|(_, query)| query)
        .unwrap_or_default();
    let queried = query
        .split('&')
        .filter_map(|pair| pair.strip_prefix("tkn="));
    let cookie = format!("{COOKIE}=");
    let cookies = request
        .values("cookie")
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().strip_prefix(cookie.as_str()));
    let mut found = false;
    for candidate in queried.chain(cookies).take(CANDIDATES_MAX) {
        found |= bool::from(candidate.as_bytes().ct_eq(token.as_bytes()));
    }
    found
}

/// Tells whether a workbench page request opens exactly the engine's workspace: one
/// `workspace` parameter naming it, besides the `tkn` parameter, and no `folder`, empty
/// window or other parameter, so neither an extension nor the owner can open another folder
/// or a workspace file written elsewhere. Inputs: the request and the workspace file.
/// Output: the answer.
pub(crate) fn pinned(request: &Request<'_>, workspace: &str) -> bool {
    assert!(workspace.starts_with('/'));
    let query = request
        .target
        .split('#')
        .next()
        .and_then(|target| target.split_once('?'))
        .map(|(_, query)| query)
        .unwrap_or_default();
    let mut named = 0;
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match (decoded(key).as_deref(), decoded(value)) {
            (Some("tkn"), _) => {}
            (Some("workspace"), Some(value)) if value == workspace => named += 1,
            _ => return false,
        }
    }
    named == 1
}

/// The page the workbench is sent to when it asks for anything but the engine's workspace:
/// the workspace, keeping a `tkn` parameter the request carried so the server still sets the
/// session cookie. Inputs: the request and the workspace file. Output: the reply bytes.
pub(crate) fn pin_reply(request: &Request<'_>, workspace: &str) -> Vec<u8> {
    let tkn = request
        .target
        .split('#')
        .next()
        .and_then(|target| target.split_once('?'))
        .map(|(_, query)| query)
        .unwrap_or_default()
        .split('&')
        .find(|pair| pair.starts_with("tkn="));
    let location = match tkn {
        Some(pair) => format!("/?{pair}&workspace={}", encoded(workspace)),
        None => format!("/?workspace={}", encoded(workspace)),
    };
    assert!(!location.contains(['\r', '\n']));
    format!(
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
    )
    .into_bytes()
}

/// Adds the proxy's webview endpoint and [`configuration_defaults`] to the workbench page's
/// configuration, after the server's own members so they win. The page must hold the
/// configuration meta once, with the server's escaped object. Inputs: the page and the proxy
/// port. Output: the new page, or none when the page is not the expected one.
pub(crate) fn inject(page: &str, port: u16) -> Option<String> {
    let mut found = page.match_indices(MARKER);
    let (at, _) = found.next()?;
    if found.next().is_some() {
        return None;
    }
    let start = at + MARKER.len();
    let end = start + page[start..].find('"')?;
    let inner = page[start..end].strip_prefix('{')?.strip_suffix('}')?;
    if !inner.is_empty() && !inner.starts_with("&quot;") {
        return None;
    }
    let added = serde_json::json!({
        "webviewEndpoint": template(port),
        "configurationDefaults": configuration_defaults(),
    })
    .to_string();
    let members = escape(&added[1..added.len() - 1]);
    let joined = if inner.is_empty() {
        format!("{{{members}}}")
    } else {
        format!("{{{inner},{members}}}")
    };
    let injected = format!("{}{joined}{}", &page[..start], &page[end..]);
    assert_eq!(injected.matches(MARKER).count(), 1);
    Some(injected)
}

/// Rewrites the workbench page's Content-Security-Policy for the proxy: frames from the
/// webview hosts on this port only, images from the page and inline data only, and
/// connections to the page and its WebSocket only, so the workbench fetches nothing from
/// the internet. Other directives are kept. Inputs: the server's policy, the proxy port and
/// the page's Host. Output: the policy.
pub fn tighten(policy: &str, port: u16, host: &str) -> String {
    let set = [
        format!("frame-src 'self' http://*.localhost:{port} data:"),
        "img-src 'self' data: blob:".to_owned(),
        format!("connect-src 'self' ws://{host}"),
    ];
    let mut kept: Vec<String> = Vec::new();
    for directive in policy
        .split(';')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let name = directive_name(directive);
        let replaced = set
            .iter()
            .find(|item| directive_name(item) == name)
            .cloned()
            .unwrap_or_else(|| directive.to_owned());
        if !kept.iter().any(|known| directive_name(known) == name) {
            kept.push(replaced);
        }
    }
    for item in &set {
        if !kept.contains(item) {
            kept.push(item.clone());
        }
    }
    assert!(set.iter().all(|item| kept.contains(item)));
    format!("{};", kept.join("; "))
}

/// Builds the reply sent for the workbench page from the server's whole response: a 200
/// HTML page gets [`inject`], [`tighten`] and its new length; any other response passes
/// unchanged. A page that cannot be rewritten is a 502, so no page leaves with the
/// server's own policy. Inputs: the response bytes, the proxy port and the page's Host.
/// Output: the reply bytes.
pub(crate) fn page_reply(raw: &[u8], port: u16, host: &str) -> Vec<u8> {
    let Some(split) = find(raw, b"\r\n\r\n") else {
        return refusal("502 Bad Gateway");
    };
    let (head, body) = raw.split_at(split + 4);
    let Some(head) = std::str::from_utf8(head)
        .ok()
        .and_then(|head| head.strip_suffix("\r\n\r\n"))
    else {
        return refusal("502 Bad Gateway");
    };
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or_default();
    let headers: Vec<(&str, &str)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name, value.trim()))
        .collect();
    let values = |name: &'static str| {
        headers
            .iter()
            .filter(move |(known, _)| known.eq_ignore_ascii_case(name))
            .map(|(_, value)| *value)
    };
    let html =
        values("content-type").any(|kind| kind.to_ascii_lowercase().starts_with("text/html"));
    if status.split(' ').nth(1) != Some("200") || !html {
        return raw.to_vec();
    }
    let length_fits =
        values("content-length").all(|length| length.parse::<usize>().ok() == Some(body.len()));
    if values("transfer-encoding").next().is_some()
        || values("content-encoding").next().is_some()
        || !length_fits
    {
        return refusal("502 Bad Gateway");
    }
    let Some(page) = std::str::from_utf8(body)
        .ok()
        .and_then(|page| inject(page, port))
    else {
        return refusal("502 Bad Gateway");
    };
    let mut reply = format!("{status}\r\n");
    let mut policed = false;
    for (name, value) in &headers {
        let lower = name.to_ascii_lowercase();
        if lower == "content-length" || HOP_HEADERS.contains(&lower.as_str()) {
            continue;
        }
        if lower == "content-security-policy" {
            policed = true;
            reply.push_str(&format!("{name}: {}\r\n", tighten(value, port, host)));
        } else {
            reply.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    if !policed {
        reply.push_str(&format!(
            "Content-Security-Policy: {}\r\n",
            tighten("", port, host)
        ));
    }
    reply.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        page.len()
    ));
    let mut bytes = reply.into_bytes();
    bytes.extend_from_slice(page.as_bytes());
    bytes
}

/// The head a request is sent to the server with: its start line at `version`, its headers
/// without hop-by-hop ones and `dropped`, and `Connection: close`, so one connection carries
/// one checked request. Inputs: the request, the version and the extra headers to drop.
/// Output: the head bytes.
pub(crate) fn forwarded(request: &Request<'_>, version: &str, dropped: &[&str]) -> Vec<u8> {
    let mut head = format!("{} {} {version}\r\n", request.method, request.target);
    for (name, value) in &request.headers {
        let lower = name.to_ascii_lowercase();
        if HOP_HEADERS.contains(&lower.as_str()) || dropped.contains(&lower.as_str()) {
            continue;
        }
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("Connection: close\r\n\r\n");
    head.into_bytes()
}

async fn connection(mut client: TcpStream, site: Arc<Site>) {
    let Ok(Ok((head, rest))) = tokio::time::timeout(READ_TIME, read_head(&mut client)).await else {
        return;
    };
    let Some(request) = parse(&head) else {
        send(&mut client, &refusal("400 Bad Request")).await;
        return;
    };
    match decide(&request, site.port, &site.token, &site.workspace) {
        Action::Refuse(status) => send(&mut client, &refusal(status)).await,
        Action::Pre {
            name,
            kind,
            head_only,
        } => send(&mut client, &pre_file(&site.pre, name, kind, head_only)).await,
        Action::Page => page(client, &request, &site).await,
        Action::Pin => send(&mut client, &pin_reply(&request, &site.workspace)).await,
        Action::Upgrade => upgrade(client, &head, &rest, &site).await,
        Action::Forward { body } => forward(client, &request, &rest, body, &site).await,
    }
}

async fn upstream(port: u16) -> io::Result<TcpStream> {
    tokio::time::timeout(
        CONNECT_TIME,
        crate::spawn_gate::connect_tcp(("127.0.0.1", port)),
    )
    .await
    .map_err(|_| io::Error::other("ide_proxy_upstream_timeout"))?
}

async fn page(mut client: TcpStream, request: &Request<'_>, site: &Site) {
    let host = request.values("host").next().unwrap_or_default();
    assert!(workbench_host(host, site.port));
    let fetched = async {
        let mut server = upstream(site.upstream).await?;
        server
            .write_all(&forwarded(request, "HTTP/1.0", &PAGE_DROPPED))
            .await?;
        let mut raw = Vec::with_capacity(64 * 1024);
        (&mut server)
            .take(PAGE_BYTES_MAX as u64 + 1)
            .read_to_end(&mut raw)
            .await?;
        if raw.len() > PAGE_BYTES_MAX {
            return Err(io::Error::other("ide_proxy_page_too_long"));
        }
        Ok::<_, io::Error>(raw)
    };
    let reply = match tokio::time::timeout(PAGE_TIME, fetched).await {
        Ok(Ok(raw)) => page_reply(&raw, site.port, host),
        _ => refusal("502 Bad Gateway"),
    };
    send(&mut client, &reply).await;
}

async fn upgrade(mut client: TcpStream, head: &[u8], rest: &[u8], site: &Site) {
    let Ok(mut server) = upstream(site.upstream).await else {
        send(&mut client, &refusal("502 Bad Gateway")).await;
        return;
    };
    let joined = async {
        server.write_all(head).await?;
        server.write_all(rest).await?;
        tokio::io::copy_bidirectional(&mut client, &mut server).await
    };
    let _ = joined.await;
}

async fn forward(
    mut client: TcpStream,
    request: &Request<'_>,
    rest: &[u8],
    body: u64,
    site: &Site,
) {
    let Ok(mut server) = upstream(site.upstream).await else {
        send(&mut client, &refusal("502 Bad Gateway")).await;
        return;
    };
    let early = rest.len().min(usize::try_from(body).unwrap_or(usize::MAX));
    let carried = async {
        server
            .write_all(&forwarded(request, request.version, &[]))
            .await?;
        server.write_all(&rest[..early]).await?;
        let left = body - early as u64;
        let copied = tokio::io::copy(&mut (&mut client).take(left), &mut server).await?;
        if copied != left {
            return Err(io::Error::other("ide_proxy_body_cut"));
        }
        tokio::io::copy(&mut server, &mut client).await?;
        client.shutdown().await
    };
    let _ = carried.await;
}

async fn read_head(stream: &mut TcpStream) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut buffer = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    loop {
        if let Some(at) = find(&buffer, b"\r\n\r\n") {
            if at + 4 > HEAD_BYTES_MAX {
                return Err(io::Error::other("ide_proxy_head_too_long"));
            }
            let rest = buffer.split_off(at + 4);
            return Ok((buffer, rest));
        }
        if buffer.len() > HEAD_BYTES_MAX {
            return Err(io::Error::other("ide_proxy_head_too_long"));
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::Error::other("ide_proxy_head_cut"));
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

async fn send(client: &mut TcpStream, bytes: &[u8]) {
    let _ = tokio::time::timeout(SEND_TIME, async {
        client.write_all(bytes).await?;
        client.shutdown().await
    })
    .await;
}

/// The reply for one of the [`PRE_FILES`]: the plain file of that name in the folder, at
/// most 1 MiB and never through a link, else a 404. Inputs: the folder, the file's name and
/// type, and whether only the head is sent. Output: the reply bytes.
pub(crate) fn pre_file(pre: &Path, name: &str, kind: &str, head_only: bool) -> Vec<u8> {
    assert!(PRE_FILES.iter().any(|(file, _)| *file == name));
    let file = pre.join(name);
    let body = match fs::symlink_metadata(&file) {
        Ok(metadata) if metadata.file_type().is_file() && metadata.len() <= FILE_BYTES_MAX => {
            fs::read(&file).ok()
        }
        _ => None,
    };
    let Some(body) = body.filter(|body| body.len() as u64 <= FILE_BYTES_MAX) else {
        return refusal("404 Not Found");
    };
    let mut reply = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    if !head_only {
        reply.extend_from_slice(&body);
    }
    reply
}

fn refusal(status: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
    )
    .into_bytes()
}

fn directive_name(directive: &str) -> String {
    directive
        .split_ascii_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len() * 2);
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '"' => escaped.push_str("&quot;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn decoded(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'%' => {
                let hex = text.get(at + 1..at + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                at += 3;
            }
            b'+' => {
                out.push(b' ');
                at += 1;
            }
            byte => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Percent-encodes a path for a query value, keeping `/` and the unreserved characters.
/// Input: the path. Output: the encoded text.
pub(crate) fn encoded(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests;
