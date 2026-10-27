use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The folder below a VS Code build's root whose files every webview loads first: its host
/// page and its service worker.
pub const PRE: &str = "out/vs/workbench/contrib/webview/browser/pre";

const HEAD_BYTES_MAX: usize = 8 * 1024;
const FILE_BYTES_MAX: u64 = 1024 * 1024;
const NAME_BYTES_MAX: usize = 64;
const LABEL_BYTES_MAX: usize = 63;
const CONNECTIONS_MAX: usize = 32;
const READ_TIME: Duration = Duration::from_secs(5);
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);

/// The template the workbench builds each webview's origin from: every webview gets its own
/// `<uuid>.localhost` host on the webview port, so webviews stay apart from the workbench
/// and from each other and nothing loads from the internet. Input: the webview port.
/// Output: the template, with `{{uuid}}` left for the workbench to fill.
pub fn template(port: u16) -> String {
    format!("http://{{{{uuid}}}}.localhost:{port}/")
}

/// Binds the webview host's loopback listener: on the recorded port while it is free, else
/// on one the system picks. Input: the recorded port, if any. Output: the listener and its
/// port.
pub async fn bind(recorded: Option<u16>) -> std::io::Result<(TcpListener, u16)> {
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

/// Serves the files of a build's [`PRE`] folder to the IDE's webviews until the task is
/// aborted: GET and HEAD of one plain file by name, only for a loopback host on this port,
/// so a page whose name was rebound to 127.0.0.1 gets nothing. At most 32 connections are
/// answered at once; more are closed. Inputs: the bound listener, the folder and the port.
pub async fn serve(listener: TcpListener, pre: PathBuf, port: u16) {
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
        connections.spawn(answer(stream, pre.clone(), port));
        assert!(connections.len() <= CONNECTIONS_MAX);
    }
}

struct Reply {
    status: &'static str,
    kind: &'static str,
    body: Vec<u8>,
    head_only: bool,
}

impl Reply {
    fn refused(status: &'static str) -> Self {
        Self {
            status,
            kind: "text/plain; charset=utf-8",
            body: Vec::new(),
            head_only: false,
        }
    }
}

async fn answer(mut stream: TcpStream, pre: PathBuf, port: u16) {
    let Ok(Ok(head)) = tokio::time::timeout(READ_TIME, read_head(&mut stream)).await else {
        return;
    };
    let reply = respond(&head, &pre, port);
    let start = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        reply.status,
        reply.kind,
        reply.body.len()
    );
    let _ = tokio::time::timeout(READ_TIME, async {
        stream.write_all(start.as_bytes()).await?;
        if !reply.head_only {
            stream.write_all(&reply.body).await?;
        }
        stream.shutdown().await
    })
    .await;
}

async fn read_head(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut head = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        if head.len() > HEAD_BYTES_MAX {
            return Err(std::io::Error::other("ide_webview_head_too_long"));
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(std::io::Error::other("ide_webview_head_cut"));
        }
        head.extend_from_slice(&chunk[..read]);
    }
    Ok(head)
}

/// Answers one request head for a file of the [`PRE`] folder. Inputs: the head bytes, the
/// folder and the webview port. Output: the reply; a refusal for anything else.
fn respond(head: &[u8], pre: &Path, port: u16) -> Reply {
    let Ok(text) = std::str::from_utf8(head) else {
        return Reply::refused("400 Bad Request");
    };
    let mut lines = text.split("\r\n");
    let mut request = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target), Some(version), None) = (
        request.next(),
        request.next(),
        request.next(),
        request.next(),
    ) else {
        return Reply::refused("400 Bad Request");
    };
    if !version.starts_with("HTTP/1.") {
        return Reply::refused("400 Bad Request");
    }
    let head_only = method == "HEAD";
    if method != "GET" && !head_only {
        return Reply::refused("405 Method Not Allowed");
    }
    let hosts: Vec<&str> = lines
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.eq_ignore_ascii_case("host"))
        .map(|(_, value)| value.trim())
        .collect();
    if hosts.len() != 1 || !loopback_host(hosts[0], port) {
        return Reply::refused("403 Forbidden");
    }
    let name = target
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .trim_start_matches('/');
    if !plain_name(name) {
        return Reply::refused("404 Not Found");
    }
    let file = pre.join(name);
    let body = match fs::symlink_metadata(&file) {
        Ok(metadata) if metadata.file_type().is_file() && metadata.len() <= FILE_BYTES_MAX => {
            fs::read(&file).ok()
        }
        _ => None,
    };
    let Some(body) = body.filter(|body| body.len() as u64 <= FILE_BYTES_MAX) else {
        return Reply::refused("404 Not Found");
    };
    Reply {
        status: "200 OK",
        kind: kind(name),
        body,
        head_only,
    }
}

/// Tells whether a Host header names this webview host: `127.0.0.1`, `localhost` or one
/// lowercase DNS label under `localhost`, on this port. Inputs: the header value and the
/// port. Output: the answer.
fn loopback_host(host: &str, port: u16) -> bool {
    let host = host.to_ascii_lowercase();
    let Some(name) = host.strip_suffix(&format!(":{port}")) else {
        return false;
    };
    if name == "127.0.0.1" || name == "localhost" {
        return true;
    }
    name.strip_suffix(".localhost").is_some_and(|label| {
        !label.is_empty()
            && label.len() <= LABEL_BYTES_MAX
            && !label.starts_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    })
}

fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_BYTES_MAX
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn kind(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, extension)| extension) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}
