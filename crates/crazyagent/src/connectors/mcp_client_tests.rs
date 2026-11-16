use super::*;
use crate::app_plugins::connector::ReadOnly;
use crate::app_plugins::{manifest, store};
use crate::connectors::tests::{Case, add, case};
use base64::Engine;
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(crate) const KEY: &str = "sk-test-key-0001";
pub(in crate::connectors) const USER: &str = "ada@example.com";

pub(crate) const BOARD: &str = "schema: 1
kind: connector
name: board
title: Board
version: 1
mcp:
  url: https://mcp.board.example/mcp
  hosts: [mcp.board.example]
key:
  secret: api_key
  header: Authorization
  scheme: Bearer
";

pub(in crate::connectors) const DESK: &str = "schema: 1
kind: connector
name: desk
title: Desk
version: 1
mcp:
  url: https://mcp.desk.example/mcp
  hosts: [mcp.desk.example]
key:
  secret: api_key
  user_secret: user
  header: Authorization
  scheme: Basic
";

/// One request the stand-in vendor got: its verb, path, headers and JSON body.
#[derive(Debug, Clone)]
pub(in crate::connectors) struct Seen {
    pub(in crate::connectors) verb: String,
    pub(in crate::connectors) path: String,
    pub(in crate::connectors) headers: Vec<(String, String)>,
    pub(in crate::connectors) body: Value,
}

impl Seen {
    pub(in crate::connectors) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(known, _)| known.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub(in crate::connectors) fn rpc(&self) -> &str {
        self.body["method"].as_str().unwrap_or("")
    }

    pub(in crate::connectors) fn id(&self) -> Value {
        self.body["id"].clone()
    }

    pub(in crate::connectors) fn tool(&self) -> &str {
        self.body["params"]["name"].as_str().unwrap_or("")
    }
}

/// A real MCP server on loopback that stands in for a vendor: it reads each request,
/// keeps it, and lets the row write the reply. It stops when dropped.
pub(in crate::connectors) struct Vendor {
    pub(in crate::connectors) port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
}

impl Vendor {
    pub(in crate::connectors) fn start(
        answer: impl Fn(&Seen, usize, &mut TcpStream) + Send + 'static,
    ) -> Self {
        let listener = crate::spawn_gate::bind_tcp_std(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (kept, stopped) = (seen.clone(), stop.clone());
        std::thread::spawn(move || {
            loop {
                let Ok((mut stream, _)) = crate::spawn_gate::accept_tcp_std(&listener) else {
                    break;
                };
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                let request = read_request(&stream);
                let at = {
                    let mut all = kept.lock().unwrap();
                    all.push(request.clone());
                    all.len() - 1
                };
                answer(&request, at, &mut stream);
            }
        });
        Self { port, seen, stop }
    }

    pub(in crate::connectors) fn endpoint(&self) -> McpEndpoint {
        McpEndpoint::loopback_for_rows(self.port)
    }

    pub(in crate::connectors) fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    pub(in crate::connectors) fn calls(&self) -> Vec<String> {
        self.seen()
            .iter()
            .map(|seen| seen.rpc().to_owned())
            .collect()
    }
}

impl Drop for Vendor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = crate::spawn_gate::connect_tcp_std(("127.0.0.1", self.port));
    }
}

fn read_request(stream: &TcpStream) -> Seen {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    let mut parts = first.split_whitespace();
    let verb = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("").to_owned();
    let mut headers = Vec::new();
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').unwrap();
        if name.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().unwrap();
        }
        headers.push((name.to_owned(), value.trim().to_owned()));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    Seen {
        verb,
        path,
        headers,
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    }
}

pub(in crate::connectors) fn send(stream: &mut TcpStream, bytes: &[u8]) {
    let _ = stream.write_all(bytes);
    let _ = stream.flush();
}

pub(in crate::connectors) fn http(
    status: &str,
    kind: &str,
    extra: &[(&str, &str)],
    body: &[u8],
) -> Vec<u8> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

pub(in crate::connectors) fn json_reply(
    status: &str,
    extra: &[(&str, &str)],
    body: &Value,
) -> Vec<u8> {
    http(
        status,
        "application/json",
        extra,
        body.to_string().as_bytes(),
    )
}

pub(in crate::connectors) fn result(seen: &Seen, result: Value) -> Vec<u8> {
    json_reply(
        "200 OK",
        &[],
        &json!({"jsonrpc": "2.0", "id": seen.id(), "result": result}),
    )
}

pub(in crate::connectors) fn rpc_error(
    status: &str,
    seen: &Seen,
    code: i64,
    message: &str,
) -> Vec<u8> {
    json_reply(
        status,
        &[],
        &json!({"jsonrpc": "2.0", "id": seen.id(), "error": {"code": code, "message": message}}),
    )
}

pub(in crate::connectors) fn empty(status: &str, extra: &[(&str, &str)]) -> Vec<u8> {
    let mut head = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n");
    for (name, value) in extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    head.into_bytes()
}

const SSE_HEAD: &[u8] =
    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";

fn event(message: &Value) -> Vec<u8> {
    format!("event: message\r\ndata: {message}\r\n\r\n").into_bytes()
}

fn sse(messages: &[Value]) -> Vec<u8> {
    let mut bytes = SSE_HEAD.to_vec();
    for message in messages {
        bytes.extend_from_slice(&event(message));
    }
    bytes
}

pub(in crate::connectors) fn tool(name: &str) -> Value {
    json!({
        "name": name,
        "description": format!("Does {name}."),
        "inputSchema": {"type": "object", "properties": {}},
    })
}

pub(in crate::connectors) fn text_result(text: &str) -> Value {
    json!({"resultType": "complete", "content": [{"type": "text", "text": text}]})
}

/// The replies of a server of the per-request era to the opening probe. Output: the reply,
/// none for any other request.
pub(in crate::connectors) fn modern(seen: &Seen) -> Option<Vec<u8>> {
    (seen.rpc() == "server/discover").then(|| {
        result(
            seen,
            json!({
                "resultType": "complete",
                "supportedVersions": [MODERN_VERSION],
                "capabilities": {"tools": {}},
                "ttlMs": 1000,
                "cacheScope": "private",
            }),
        )
    })
}

/// The replies of a server of the handshake era to the opening requests: it refuses the
/// probe, answers `initialize` in `version` with the session id given, and takes the
/// `initialized` notice. Output: the reply, none for any other request.
pub(in crate::connectors) fn legacy(
    seen: &Seen,
    session: Option<&str>,
    version: &str,
) -> Option<Vec<u8>> {
    match seen.rpc() {
        "server/discover" => Some(json_reply(
            "400 Bad Request",
            &[],
            &json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": {"code": -32000, "message": "Bad Request: No valid session ID provided"},
            }),
        )),
        "initialize" => {
            let body = json!({
                "jsonrpc": "2.0",
                "id": seen.id(),
                "result": {
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "stand-in", "version": "1"},
                },
            });
            let headers: Vec<(&str, &str)> =
                session.iter().map(|id| ("Mcp-Session-Id", *id)).collect();
            Some(json_reply("200 OK", &headers, &body))
        }
        "notifications/initialized" => Some(empty("202 Accepted", &[])),
        _ => None,
    }
}

struct Desk {
    case: Case,
    bearer: Credential,
    basic: Credential,
}

fn credential(case: &Case, name: &str) -> Credential {
    let loaded = store::connector_ready(&case.db, &case.root, name).unwrap();
    let spec = loaded.manifest.connector().unwrap();
    Credential::key(spec.key.as_ref().unwrap(), &loaded.bindings)
}

/// A real engine root with two trusted connectors whose keys are saved: `board` with a
/// Bearer key and `desk` with a Basic one. Output: the root and both credentials.
fn desk() -> Desk {
    let case = case();
    for (text, name, secrets) in [
        (BOARD, "board", &[("api_key", KEY)][..]),
        (DESK, "desk", &[("api_key", KEY), ("user", USER)][..]),
    ] {
        add(&case, text, false);
        for (secret, value) in secrets {
            store::set_key(
                &case.db,
                &case.root,
                name,
                secret,
                value.as_bytes().to_vec(),
            )
            .unwrap();
        }
    }
    let bearer = credential(&case, "board");
    let basic = credential(&case, "desk");
    Desk {
        case,
        bearer,
        basic,
    }
}

pub(in crate::connectors) fn no_lookup() -> ProviderReach {
    ProviderReach::new(
        |name, _| panic!("a loopback row looks no name up: {name}"),
        |_| Ok(()),
    )
}

fn open(desk: &Desk, endpoint: &McpEndpoint, access: Access) -> Result<McpSession, McpError> {
    McpSession::open_with(
        &desk.case.root,
        endpoint,
        &desk.bearer,
        access,
        SizeCap::Kib256,
        no_lookup(),
    )
}

fn names(tools: &[VendorTool]) -> Vec<&str> {
    tools.iter().map(|tool| tool.name.as_str()).collect()
}

fn encoded(text: &str) -> String {
    format!(
        "=?base64?{}?=",
        base64::engine::general_purpose::STANDARD.encode(text)
    )
}

#[test]
fn a_tools_list_over_sse_from_a_loopback_mcp_server_reads_every_page() {
    let desk = desk();
    let vendor = Vendor::start(|seen, _, stream| {
        let reply = legacy(seen, Some("sess-1"), "2025-06-18").unwrap_or_else(|| {
            assert_eq!(seen.rpc(), "tools/list");
            match seen.body["params"].get("cursor").and_then(Value::as_str) {
                None => sse(&[
                    json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {"progress": 1}}),
                    json!({"jsonrpc": "2.0", "id": seen.id(), "result": {"tools": [tool("list_issues")], "nextCursor": ""}}),
                ]),
                Some("") => result(
                    seen,
                    json!({"tools": [tool("get_issue")], "nextCursor": "p3"}),
                ),
                Some("p3") => {
                    let mut bytes = SSE_HEAD.to_vec();
                    let last = json!({"tools": [tool("search")]}).to_string();
                    bytes.extend_from_slice(
                        format!(
                            ": ping\r\n\r\nevent: message\r\nid: 7\r\nretry: 1000\r\ndata: {{\"jsonrpc\":\"2.0\",\r\ndata: \"id\":{},\"result\":{last}}}\r\n\r\n",
                            seen.id()
                        )
                        .as_bytes(),
                    );
                    bytes
                }
                Some(other) => panic!("{other}"),
            }
        });
        send(stream, &reply);
    });
    let mut session = open(&desk, &vendor.endpoint(), Access::Full).unwrap();
    let tools = session.list_tools().unwrap();
    assert_eq!(names(&tools), ["list_issues", "get_issue", "search"]);
    assert_eq!(tools[0].description, "Does list_issues.");
    assert_eq!(session.left_out(), 0);
    assert_eq!(
        vendor.calls(),
        [
            "server/discover",
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/list",
            "tools/list",
        ]
    );
    let seen = vendor.seen();
    for request in &seen {
        assert_eq!(
            (request.verb.as_str(), request.path.as_str()),
            ("POST", "/mcp")
        );
        assert_eq!(
            request.header("authorization"),
            Some(format!("Bearer {KEY}").as_str())
        );
        let accept = request.header("accept").unwrap();
        assert!(accept.contains("application/json") && accept.contains("text/event-stream"));
        assert_eq!(request.header("content-type"), Some("application/json"));
    }
    let hello = &seen[1];
    assert_eq!(hello.header("mcp-session-id"), None);
    assert_eq!(hello.body["params"]["protocolVersion"], LEGACY_VERSIONS[0]);
    assert_eq!(hello.body["params"]["capabilities"], json!({}));
    assert_eq!(hello.body["params"]["clientInfo"]["name"], "CrazyAgent");
    for request in &seen[2..] {
        assert_eq!(request.header("mcp-session-id"), Some("sess-1"));
        assert_eq!(request.header("mcp-protocol-version"), Some("2025-06-18"));
        assert_eq!(request.header("mcp-method"), None);
    }
}

#[test]
fn an_older_server_gets_the_handshake_and_a_session_it_ended_is_opened_again_once() {
    let desk = desk();
    let opened = Arc::new(AtomicUsize::new(0));
    let count = opened.clone();
    let vendor = Vendor::start(move |seen, _, stream| {
        let reply = match seen.rpc() {
            "server/discover" => rpc_error("200 OK", seen, -32601, "Method not found"),
            "initialize" => {
                let id = format!("sess-{}", count.fetch_add(1, Ordering::SeqCst) + 1);
                legacy(seen, Some(id.as_str()), LEGACY_VERSIONS[0]).unwrap()
            }
            "notifications/initialized" => empty("202 Accepted", &[]),
            "tools/call" => match (seen.tool(), seen.header("mcp-session-id")) {
                ("always_gone", _) | (_, Some("sess-1")) => empty("404 Not Found", &[]),
                _ => result(seen, json!({"content": [{"type": "text", "text": "done"}]})),
            },
            _ => empty("200 OK", &[]),
        };
        send(stream, &reply);
    });
    let mut session = open(&desk, &vendor.endpoint(), Access::Full).unwrap();
    let done = session.call("get_issue", &json!({"id": "A-1"})).unwrap();
    assert_eq!(
        done,
        ToolResult {
            text: "done".into(),
            is_error: false,
            cut: false,
        }
    );
    assert_eq!(
        vendor.calls(),
        [
            "server/discover",
            "initialize",
            "notifications/initialized",
            "tools/call",
            "initialize",
            "notifications/initialized",
            "tools/call",
        ]
    );
    let seen = vendor.seen();
    assert_eq!(seen[3].header("mcp-session-id"), Some("sess-1"));
    assert_eq!(seen[4].header("mcp-session-id"), None);
    assert_eq!(seen[6].header("mcp-session-id"), Some("sess-2"));
    assert_eq!(
        seen[6].body["params"],
        json!({"name": "get_issue", "arguments": {"id": "A-1"}})
    );
    assert_eq!(
        session.call("always_gone", &json!({})),
        Err(McpError::Vendor("HTTP 404".into()))
    );
    assert_eq!(
        opened.load(Ordering::SeqCst),
        3,
        "one new session per call, not a loop"
    );
    let before = vendor.seen().len();
    session.close();
    let seen = vendor.seen();
    assert_eq!(seen.len(), before + 1);
    assert_eq!(seen[before].verb, "DELETE");
    assert_eq!(seen[before].header("mcp-session-id"), Some("sess-3"));
    assert_eq!(
        seen[before].header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
}

#[test]
fn a_server_of_the_per_request_era_gets_no_handshake_and_every_request_names_itself() {
    let desk = desk();
    let vendor = Vendor::start(|seen, _, stream| {
        let reply = modern(seen).unwrap_or_else(|| match seen.rpc() {
            "tools/list" => result(
                seen,
                json!({
                    "resultType": "complete",
                    "tools": [{
                        "name": "list_issues",
                        "description": "Lists issues.",
                        "inputSchema": {"type": "object", "properties": {
                            "team": {"type": "string", "x-mcp-header": "Team"},
                            "limit": {"type": "integer", "x-mcp-header": "Limit"},
                            "open": {"type": "boolean", "x-mcp-header": "Open"},
                            "filter": {"type": "object", "properties": {
                                "state": {"type": "string", "x-mcp-header": "State"},
                            }},
                            "note": {"type": "string", "x-mcp-header": "Note"},
                            "blank": {"type": "string", "x-mcp-header": "Blank"},
                            "skip": {"type": "string", "x-mcp-header": "Skip"},
                            "absent": {"type": "string", "x-mcp-header": "Absent"},
                        }},
                        "annotations": {"readOnlyHint": true, "destructiveHint": false},
                    }],
                    "ttlMs": 1000,
                    "cacheScope": "private",
                }),
            ),
            "tools/call" => result(seen, text_result("three issues")),
            other => panic!("{other}"),
        });
        send(stream, &reply);
    });
    let mut session = open(&desk, &vendor.endpoint(), Access::Full).unwrap();
    let tools = session.list_tools().unwrap();
    assert_eq!(names(&tools), ["list_issues"]);
    assert_eq!(tools[0].read_only_hint, Some(true));
    assert_eq!(tools[0].destructive_hint, Some(false));
    let args = json!({
        "team": "core",
        "limit": 25,
        "open": true,
        "filter": {"state": "caf\u{e9}"},
        "note": " padded ",
        "blank": "",
        "skip": null,
    });
    let reply = session.call("list_issues", &args).unwrap();
    assert_eq!(reply.text, "three issues");
    assert_eq!(
        vendor.calls(),
        ["server/discover", "tools/list", "tools/call"]
    );
    let seen = vendor.seen();
    for request in &seen {
        assert_eq!(request.header("mcp-protocol-version"), Some(MODERN_VERSION));
        assert_eq!(request.header("mcp-method"), Some(request.rpc()));
        assert_eq!(request.header("mcp-session-id"), None);
        assert_eq!(
            request.header("authorization"),
            Some(format!("Bearer {KEY}").as_str())
        );
        let meta = &request.body["params"]["_meta"];
        assert_eq!(
            meta["io.modelcontextprotocol/protocolVersion"],
            MODERN_VERSION
        );
        assert_eq!(
            meta["io.modelcontextprotocol/clientCapabilities"],
            json!({})
        );
        assert_eq!(
            meta["io.modelcontextprotocol/clientInfo"]["name"],
            "CrazyAgent"
        );
    }
    assert_eq!(seen[0].header("mcp-name"), None);
    assert_eq!(seen[1].header("mcp-name"), None);
    let call = &seen[2];
    assert_eq!(call.header("mcp-name"), Some("list_issues"));
    assert_eq!(call.body["params"]["name"], "list_issues");
    assert_eq!(call.body["params"]["arguments"], args);
    assert_eq!(call.header("mcp-param-team"), Some("core"));
    assert_eq!(call.header("mcp-param-limit"), Some("25"));
    assert_eq!(call.header("mcp-param-open"), Some("true"));
    assert_eq!(
        call.header("mcp-param-state"),
        Some(encoded("caf\u{e9}").as_str())
    );
    assert_eq!(
        call.header("mcp-param-note"),
        Some(encoded(" padded ").as_str())
    );
    assert_eq!(call.header("mcp-param-blank"), Some(encoded("").as_str()));
    assert_eq!(call.header("mcp-param-skip"), None);
    assert_eq!(call.header("mcp-param-absent"), None);
    let before = seen.len();
    session.close();
    assert_eq!(vendor.seen().len(), before, "no session to end in this era");
}

#[test]
fn a_tool_whose_header_marks_break_the_rules_is_left_out() {
    let desk = desk();
    let marked =
        |name: &str, schema: Value| json!({"name": name, "description": "", "inputSchema": schema});
    let vendor = Vendor::start(move |seen, _, stream| {
        let reply = modern(seen).unwrap_or_else(|| {
            result(
                seen,
                json!({"resultType": "complete", "tools": [
                    marked("good", json!({"type": "object", "properties": {
                        "team": {"type": "string", "x-mcp-header": "Team"},
                        "x-mcp-header": {"type": "string"},
                    }})),
                    marked("in_items", json!({"type": "object", "properties": {
                        "teams": {"type": "array", "items": {"type": "string", "x-mcp-header": "Team"}},
                    }})),
                    marked("in_any_of", json!({"type": "object", "anyOf": [
                        {"properties": {"team": {"type": "string", "x-mcp-header": "Team"}}},
                    ]})),
                    marked("twice", json!({"type": "object", "properties": {
                        "a": {"type": "string", "x-mcp-header": "Team"},
                        "b": {"type": "string", "x-mcp-header": "team"},
                    }})),
                    marked("a_number", json!({"type": "object", "properties": {
                        "a": {"type": "number", "x-mcp-header": "Ratio"},
                    }})),
                    marked("not_a_token", json!({"type": "object", "properties": {
                        "a": {"type": "string", "x-mcp-header": "Bad Name"},
                    }})),
                    marked("a_line_break", json!({"type": "object", "properties": {
                        "a": {"type": "string", "x-mcp-header": "Team\r\nAuthorization: x"},
                    }})),
                    marked("empty_mark", json!({"type": "object", "properties": {
                        "a": {"type": "string", "x-mcp-header": ""},
                    }})),
                ]}),
            )
        });
        send(stream, &reply);
    });
    let mut session = open(&desk, &vendor.endpoint(), Access::Full).unwrap();
    assert_eq!(names(&session.list_tools().unwrap()), ["good"]);
    assert_eq!(session.left_out(), 7);
    let before = vendor.seen().len();
    assert_eq!(
        session.call("in_items", &json!({"teams": ["core"]})),
        Err(McpError::Refused("connector_tool_unknown"))
    );
    assert_eq!(vendor.seen().len(), before);
}

#[test]
fn a_redirect_is_refused() {
    let desk = desk();
    for status in [
        "301 Moved Permanently",
        "302 Found",
        "307 Temporary Redirect",
        "308 Permanent Redirect",
    ] {
        let vendor = Vendor::start(move |_, _, stream| {
            send(stream, &empty(status, &[("Location", "/elsewhere")]));
        });
        assert_eq!(
            open(&desk, &vendor.endpoint(), Access::Full).unwrap_err(),
            McpError::Refused("connector_redirect_refused"),
            "{status}"
        );
        let seen = vendor.seen();
        assert_eq!(seen.len(), 1, "{status}");
        assert_eq!(seen[0].path, "/mcp");
    }
    let vendor = Vendor::start(|seen, _, stream| {
        let reply = modern(seen)
            .unwrap_or_else(|| empty("307 Temporary Redirect", &[("Location", "/elsewhere")]));
        send(stream, &reply);
    });
    let mut session = open(&desk, &vendor.endpoint(), Access::Full).unwrap();
    assert_eq!(
        session.list_tools(),
        Err(McpError::Refused("connector_redirect_refused"))
    );
    assert!(vendor.seen().iter().all(|seen| seen.path == "/mcp"));
}

#[test]
fn a_response_over_the_cap_is_cut_and_marked() {
    let desk = desk();
    let vendor = Vendor::start(|seen, _, stream| {
        let reply = modern(seen).unwrap_or_else(|| match seen.tool() {
            "small" => result(seen, text_result("ok")),
            "big" => result(seen, text_result(&"a".repeat(300_000))),
            "accents" => result(seen, text_result(&"\u{e9}".repeat(100_000))),
            "huge" => result(seen, text_result(&"a".repeat(REPLY_BYTES_MAX + 1))),
            _ => result(
                seen,
                json!({"resultType": "complete", "tools": [
                    tool("small"), tool("big"), tool("accents"), tool("huge"),
                ]}),
            ),
        });
        send(stream, &reply);
    });
    let cap = SizeCap::Kib64;
    let mut session = McpSession::open_with(
        &desk.case.root,
        &vendor.endpoint(),
        &desk.bearer,
        Access::Full,
        cap,
        no_lookup(),
    )
    .unwrap();
    session.list_tools().unwrap();
    assert_eq!(
        session.call("small", &json!({})),
        Ok(ToolResult {
            text: "ok".into(),
            is_error: false,
            cut: false,
        })
    );
    for name in ["big", "accents"] {
        let reply = session.call(name, &json!({})).unwrap();
        assert!(reply.cut, "{name}");
        assert!(reply.text.len() <= cap.bytes(), "{name}");
        assert!(
            reply.text.len() > cap.bytes() - 8 - CUT_MARK.len(),
            "{name}"
        );
        assert!(reply.text.ends_with(CUT_MARK), "{name}");
        assert_eq!(reply.text.matches(CUT_MARK).count(), 1, "{name}");
    }
    assert_eq!(session.call("huge", &json!({})), Err(McpError::TooLarge));
    assert_eq!(
        session.call("small", &json!({})).map(|reply| reply.text),
        Ok("ok".into()),
        "the session still works after a reply it gave up on"
    );
}

#[test]
fn a_result_is_handed_on_as_text_only_with_hidden_characters_taken_out() {
    let desk = desk();
    let vendor = Vendor::start(|seen, _, stream| {
        let reply = modern(seen).unwrap_or_else(|| match seen.tool() {
            "mixed" => result(
                seen,
                json!({"resultType": "complete", "content": [
                    {"type": "text", "text": "first\u{200b} line\u{202e}"},
                    {"type": "image", "data": "aGVsbG8=", "mimeType": "image/png"},
                    {"type": "resource_link", "uri": "file:///etc/passwd", "name": "x"},
                    {"type": "made\u{202e}up", "text": "ignored"},
                    {"type": "text", "text": "second\tline\nthird"},
                ], "structuredContent": {"ignored": true}}),
            ),
            "structured" => result(
                seen,
                json!({"resultType": "complete", "content": [], "structuredContent": {"count": 2}}),
            ),
            "failed" => result(
                seen,
                json!({"resultType": "complete", "isError": true, "content": [
                    {"type": "text", "text": "rate limit"},
                ]}),
            ),
            "nothing" => result(seen, json!({"resultType": "complete"})),
            _ => result(
                seen,
                json!({"resultType": "complete", "tools": [
                    tool("mixed"), tool("structured"), tool("failed"), tool("nothing"),
                ]}),
            ),
        });
        send(stream, &reply);
    });
    let mut session = open(&desk, &vendor.endpoint(), Access::Full).unwrap();
    session.list_tools().unwrap();
    assert_eq!(
        session.call("mixed", &json!({})).unwrap().text,
        "first line\n[image left out]\n[resource_link left out]\n[other content left out]\nsecond\tline\nthird"
    );
    assert_eq!(
        session.call("structured", &json!({})).unwrap().text,
        "{\"count\":2}"
    );
    assert_eq!(
        session.call("failed", &json!({})),
        Ok(ToolResult {
            text: "rate limit".into(),
            is_error: true,
            cut: false,
        })
    );
    assert_eq!(session.call("nothing", &json!({})).unwrap().text, "");
}

#[test]
fn a_401_with_a_scope_is_reported_as_unauthorized_with_that_scope() {
    let desk = desk();
    let cases: [(&str, &str, Option<&str>); 6] = [
        (
            "401 Unauthorized",
            "Bearer resource_metadata=\"https://mcp.board.example/.well-known/oauth-protected-resource\", scope=\"read:issues write:issues\"",
            Some("read:issues write:issues"),
        ),
        ("401 Unauthorized", "Bearer realm=\"board\"", None),
        (
            "401 Unauthorized",
            "Bearer scope=\"read\u{202e}issues\"",
            None,
        ),
        (
            "403 Forbidden",
            "Bearer error=\"insufficient_scope\", scope=\"issues:write\", error_description=\"More access needed\"",
            Some("issues:write"),
        ),
        (
            "401 Unauthorized",
            "Bearer error=\"invalid_token\", SCOPE=read",
            Some("read"),
        ),
        ("401 Unauthorized", "Bearer scope=\"a\\\"b\"", None),
    ];
    for (status, challenge, scope) in cases {
        let vendor = Vendor::start(move |_, _, stream| {
            send(stream, &empty(status, &[("WWW-Authenticate", challenge)]));
        });
        assert_eq!(
            open(&desk, &vendor.endpoint(), Access::Full).unwrap_err(),
            McpError::Unauthorized {
                scope: scope.map(str::to_owned)
            },
            "{challenge}"
        );
        assert_eq!(
            vendor.calls(),
            ["server/discover"],
            "no fall back on a refused key"
        );
    }
    let vendor = Vendor::start(|_, _, stream| send(stream, &empty("401 Unauthorized", &[])));
    assert_eq!(
        open(&desk, &vendor.endpoint(), Access::Full).unwrap_err(),
        McpError::Unauthorized { scope: None }
    );
    let vendor = Vendor::start(|seen, _, stream| {
        let reply = modern(seen).unwrap_or_else(|| match seen.rpc() {
            "tools/list" => result(
                seen,
                json!({"resultType": "complete", "tools": [tool("create_issue"), tool("blocked")]}),
            ),
            _ if seen.tool() == "blocked" => empty("403 Forbidden", &[]),
            _ => empty(
                "403 Forbidden",
                &[(
                    "WWW-Authenticate",
                    "Bearer error=\"insufficient_scope\", scope=\"issues:write\"",
                )],
            ),
        });
        send(stream, &reply);
    });
    let mut session = open(&desk, &vendor.endpoint(), Access::Full).unwrap();
    session.list_tools().unwrap();
    assert_eq!(
        session.call("create_issue", &json!({"title": "x"})),
        Err(McpError::Unauthorized {
            scope: Some("issues:write".into())
        })
    );
    assert_eq!(
        session.call("blocked", &json!({})),
        Err(McpError::Vendor("HTTP 403".into()))
    );
}

#[test]
fn hidden_characters_in_tool_descriptions_are_removed() {
    let desk = desk();
    let vendor = Vendor::start(|seen, _, stream| {
        let reply = legacy(seen, None, LEGACY_VERSIONS[0]).unwrap_or_else(|| {
            result(
                seen,
                json!({"tools": [{
                    "name": "list_issues",
                    "description": "Lists\u{200b} issues.\u{202e}\u{7} Second line\nstays.",
                    "inputSchema": {"type": "object", "properties": {
                        "te\u{200b}am": {
                            "type": "string",
                            "description": "The\u{200f} team.",
                            "enum": ["co\u{200d}re", "ops"],
                        },
                    }},
                    "annotations": {"readOnlyHint": "yes", "title": "x"},
                }, {
                    "name": "long_text",
                    "description": "d".repeat(20_000),
                    "inputSchema": {"type": "object"},
                }]}),
            )
        });
        send(stream, &reply);
    });
    let mut session = open(&desk, &vendor.endpoint(), Access::Full).unwrap();
    let tools = session.list_tools().unwrap();
    assert_eq!(names(&tools), ["list_issues", "long_text"]);
    assert_eq!(tools[0].description, "Lists issues. Second line\nstays.");
    assert_eq!(
        tools[0].input_schema,
        json!({"type": "object", "properties": {
            "team": {"type": "string", "description": "The team.", "enum": ["core", "ops"]},
        }})
    );
    assert_eq!(tools[0].read_only_hint, None);
    assert_eq!(tools[0].destructive_hint, None);
    assert_eq!(tools[1].description.len(), 4096);
    assert!(
        vendor
            .seen()
            .iter()
            .all(|seen| seen.header("mcp-session-id").is_none()),
        "a server that gave no session id is sent none"
    );
}

#[test]
fn a_tool_with_a_hidden_character_in_its_name_is_left_out_and_never_renamed() {
    let desk = desk();
    let vendor = Vendor::start(|seen, _, stream| {
        let reply = legacy(seen, None, LEGACY_VERSIONS[0]).unwrap_or_else(|| {
            let tools = vec![
                tool("list_issues"),
                tool("list\u{200b}_issues"),
                tool("has space"),
                tool(""),
                tool(&"n".repeat(129)),
                tool("line\nbreak"),
                tool("caf\u{e9}"),
                json!({"name": 7, "inputSchema": {"type": "object"}}),
                json!({"name": "no_schema", "description": "x"}),
                json!({"name": "schema_not_an_object", "inputSchema": []}),
                json!("not an object"),
                tool(&"n".repeat(128)),
                tool("Dots.and-dashes_1"),
            ];
            result(seen, json!({"tools": tools}))
        });
        send(stream, &reply);
    });
    let mut session = open(&desk, &vendor.endpoint(), Access::Full).unwrap();
    let tools = session.list_tools().unwrap();
    let long = "n".repeat(128);
    assert_eq!(
        names(&tools),
        ["list_issues", long.as_str(), "Dots.and-dashes_1"]
    );
    assert_eq!(session.left_out(), 10);
    for name in [
        "list\u{200b}_issues",
        "has space",
        "",
        "line\nbreak",
        "a/b",
        "caf\u{e9}",
    ] {
        assert_eq!(
            session.call(name, &json!({})),
            Err(McpError::Refused("connector_tool_name_invalid")),
            "{name:?}"
        );
    }
}

#[test]
fn a_list_that_names_one_tool_twice_or_never_ends_is_refused() {
    let desk = desk();
    let vendor = Vendor::start(|seen, _, stream| {
        let reply = legacy(seen, None, LEGACY_VERSIONS[0])
            .unwrap_or_else(|| result(seen, json!({"tools": [tool("search"), tool("search")]})));
        send(stream, &reply);
    });
    let mut session = open(&desk, &vendor.endpoint(), Access::Full).unwrap();
    assert_eq!(
        session.list_tools(),
        Err(McpError::Refused("connector_reply_invalid"))
    );
    let vendor = Vendor::start(|seen, at, stream| {
        let reply = legacy(seen, None, LEGACY_VERSIONS[0]).unwrap_or_else(|| {
            result(
                seen,
                json!({"tools": [tool(&format!("tool_{at}"))], "nextCursor": "again"}),
            )
        });
        send(stream, &reply);
    });
    let mut session = open(&desk, &vendor.endpoint(), Access::Full).unwrap();
    assert_eq!(
        session.list_tools(),
        Err(McpError::Refused("connector_reply_invalid"))
    );
    let lists = vendor
        .calls()
        .iter()
        .filter(|call| *call == "tools/list")
        .count();
    assert_eq!(lists, 2, "the same cursor twice ends the list");
    let many = |count: usize| {
        Vendor::start(move |seen, _, stream| {
            let reply = legacy(seen, None, LEGACY_VERSIONS[0]).unwrap_or_else(|| {
                let tools: Vec<Value> = (0..count).map(|at| tool(&format!("tool_{at}"))).collect();
                result(seen, json!({"tools": tools}))
            });
            send(stream, &reply);
        })
    };
    let vendor = many(TOOLS_MAX);
    let mut session = open(&desk, &vendor.endpoint(), Access::Full).unwrap();
    assert_eq!(session.list_tools().unwrap().len(), TOOLS_MAX);
    let vendor = many(TOOLS_MAX + 1);
    let mut session = open(&desk, &vendor.endpoint(), Access::Full).unwrap();
    assert_eq!(
        session.list_tools(),
        Err(McpError::Refused("connector_tools_limit"))
    );
    let vendor = Vendor::start(|seen, at, stream| {
        let reply = legacy(seen, None, LEGACY_VERSIONS[0]).unwrap_or_else(|| {
            let tools: Vec<Value> = (0..100).map(|n| tool(&format!("tool_{at}_{n}"))).collect();
            result(
                seen,
                json!({"tools": tools, "nextCursor": format!("page-{at}")}),
            )
        });
        send(stream, &reply);
    });
    let mut session = open(&desk, &vendor.endpoint(), Access::Full).unwrap();
    assert_eq!(
        session.list_tools(),
        Err(McpError::Refused("connector_tools_limit"))
    );
    let lists = vendor
        .calls()
        .iter()
        .filter(|call| *call == "tools/list")
        .count();
    assert_eq!(lists, 6, "the list stops at the first page past the limit");
}

#[test]
fn a_list_of_empty_pages_stops_at_the_page_bound() {
    let desk = desk();
    let paged = |last: Option<usize>| {
        Vendor::start(move |seen, _, stream| {
            let reply = legacy(seen, None, LEGACY_VERSIONS[0]).unwrap_or_else(|| {
                assert_eq!(seen.rpc(), "tools/list");
                let page = seen.body["params"]
                    .get("cursor")
                    .and_then(Value::as_str)
                    .map_or(1, |cursor| {
                        cursor
                            .strip_prefix("page-")
                            .unwrap()
                            .parse::<usize>()
                            .unwrap()
                    });
                let tools: Vec<Value> = if page == 1 {
                    vec![tool("search")]
                } else {
                    Vec::new()
                };
                if last == Some(page) {
                    result(seen, json!({"tools": tools}))
                } else {
                    result(
                        seen,
                        json!({"tools": tools, "nextCursor": format!("page-{}", page + 1)}),
                    )
                }
            });
            send(stream, &reply);
        })
    };
    let lists = |vendor: &Vendor| {
        vendor
            .calls()
            .iter()
            .filter(|call| *call == "tools/list")
            .count()
    };
    let vendor = paged(Some(64));
    let mut session = open(&desk, &vendor.endpoint(), Access::Full).unwrap();
    assert_eq!(names(&session.list_tools().unwrap()), ["search"]);
    assert_eq!(lists(&vendor), 64);
    let vendor = paged(None);
    let mut session = open(&desk, &vendor.endpoint(), Access::Full).unwrap();
    assert_eq!(
        session.list_tools(),
        Err(McpError::Refused("connector_tools_limit"))
    );
    assert_eq!(
        lists(&vendor),
        64,
        "the 64th page's cursor is refused, not fetched"
    );
}

#[test]
fn a_vendor_error_a_wrong_reply_and_a_request_for_input_are_named() {
    let desk = desk();
    let vendor = Vendor::start(|seen, _, stream| {
        let reply = modern(seen).unwrap_or_else(|| match seen.tool() {
            "asks" => result(
                seen,
                json!({"resultType": "input_required", "inputRequests": {"login": {
                    "method": "elicitation/create",
                    "params": {"mode": "form", "message": "Your user name"},
                }}}),
            ),
            "unknown" => rpc_error("200 OK", seen, -32602, "Unknown\u{202e} tool: x"),
            "wordy" => rpc_error("400 Bad Request", seen, -32000, &"w".repeat(5000)),
            "broken" => empty("500 Internal Server Error", &[]),
            "html" => http("502 Bad Gateway", "text/html", &[], b"<html>bad gateway</html>"),
            "other_id" => sse(&[
                json!({"jsonrpc": "2.0", "id": 999_999, "result": {"content": []}}),
            ]),
            "not_json" => http("200 OK", "application/json", &[], b"{\"jsonrpc\":"),
            "wrong_type" => http("200 OK", "text/plain", &[], b"hello"),
            "asks_back" => sse(&[
                json!({"jsonrpc": "2.0", "id": "srv-1", "method": "sampling/createMessage", "params": {}}),
                json!({"jsonrpc": "2.0", "id": seen.id(), "result": {"resultType": "complete", "content": [{"type": "text", "text": "after"}]}}),
            ]),
            _ => result(
                seen,
                json!({"resultType": "complete", "tools": [
                    tool("asks"), tool("unknown"), tool("wordy"), tool("broken"), tool("html"),
                    tool("other_id"), tool("not_json"), tool("wrong_type"), tool("asks_back"),
                ]}),
            ),
        });
        send(stream, &reply);
    });
    let mut session = open(&desk, &vendor.endpoint(), Access::Full).unwrap();
    session.list_tools().unwrap();
    let mut call = |name: &str| session.call(name, &json!({}));
    assert_eq!(
        call("asks"),
        Err(McpError::Refused("connector_needs_input"))
    );
    assert_eq!(
        call("unknown"),
        Err(McpError::Vendor("Unknown tool: x".into()))
    );
    match call("wordy") {
        Err(McpError::Vendor(text)) => {
            assert!(
                text.len() <= 300 && text.starts_with("www"),
                "{}",
                text.len()
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(call("broken"), Err(McpError::Vendor("HTTP 500".into())));
    assert_eq!(call("html"), Err(McpError::Vendor("HTTP 502".into())));
    for name in ["other_id", "not_json", "wrong_type"] {
        assert_eq!(
            call(name),
            Err(McpError::Refused("connector_reply_invalid")),
            "{name}"
        );
    }
    assert_eq!(
        call("asks_back").map(|reply| reply.text),
        Ok("after".into())
    );
    assert!(
        vendor
            .seen()
            .iter()
            .all(|seen| seen.body.get("result").is_none() && seen.body.get("error").is_none()),
        "the engine never answers a request the vendor sends on a stream"
    );
}

#[test]
fn a_server_that_speaks_no_version_the_engine_does_is_named() {
    let desk = desk();
    let vendor = Vendor::start(|seen, _, stream| {
        let reply = match seen.rpc() {
            "server/discover" => json_reply(
                "400 Bad Request",
                &[],
                &json!({"jsonrpc": "2.0", "id": seen.id(), "error": {
                    "code": -32022,
                    "message": "Unsupported protocol version",
                    "data": {"supported": ["2027-03-01"], "requested": MODERN_VERSION},
                }}),
            ),
            other => panic!("{other}"),
        };
        send(stream, &reply);
    });
    assert_eq!(
        open(&desk, &vendor.endpoint(), Access::Full).unwrap_err(),
        McpError::Refused("connector_protocol_unsupported")
    );
    assert_eq!(vendor.calls(), ["server/discover"]);
    let vendor = Vendor::start(|seen, _, stream| {
        let reply = match seen.rpc() {
            "server/discover" => json_reply(
                "400 Bad Request",
                &[],
                &json!({"jsonrpc": "2.0", "id": seen.id(), "error": {
                    "code": -32022,
                    "message": "Unsupported protocol version",
                    "data": {"supported": ["2025-06-18"], "requested": MODERN_VERSION},
                }}),
            ),
            _ => legacy(seen, Some("sess-1"), "2025-06-18").unwrap(),
        };
        send(stream, &reply);
    });
    open(&desk, &vendor.endpoint(), Access::Full).unwrap();
    assert_eq!(
        vendor.calls(),
        ["server/discover", "initialize", "notifications/initialized"]
    );
    let vendor = Vendor::start(|seen, _, stream| {
        let reply = legacy(seen, Some("sess-1"), "2024-11-05").unwrap();
        send(stream, &reply);
    });
    assert_eq!(
        open(&desk, &vendor.endpoint(), Access::Full).unwrap_err(),
        McpError::Refused("connector_protocol_unsupported")
    );
    assert_eq!(vendor.calls(), ["server/discover", "initialize"]);
    for session in ["has space", "caf\u{e9}", ""] {
        let vendor = Vendor::start(move |seen, _, stream| {
            let reply = legacy(seen, Some(session), LEGACY_VERSIONS[0]).unwrap();
            send(stream, &reply);
        });
        assert_eq!(
            open(&desk, &vendor.endpoint(), Access::Full).unwrap_err(),
            McpError::Refused("connector_reply_invalid"),
            "{session:?}"
        );
    }
    let vendor = Vendor::start(|seen, _, stream| {
        let long = "s".repeat(600);
        let reply = legacy(seen, Some(long.as_str()), LEGACY_VERSIONS[0]).unwrap();
        send(stream, &reply);
    });
    assert_eq!(
        open(&desk, &vendor.endpoint(), Access::Full).unwrap_err(),
        McpError::Refused("connector_reply_invalid")
    );
}

#[test]
fn a_request_over_the_limit_or_a_bad_tool_name_never_leaves_the_engine() {
    let desk = desk();
    let vendor = Vendor::start(|seen, _, stream| {
        let reply = modern(seen).unwrap_or_else(|| match seen.rpc() {
            "tools/list" => result(
                seen,
                json!({"resultType": "complete", "tools": [tool("create_issue")]}),
            ),
            _ => result(seen, text_result("made")),
        });
        send(stream, &reply);
    });
    let mut session = open(&desk, &vendor.endpoint(), Access::Full).unwrap();
    session.list_tools().unwrap();
    let before = vendor.seen().len();
    assert_eq!(
        session.call(
            "create_issue",
            &json!({"body": "b".repeat(REQUEST_BYTES_MAX)})
        ),
        Err(McpError::Refused("connector_request_too_large"))
    );
    for name in ["create issue", "create_issue\r\nX-Evil: 1", "", "caf\u{e9}"] {
        assert_eq!(
            session.call(name, &json!({})),
            Err(McpError::Refused("connector_tool_name_invalid")),
            "{name:?}"
        );
    }
    for args in [json!([1]), json!("text"), json!(7), json!(null)] {
        assert_eq!(
            session.call("create_issue", &args),
            Err(McpError::Refused("connector_arguments_invalid")),
            "{args}"
        );
    }
    assert_eq!(
        session.call("delete_everything", &json!({})),
        Err(McpError::Refused("connector_tool_unknown")),
        "a tool this server never listed is not called in the per-request era"
    );
    assert_eq!(vendor.seen().len(), before);
    let fits = json!({"body": "b".repeat(REQUEST_BYTES_MAX - 1024)});
    assert_eq!(
        session.call("create_issue", &fits).map(|reply| reply.text),
        Ok("made".into())
    );
    assert_eq!(vendor.seen().len(), before + 1);
}

#[test]
fn a_basic_key_is_sent_as_the_user_and_the_key_and_a_missing_key_sends_nothing() {
    let desk = desk();
    let vendor = Vendor::start(|seen, _, stream| {
        let reply = modern(seen)
            .unwrap_or_else(|| result(seen, json!({"resultType": "complete", "tools": []})));
        send(stream, &reply);
    });
    let mut session = McpSession::open_with(
        &desk.case.root,
        &vendor.endpoint(),
        &desk.basic,
        Access::Full,
        SizeCap::Kib256,
        no_lookup(),
    )
    .unwrap();
    session.list_tools().unwrap();
    let basic = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{USER}:{KEY}"))
    );
    let seen = vendor.seen();
    assert_eq!(seen.len(), 2);
    for request in &seen {
        assert_eq!(request.header("authorization"), Some(basic.as_str()));
        assert_eq!(
            request
                .headers
                .iter()
                .filter(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                .count(),
            1
        );
        assert!(!request.path.contains(KEY) && !request.body.to_string().contains(KEY));
    }
    for shown in [
        format!("{session:?}"),
        format!("{:?}", desk.basic),
        format!("{:?}", desk.bearer),
    ] {
        assert!(!shown.contains(KEY) && !shown.contains(USER), "{shown}");
    }
    store::remove(&desk.case.db, &desk.case.root, "desk").unwrap();
    assert_eq!(
        session.list_tools(),
        Err(McpError::Refused("connector_key_missing"))
    );
    assert_eq!(
        McpSession::open_with(
            &desk.case.root,
            &vendor.endpoint(),
            &desk.basic,
            Access::Full,
            SizeCap::Kib256,
            no_lookup(),
        )
        .unwrap_err(),
        McpError::Refused("connector_key_missing")
    );
    assert_eq!(vendor.seen().len(), 2, "no request leaves without its key");
    let scratch = desk.case.root.join(crate::app_plugins::http::SCRATCH);
    assert_eq!(
        std::fs::read_dir(&scratch).unwrap().count(),
        0,
        "no request body is left behind"
    );
}

#[test]
fn while_writes_are_off_the_vendor_read_only_address_or_header_is_used() {
    let desk = desk();
    let serve = || {
        Vendor::start(|seen, _, stream| {
            let reply = modern(seen).unwrap_or_else(|| {
                result(
                    seen,
                    json!({"resultType": "complete", "tools": [tool("list_issues")]}),
                )
            });
            send(stream, &reply);
        })
    };
    let vendor = serve();
    let mut by_header = vendor.endpoint();
    by_header.read_only = ReadOnly::Header {
        name: "X-MCP-Readonly".into(),
        value: "true".into(),
    };
    let mut session = open(&desk, &by_header, Access::ReadOnly).unwrap();
    session.list_tools().unwrap();
    let seen = vendor.seen();
    assert_eq!(seen.len(), 2);
    for request in &seen {
        assert_eq!(request.header("x-mcp-readonly"), Some("true"));
        assert_eq!(request.path, "/mcp");
    }
    let vendor = serve();
    by_header.url = vendor.endpoint().url;
    let mut session = open(&desk, &by_header, Access::Full).unwrap();
    session.list_tools().unwrap();
    assert!(
        vendor
            .seen()
            .iter()
            .all(|seen| seen.header("x-mcp-readonly").is_none())
    );
    let vendor = serve();
    let mut by_url = vendor.endpoint();
    by_url.read_only = ReadOnly::Url(
        url::Url::parse(&format!("http://127.0.0.1:{}/mcp/readonly", vendor.port)).unwrap(),
    );
    let mut session = open(&desk, &by_url, Access::ReadOnly).unwrap();
    session.list_tools().unwrap();
    let mut session = open(&desk, &by_url, Access::Full).unwrap();
    session.list_tools().unwrap();
    let paths: Vec<String> = vendor.seen().into_iter().map(|seen| seen.path).collect();
    assert_eq!(paths, ["/mcp/readonly", "/mcp/readonly", "/mcp", "/mcp"]);
    let vendor = serve();
    let mut session = open(&desk, &vendor.endpoint(), Access::ReadOnly).unwrap();
    session.list_tools().unwrap();
    assert!(
        vendor.seen().iter().all(|seen| seen.path == "/mcp"),
        "a vendor with no read-only mode is called at its one address"
    );
}

#[test]
fn a_stream_the_vendor_keeps_open_ends_at_the_response() {
    let desk = desk();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let released = Mutex::new(released);
    let (told, heard) = std::sync::mpsc::channel::<bool>();
    let told = Mutex::new(told);
    let vendor = Vendor::start(move |seen, _, stream| {
        if let Some(reply) = modern(seen) {
            send(stream, &reply);
            return;
        }
        send(stream, SSE_HEAD);
        send(
            stream,
            &event(&json!({"jsonrpc": "2.0", "id": seen.id(), "result": {
                "resultType": "complete", "tools": [tool("list_issues")],
            }})),
        );
        let in_time = released
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(20))
            .is_ok();
        let _ = told.lock().unwrap().send(in_time);
    });
    let mut session = open(&desk, &vendor.endpoint(), Access::Full).unwrap();
    let tools = session.list_tools().unwrap();
    release.send(()).unwrap();
    assert_eq!(names(&tools), ["list_issues"]);
    assert!(
        heard.recv_timeout(Duration::from_secs(60)).unwrap(),
        "the list came back while the vendor still held the stream open"
    );
}

#[test]
fn the_curl_command_holds_no_address_header_or_key() {
    let command = wire::command("=https");
    assert_eq!(command.get_program(), "/usr/bin/curl");
    let args: Vec<&str> = command
        .get_args()
        .map(|arg| arg.to_str().unwrap())
        .collect();
    assert_eq!(
        args,
        [
            "-q",
            "--silent",
            "--globoff",
            "--no-buffer",
            "--include",
            "--proto",
            "=https",
            "--max-redirs",
            "0",
            "--noproxy",
            "*",
            "--connect-timeout",
            "10",
            "--max-time",
            "60",
            "--config",
            "-",
        ]
    );
    let envs: Vec<_> = command.get_envs().collect();
    assert_eq!(envs.len(), 1);
    assert_eq!(envs[0].0, "PATH");
    assert_eq!(command.get_current_dir(), Some(std::path::Path::new("/")));
}

fn address(text: &str) -> IpAddr {
    text.parse().unwrap()
}

fn found(addresses: &[&str], asked: Arc<Mutex<Vec<(String, u16)>>>) -> ProviderReach {
    let addresses: Vec<SocketAddr> = addresses
        .iter()
        .map(|text| SocketAddr::new(address(text), 443))
        .collect();
    ProviderReach::new(
        move |name, port| {
            asked.lock().unwrap().push((name.to_owned(), port));
            Ok(addresses.clone())
        },
        |_| panic!("the client never dials by itself"),
    )
}

fn remote(url: &str, hosts: &[&str]) -> McpEndpoint {
    McpEndpoint {
        url: url::Url::parse(url).unwrap(),
        read_only: ReadOnly::None,
        hosts: hosts.iter().map(|host| (*host).to_owned()).collect(),
    }
}

#[test]
fn a_name_that_resolves_to_a_private_or_mapped_address_is_refused() {
    let desk = desk();
    let private = [
        "10.0.0.5",
        "172.16.0.1",
        "192.168.1.10",
        "127.0.0.1",
        "169.254.169.254",
        "100.64.0.1",
        "0.0.0.0",
        "0.1.2.3",
        "224.0.0.1",
        "240.0.0.1",
        "255.255.255.255",
        "192.0.2.1",
        "198.18.0.1",
        "192.0.0.8",
        "::1",
        "::",
        "::ffff:10.0.0.5",
        "::ffff:127.0.0.1",
        "::10.0.0.5",
        "fc00::1",
        "fd12:3456::1",
        "fe80::1",
        "fec0::1",
        "ff02::1",
        "2001:db8::1",
        "2001::1",
        "64:ff9b::a00:5",
        "2002:a00:5::1",
    ];
    for text in private {
        assert!(!guard::public(address(text)), "{text}");
    }
    for text in [
        "1.1.1.1",
        "93.184.216.34",
        "2606:4700::1111",
        "::ffff:1.1.1.1",
        "64:ff9b::101:101",
        "2002:101:101::1",
    ] {
        assert!(guard::public(address(text)), "{text}");
    }
    let endpoint = remote("https://mcp.vendor.example/mcp", &["mcp.vendor.example"]);
    let asked = Arc::new(Mutex::new(Vec::new()));
    for answer in [
        &["10.0.0.5"][..],
        &["::ffff:10.0.0.5"],
        &["1.1.1.1", "192.168.1.10"],
        &["2606:4700::1111", "fe80::1"],
    ] {
        assert_eq!(
            McpSession::open_with(
                &desk.case.root,
                &endpoint,
                &desk.bearer,
                Access::Full,
                SizeCap::Kib256,
                found(answer, asked.clone()),
            )
            .unwrap_err(),
            McpError::Refused("connector_host_private"),
            "{answer:?}"
        );
    }
    assert_eq!(
        McpSession::open_with(
            &desk.case.root,
            &endpoint,
            &desk.bearer,
            Access::Full,
            SizeCap::Kib256,
            found(&[], asked.clone()),
        )
        .unwrap_err(),
        McpError::Refused("connector_host_unknown")
    );
    let failing = ProviderReach::new(
        |_, _| Err(std::io::Error::from(std::io::ErrorKind::TimedOut)),
        |_| Ok(()),
    );
    assert_eq!(
        McpSession::open_with(
            &desk.case.root,
            &endpoint,
            &desk.bearer,
            Access::Full,
            SizeCap::Kib256,
            failing,
        )
        .unwrap_err(),
        McpError::Refused("connector_host_unknown")
    );
    assert!(
        asked
            .lock()
            .unwrap()
            .iter()
            .all(|asked| *asked == ("mcp.vendor.example".to_owned(), 443))
    );
    assert_eq!(asked.lock().unwrap().len(), 5);
    let mut read_only = remote(
        "https://mcp.vendor.example:8443/mcp",
        &["mcp.vendor.example", "ro.vendor.example"],
    );
    read_only.read_only = ReadOnly::Url(url::Url::parse("https://ro.vendor.example/mcp").unwrap());
    let asked = Arc::new(Mutex::new(Vec::new()));
    for (access, host, port) in [
        (Access::ReadOnly, "ro.vendor.example", 443),
        (Access::Full, "mcp.vendor.example", 8443),
    ] {
        assert_eq!(
            McpSession::open_with(
                &desk.case.root,
                &read_only,
                &desk.bearer,
                access,
                SizeCap::Kib256,
                found(&["10.0.0.5"], asked.clone()),
            )
            .unwrap_err(),
            McpError::Refused("connector_host_private")
        );
        assert_eq!(asked.lock().unwrap().last(), Some(&(host.to_owned(), port)));
    }
    let asked = Arc::new(Mutex::new(Vec::new()));
    assert_eq!(
        guard::pins(
            "mcp.vendor.example",
            443,
            &found(&["93.184.216.34", "2606:4700::1111"], asked.clone()),
        ),
        Ok("mcp.vendor.example:443:93.184.216.34,[2606:4700::1111]".to_owned())
    );
    let ten: Vec<String> = (1..=10).map(|n| format!("93.184.216.{n}")).collect();
    let ten: Vec<&str> = ten.iter().map(String::as_str).collect();
    assert_eq!(
        guard::pins("mcp.vendor.example", 443, &found(&ten, asked.clone()))
            .unwrap()
            .matches(',')
            .count(),
        7,
        "at most eight addresses are pinned"
    );
    let mut eleven = ten.clone();
    eleven.push("10.0.0.5");
    assert_eq!(
        guard::pins("mcp.vendor.example", 443, &found(&eleven, asked)),
        Err(McpError::Refused("connector_host_private")),
        "every address is checked, also one past those pinned"
    );
}

#[test]
fn the_production_parser_refuses_plain_http_with_a_credential() {
    let plain = BOARD.replacen("https://mcp.board.example", "http://mcp.board.example", 1);
    assert_eq!(
        manifest::parse(&plain).unwrap_err().code,
        "app_plugin_connector_url_not_https"
    );
    let loopback = BOARD
        .replacen(
            "https://mcp.board.example/mcp",
            "http://127.0.0.1:8080/mcp",
            1,
        )
        .replacen("[mcp.board.example]", "[127.0.0.1]", 1);
    assert!(manifest::parse(&loopback).is_err());
    let desk = desk();
    let vendor = Vendor::start(|_, _, _| panic!("a refused address is never called"));
    let port = vendor.port;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let mut undeclared = vendor.endpoint();
    undeclared.hosts = vec!["mcp.vendor.example".into()];
    let mut read_only_elsewhere = vendor.endpoint();
    read_only_elsewhere.read_only =
        ReadOnly::Url(url::Url::parse("http://mcp.vendor.example/mcp").unwrap());
    let refused = [
        (
            remote("http://mcp.vendor.example/mcp", &["mcp.vendor.example"]),
            Access::Full,
        ),
        (
            remote(&format!("http://localhost:{port}/mcp"), &["localhost"]),
            Access::Full,
        ),
        (
            remote(&format!("https://127.0.0.1:{port}/mcp"), &["127.0.0.1"]),
            Access::Full,
        ),
        (
            remote(&format!("http://127.0.0.2:{port}/mcp"), &["127.0.0.2"]),
            Access::Full,
        ),
        (
            remote(&format!("http://[::1]:{port}/mcp"), &["[::1]"]),
            Access::Full,
        ),
        (undeclared, Access::Full),
        (read_only_elsewhere, Access::ReadOnly),
        (
            remote("https://mcp.vendor.example/mcp", &["other.vendor.example"]),
            Access::Full,
        ),
        (
            remote("https://93.184.216.34/mcp", &["93.184.216.34"]),
            Access::Full,
        ),
        (
            remote(
                "https://user:pw@mcp.vendor.example/mcp",
                &["mcp.vendor.example"],
            ),
            Access::Full,
        ),
        (
            remote(
                "https://mcp.vendor.example/mcp?x=1",
                &["mcp.vendor.example"],
            ),
            Access::Full,
        ),
        (
            remote("https://mcp.vendor.example/mcp#x", &["mcp.vendor.example"]),
            Access::Full,
        ),
        (
            remote("ftp://mcp.vendor.example/mcp", &["mcp.vendor.example"]),
            Access::Full,
        ),
    ];
    for (endpoint, access) in refused {
        assert_eq!(
            McpSession::open_with(
                &desk.case.root,
                &endpoint,
                &desk.bearer,
                access,
                SizeCap::Kib256,
                found(&["93.184.216.34"], asked.clone()),
            )
            .unwrap_err(),
            McpError::Refused("connector_address_refused"),
            "{}",
            endpoint.url
        );
    }
    assert!(
        asked.lock().unwrap().is_empty(),
        "a refused address is never looked up"
    );
    assert!(vendor.seen().is_empty());
}

#[test]
fn every_mcp_client_code_has_a_plain_sentence() {
    let mut sorted = MCP_CODES.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, MCP_CODES);
    for code in MCP_CODES {
        let plain = crate::connectors::plain_problem(code).unwrap_or_else(|| panic!("{code}"));
        assert!(!plain.contains('_') && plain.ends_with('.'), "{code}");
    }
    for error in [
        McpError::Unauthorized { scope: None },
        McpError::TooLarge,
        McpError::Timeout,
        McpError::Refused("connector_call_failed"),
        McpError::Vendor("x".into()),
    ] {
        assert!(MCP_CODES.contains(&error.code()), "{error:?}");
    }
}

#[test]
fn curl_reaches_the_address_the_guard_pinned_and_looks_no_name_up_itself() {
    let desk = desk();
    let vendor = Vendor::start(|seen, _, stream| {
        send(stream, &result(seen, json!({"resultType": "complete"})));
    });
    let host = format!("pinned.invalid:{}", vendor.port);
    let url = url::Url::parse(&format!("http://{host}/mcp")).unwrap();
    let pins = format!("{host}:127.0.0.1");
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();
    let reply = super::wire::send(
        &desk.case.root,
        &super::wire::Request {
            url: &url,
            verb: super::wire::Verb::Post(body.as_bytes()),
            pins: Some(&pins),
            headers: &[],
            auth: &desk.bearer,
            wait: Some(json!(1)),
        },
    )
    .unwrap();
    assert_eq!(reply.status, 200);
    let seen = vendor.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].header("host"), Some(host.as_str()));
    assert_eq!(seen[0].rpc(), "ping");
}

#[test]
fn every_request_looks_the_vendor_up_again_and_one_that_turns_private_is_not_sent() {
    let desk = desk();
    let vendor = Vendor::start(|seen, _, stream| {
        if seen.verb == "DELETE" {
            send(stream, &empty("200 OK", &[]));
            return;
        }
        let reply = legacy(seen, Some("sess-1"), "2025-06-18").unwrap_or_else(|| {
            match (seen.rpc(), seen.body["params"].get("cursor")) {
                ("tools/list", None) => result(
                    seen,
                    json!({"tools": [tool("list_issues")], "nextCursor": "p2"}),
                ),
                ("tools/list", Some(_)) => result(seen, json!({"tools": [tool("get_issue")]})),
                ("tools/call", _) => result(seen, text_result("ran")),
                other => panic!("{other:?}"),
            }
        });
        send(stream, &reply);
    });
    let looked = Arc::new(AtomicUsize::new(0));
    let private_from = Arc::new(AtomicUsize::new(usize::MAX));
    let (count, turn) = (looked.clone(), private_from.clone());
    let reach = ProviderReach::new(
        move |name, port| {
            assert_eq!(name, "127.0.0.1");
            let at = count.fetch_add(1, Ordering::SeqCst) + 1;
            let address = if at >= turn.load(Ordering::SeqCst) {
                "10.0.0.5"
            } else {
                "1.1.1.1"
            };
            Ok(vec![SocketAddr::new(address.parse().unwrap(), port)])
        },
        |_| panic!("the client never dials by itself"),
    );
    let mut session = McpSession::open_vetting(
        &desk.case.root,
        &vendor.endpoint(),
        &desk.bearer,
        Access::Full,
        SizeCap::Kib256,
        reach,
    )
    .unwrap();
    let lookups = || looked.load(Ordering::SeqCst);
    assert_eq!(
        vendor.calls(),
        ["server/discover", "initialize", "notifications/initialized"]
    );
    assert_eq!(lookups(), 3);
    let tools = session.list_tools().unwrap();
    assert_eq!(names(&tools), ["list_issues", "get_issue"]);
    assert_eq!(lookups(), 5);
    assert!(session.call("list_issues", &json!({})).is_ok());
    assert_eq!(lookups(), 6);
    assert_eq!(vendor.seen().len(), 6);

    private_from.store(7, Ordering::SeqCst);
    assert_eq!(
        session.call("list_issues", &json!({})),
        Err(McpError::Refused("connector_host_private"))
    );
    assert_eq!(lookups(), 7);
    assert_eq!(vendor.seen().len(), 6);

    private_from.store(usize::MAX, Ordering::SeqCst);
    assert!(session.call("get_issue", &json!({})).is_ok());
    assert_eq!(lookups(), 8);
    session.close();
    assert_eq!(lookups(), 9);
    let seen = vendor.seen();
    assert_eq!(seen.len(), 8);
    assert_eq!(seen[7].verb, "DELETE");
    assert_eq!(seen[7].header("mcp-session-id"), Some("sess-1"));
}
