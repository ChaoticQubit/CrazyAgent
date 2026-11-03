use rusqlite::{Connection, Transaction, TransactionBehavior};
use serde_json::{Map, Value, json};
use std::io::{BufRead, ErrorKind, Read, Write};
use trackwright_core::catalog::{OpSpec, catalog, op_schema};
use trackwright_core::host::Host;
use trackwright_core::model::{Actor, REQUEST_BYTES_MAX, Refusal, codes};
use trackwright_core::ops::dispatch;
use trackwright_core::reply::Reply;
use trackwright_core::requests::Request;

/// The server name an MCP client sees in the `initialize` reply.
pub const SERVER_NAME: &str = "trackwright";

/// The longest JSON-RPC line the server reads, newline excluded: one request at the core's
/// 16 KiB bound plus its JSON-RPC wrapping and escapes. A longer line is answered with
/// `Request too large` and its top-level id when it has one, skipped to its newline without
/// buffering it, and serving goes on.
pub const LINE_BYTES_MAX: usize = 2 * REQUEST_BYTES_MAX;

/// The most requests one JSON-RPC batch may hold (MCP 2025-03-26 batches). A longer or an
/// empty batch gets one `Invalid Request` and none of it runs, so one line never fans out
/// into an unbounded reply.
pub const BATCH_REQUESTS_MAX: usize = 32;

/// The MCP protocol versions the server speaks, oldest first. A client that asks for any
/// other version is offered the latest, which it may accept or close on.
pub const PROTOCOL_VERSIONS: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

const PROTOCOL_LATEST: &str = PROTOCOL_VERSIONS[PROTOCOL_VERSIONS.len() - 1];
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const ID_BYTES_MAX: usize = 256;

const _: () = assert!(LINE_BYTES_MAX > REQUEST_BYTES_MAX);
const _: () = assert!(REQUEST_BYTES_MAX == 16 * 1024);
const _: () = assert!(BATCH_REQUESTS_MAX == 32);
const _: () = assert!(ID_BYTES_MAX < LINE_BYTES_MAX);

/// One MCP client's link to a tracker: the database connection, the host that answers for
/// links, evidence and authority, and the actor every call runs as. The actor is fixed when
/// the server starts (the standalone `--actor` argument or a host's own identity of the
/// client) and no call can change it.
pub struct Session<H: Host> {
    connection: Connection,
    host: H,
    actor: Actor,
}

impl<H: Host> Session<H> {
    /// Binds the three. Inputs: an open connection, outside any transaction, to a database
    /// holding the migrated core schema; the host; and the client's actor, never one of the
    /// core's reserved `host:` actors. Output: the session.
    pub fn new(connection: Connection, host: H, actor: Actor) -> Self {
        assert!(
            !actor.is_host(),
            "an MCP client never acts as the tracker itself"
        );
        assert!(
            connection.is_autocommit(),
            "a session starts outside any transaction"
        );
        Self {
            connection,
            host,
            actor,
        }
    }
}

/// Serves the catalog as an MCP server over stdio JSON-RPC, one message or batch per line,
/// until the client closes its input. Inputs: the client's input and output, and the session
/// every call runs in. Output: Ok at the end of input, or the input or output failure, or the
/// failure of a database that cannot end a call's transaction. Every request gets its reply,
/// a batch gets one array of them, and notifications get none.
pub fn serve<H: Host>(
    mut input: impl BufRead,
    mut output: impl Write,
    session: &Session<H>,
) -> std::io::Result<()> {
    let mut line = Vec::with_capacity(LINE_BYTES_MAX + 1);
    loop {
        let answered = match read_line(&mut input, &mut line)? {
            Line::End => return Ok(()),
            Line::TooLong(id) => Some(failure(id, INVALID_REQUEST, "Request too large")),
            Line::Read if line.trim_ascii().is_empty() => None,
            Line::Read => answer(line.trim_ascii(), session)?,
        };
        if let Some(message) = answered {
            send(&mut output, &message)?;
        }
    }
}

enum Line {
    Read,
    TooLong(Value),
    End,
}

fn read_line(input: &mut impl BufRead, line: &mut Vec<u8>) -> std::io::Result<Line> {
    line.clear();
    let read = Read::take(&mut *input, LINE_BYTES_MAX as u64 + 1).read_until(b'\n', line)?;
    assert_eq!(read, line.len());
    assert!(line.len() <= LINE_BYTES_MAX + 1);
    if read == 0 {
        return Ok(Line::End);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
        return Ok(Line::Read);
    }
    if line.len() <= LINE_BYTES_MAX {
        return Ok(Line::Read);
    }
    let mut scan = IdScan::default();
    scan.feed(line);
    skip_line(input, &mut scan)?;
    Ok(Line::TooLong(scan.id()))
}

fn skip_line(input: &mut impl BufRead, scan: &mut IdScan) -> std::io::Result<()> {
    loop {
        let buffer = match input.fill_buf() {
            Ok(buffer) => buffer,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if buffer.is_empty() {
            return Ok(());
        }
        match buffer.iter().position(|&byte| byte == b'\n') {
            Some(end) => {
                scan.feed(&buffer[..end]);
                input.consume(end + 1);
                return Ok(());
            }
            None => {
                scan.feed(buffer);
                let length = buffer.len();
                input.consume(length);
            }
        }
    }
}

/// Finds the top-level `id` of one JSON-RPC object fed in pieces, keeping at most
/// `ID_BYTES_MAX` bytes of it, so an oversized line is still answered with its own id.
/// Strings, escapes and nested values are skipped; the id is null when the line is not an
/// object, has no top-level id, or its id is not a short string or number.
#[derive(Default)]
struct IdScan {
    depth: usize,
    in_string: bool,
    escaped: bool,
    in_value: bool,
    reading_key: bool,
    key: Vec<u8>,
    key_other: bool,
    capturing: bool,
    captured: Vec<u8>,
    overflow: bool,
    found: Option<Vec<u8>>,
    stopped: bool,
}

impl IdScan {
    fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.stopped {
                return;
            }
            self.step(byte);
        }
    }

    fn step(&mut self, byte: u8) {
        if self.in_string {
            self.string_byte(byte);
            return;
        }
        if self.depth == 0 {
            match byte {
                b'{' => self.depth = 1,
                b' ' | b'\t' | b'\r' => {}
                _ => self.stopped = true,
            }
            return;
        }
        if self.depth == 1 {
            match byte {
                b',' => return self.end_member(),
                b'}' | b']' => {
                    self.end_member();
                    self.stopped = true;
                    return;
                }
                b':' if !self.in_value => {
                    self.in_value = true;
                    self.capturing = self.key == b"id" && !self.key_other && self.found.is_none();
                    return;
                }
                _ => {}
            }
        }
        self.keep(byte);
        match byte {
            b'"' => {
                self.in_string = true;
                self.reading_key = self.depth == 1 && !self.in_value;
                if self.reading_key {
                    self.key.clear();
                    self.key_other = false;
                }
            }
            b'{' | b'[' => self.depth = self.depth.saturating_add(1),
            b'}' | b']' => self.depth -= 1,
            _ => {}
        }
        assert!(
            self.depth >= 1,
            "a nested close never ends the top-level object"
        );
    }

    fn string_byte(&mut self, byte: u8) {
        self.keep(byte);
        if self.escaped {
            self.escaped = false;
            self.key_other |= self.reading_key;
            return;
        }
        match byte {
            b'\\' => self.escaped = true,
            b'"' => {
                self.in_string = false;
                self.reading_key = false;
            }
            _ if self.reading_key && self.key.len() < 2 => self.key.push(byte),
            _ => self.key_other |= self.reading_key,
        }
        assert!(self.key.len() <= 2);
    }

    fn keep(&mut self, byte: u8) {
        if !self.capturing || self.overflow {
            return;
        }
        if self.captured.len() == ID_BYTES_MAX {
            self.overflow = true;
            return;
        }
        self.captured.push(byte);
        assert!(self.captured.len() <= ID_BYTES_MAX);
    }

    fn end_member(&mut self) {
        if self.capturing && !self.overflow {
            assert!(
                self.found.is_none(),
                "only the first top-level id is captured"
            );
            self.found = Some(std::mem::take(&mut self.captured));
        }
        self.capturing = false;
        self.in_value = false;
    }

    fn id(&self) -> Value {
        let Some(bytes) = &self.found else {
            return Value::Null;
        };
        assert!(bytes.len() <= ID_BYTES_MAX);
        match serde_json::from_slice::<Value>(bytes.trim_ascii()) {
            Ok(id) if id.is_string() || id.is_number() => id,
            _ => Value::Null,
        }
    }
}

fn send(output: &mut impl Write, message: &Value) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    assert!(!line.contains(&b'\n'), "compact JSON holds no raw newline");
    line.push(b'\n');
    output.write_all(&line)?;
    output.flush()
}

fn failure(id: Value, code: i64, message: &str) -> Value {
    assert!(
        (-32768..=-32000).contains(&code),
        "a JSON-RPC reserved error code"
    );
    assert!(id.is_null() || id.is_string() || id.is_number());
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn answer<H: Host>(text: &[u8], session: &Session<H>) -> std::io::Result<Option<Value>> {
    let Ok(message) = serde_json::from_slice::<Value>(text) else {
        return Ok(Some(failure(Value::Null, PARSE_ERROR, "Parse error")));
    };
    let batch = match message {
        Value::Array(batch) => batch,
        single => return answer_one(&single, session),
    };
    if batch.is_empty() || batch.len() > BATCH_REQUESTS_MAX {
        let why = "Invalid Request: a batch holds 1 to 32 requests";
        return Ok(Some(failure(Value::Null, INVALID_REQUEST, why)));
    }
    let mut replies = Vec::with_capacity(batch.len());
    for message in &batch {
        if let Some(reply) = answer_one(message, session)? {
            replies.push(reply);
        }
    }
    assert!(replies.len() <= batch.len());
    if replies.is_empty() {
        return Ok(None);
    }
    Ok(Some(Value::Array(replies)))
}

fn answer_one<H: Host>(message: &Value, session: &Session<H>) -> std::io::Result<Option<Value>> {
    let Some(message) = message.as_object() else {
        let why = "Invalid Request: a JSON-RPC request is an object";
        return Ok(Some(failure(Value::Null, INVALID_REQUEST, why)));
    };
    let Some(id) = message.get("id") else {
        return Ok(None);
    };
    if !(id.is_string() || id.is_number()) {
        let why = "Invalid Request: the id is a string or a number";
        return Ok(Some(failure(Value::Null, INVALID_REQUEST, why)));
    }
    let version = message.get("jsonrpc").and_then(Value::as_str);
    let (Some("2.0"), Some(method)) = (version, message.get("method").and_then(Value::as_str))
    else {
        let why = "Invalid Request: jsonrpc 2.0 and a method name";
        return Ok(Some(failure(id.clone(), INVALID_REQUEST, why)));
    };
    let params = message.get("params");
    let result = match method {
        "initialize" => initialized(params),
        "ping" => json!({}),
        "tools/list" => tools(),
        "tools/call" => match call(params, session)? {
            Ok(result) => result,
            Err(why) => return Ok(Some(failure(id.clone(), INVALID_PARAMS, why))),
        },
        _ => {
            return Ok(Some(failure(
                id.clone(),
                METHOD_NOT_FOUND,
                "Method not found",
            )));
        }
    };
    Ok(Some(json!({"jsonrpc": "2.0", "id": id, "result": result})))
}

fn initialized(params: Option<&Value>) -> Value {
    let asked = params
        .and_then(|params| params.get("protocolVersion"))
        .and_then(Value::as_str);
    let version = asked
        .and_then(|asked| PROTOCOL_VERSIONS.into_iter().find(|known| *known == asked))
        .unwrap_or(PROTOCOL_LATEST);
    assert!(PROTOCOL_VERSIONS.contains(&version));
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
    })
}

fn tools() -> Value {
    let tools: Vec<Value> = catalog().iter().map(tool).collect();
    assert!(!tools.is_empty(), "the catalog is never empty");
    assert_eq!(tools.len(), catalog().len());
    json!({"tools": tools})
}

fn tool(spec: &OpSpec) -> Value {
    let schema = op_schema(spec)["args"].clone();
    assert_eq!(schema["type"], "object", "an MCP tool's input is an object");
    assert!(!spec.summary.is_empty(), "every op says what it does");
    json!({"name": spec.kind.as_str(), "description": spec.summary, "inputSchema": schema})
}

type Called = Result<Value, &'static str>;

fn call<H: Host>(params: Option<&Value>, session: &Session<H>) -> std::io::Result<Called> {
    let Some(params) = params.and_then(Value::as_object) else {
        return Ok(Err("tools/call takes an object with the tool's name"));
    };
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Ok(Err("tools/call takes the tool's name"));
    };
    let Some(spec) = catalog().iter().find(|spec| spec.kind.as_str() == name) else {
        return Ok(Err("Unknown tool"));
    };
    let empty = Value::Object(Map::new());
    let arguments = params.get("arguments").unwrap_or(&empty);
    if !arguments.is_object() {
        return Ok(Err("the tool's arguments are an object"));
    }
    let input = serde_json::to_vec(arguments).expect("a parsed JSON value serializes");
    let reply = run(session, spec, &input)?;
    let text = reply.json();
    assert!(text.starts_with('{'), "the envelope is a JSON object");
    let result = json!({"content": [{"type": "text", "text": text}], "isError": !reply.is_ok()});
    Ok(Ok(result))
}

fn run<H: Host>(session: &Session<H>, spec: &OpSpec, input: &[u8]) -> std::io::Result<Reply> {
    let request = match Request::parse(spec.kind.as_str(), input) {
        Ok(request) => request,
        Err(refusal) => return Ok(Reply::refused(&refusal)),
    };
    assert!(
        session.connection.is_autocommit(),
        "no call starts inside another"
    );
    let behavior = if spec.kind.is_write() {
        TransactionBehavior::Immediate
    } else {
        TransactionBehavior::Deferred
    };
    let Ok(tx) = Transaction::new_unchecked(&session.connection, behavior) else {
        let why = "the tracker database is busy or unreadable; nothing was written";
        return Ok(Reply::refused(&Refusal::new(codes::STORE_FAILED, why)));
    };
    let reply = dispatch(&tx, &session.host, &session.actor, request);
    let ended = if reply.is_ok() {
        tx.commit()
    } else {
        tx.rollback()
    };
    if ended.is_ok() {
        assert!(
            session.connection.is_autocommit(),
            "an ended call leaves no transaction"
        );
        return Ok(reply);
    }
    if !session.connection.is_autocommit() {
        let why = "the tracker database could not end a call's transaction";
        return Err(std::io::Error::other(why));
    }
    let why = "the tracker database could not finish the call; nothing was written";
    Ok(Reply::refused(&Refusal::new(codes::STORE_FAILED, why)))
}
