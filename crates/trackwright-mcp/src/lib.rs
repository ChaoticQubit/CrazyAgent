use rusqlite::{Connection, Transaction, TransactionBehavior};
use serde_json::{Map, Value, json};
use std::io::{BufRead, ErrorKind, Write};
use trackwright_core::catalog::{OpSpec, catalog};
use trackwright_core::dispatch;
use trackwright_core::host::Host;
use trackwright_core::model::{Actor, REQUEST_BYTES_MAX, Refusal, codes};
use trackwright_core::reply::Reply;
use trackwright_core::requests::Request;

/// The server name an MCP client sees in the `initialize` reply.
pub const SERVER_NAME: &str = "trackwright";

/// The longest JSON-RPC line the server reads, newline excluded: one request at the core's
/// 16 KiB bound plus its JSON-RPC wrapping and escapes. A longer line is answered with
/// `Request too large`, skipped to its newline, and serving goes on.
pub const LINE_BYTES_MAX: usize = 2 * REQUEST_BYTES_MAX;

/// The MCP protocol versions the server speaks, oldest first. A client that asks for any
/// other version is offered the latest, which it may accept or close on.
pub const PROTOCOL_VERSIONS: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

const PROTOCOL_LATEST: &str = PROTOCOL_VERSIONS[PROTOCOL_VERSIONS.len() - 1];
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

const _: () = assert!(LINE_BYTES_MAX > REQUEST_BYTES_MAX);
const _: () = assert!(REQUEST_BYTES_MAX == 16 * 1024);

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

/// Serves the catalog as an MCP server over stdio JSON-RPC, one message per line, until the
/// client closes its input. Inputs: the client's input and output, and the session every
/// call runs in. Output: Ok at the end of input, or the input or output failure, or the
/// failure of a database that cannot end a call's transaction; every request, refused ones
/// included, gets its reply, and notifications get none.
pub fn serve<H: Host>(
    mut input: impl BufRead,
    mut output: impl Write,
    session: &Session<H>,
) -> std::io::Result<()> {
    let mut line = Vec::with_capacity(LINE_BYTES_MAX + 1);
    loop {
        let answered = match read_line(&mut input, &mut line)? {
            Line::End => return Ok(()),
            Line::TooLong => Some(failure(Value::Null, INVALID_REQUEST, "Request too large")),
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
    TooLong,
    End,
}

fn read_line(input: &mut impl BufRead, line: &mut Vec<u8>) -> std::io::Result<Line> {
    line.clear();
    let read = input
        .by_ref()
        .take(LINE_BYTES_MAX as u64 + 1)
        .read_until(b'\n', line)?;
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
    skip_line(input)?;
    Ok(Line::TooLong)
}

fn skip_line(input: &mut impl BufRead) -> std::io::Result<()> {
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
                input.consume(end + 1);
                return Ok(());
            }
            None => {
                let length = buffer.len();
                input.consume(length);
            }
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
    let Some(message) = message.as_object() else {
        let why = "Invalid Request: one JSON-RPC object per line";
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
    let schema = spec.input_schema();
    assert_eq!(schema["type"], "object", "an MCP tool's input is an object");
    assert!(!spec.summary.is_empty(), "every op says what it does");
    json!({"name": spec.op.as_str(), "description": spec.summary, "inputSchema": schema})
}

type Called = Result<Value, &'static str>;

fn call<H: Host>(params: Option<&Value>, session: &Session<H>) -> std::io::Result<Called> {
    let Some(params) = params.and_then(Value::as_object) else {
        return Ok(Err("tools/call takes an object with the tool's name"));
    };
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Ok(Err("tools/call takes the tool's name"));
    };
    let Some(spec) = catalog().iter().find(|spec| spec.op.as_str() == name) else {
        return Ok(Err("Unknown tool"));
    };
    let empty = Value::Object(Map::new());
    let arguments = params.get("arguments").unwrap_or(&empty);
    if !arguments.is_object() {
        return Ok(Err("the tool's arguments are an object"));
    }
    let input = serde_json::to_vec(arguments).expect("a parsed JSON value serializes");
    let reply = run(session, spec, &input)?;
    let text = reply.to_json();
    assert!(text.starts_with('{'), "the envelope is a JSON object");
    let result = json!({"content": [{"type": "text", "text": text}], "isError": !reply.is_ok()});
    Ok(Ok(result))
}

fn run<H: Host>(session: &Session<H>, spec: &OpSpec, input: &[u8]) -> std::io::Result<Reply> {
    let request = match Request::parse(spec.op.as_str(), input) {
        Ok(request) => request,
        Err(refusal) => return Ok(Reply::from(refusal)),
    };
    assert!(
        session.connection.is_autocommit(),
        "no call starts inside another"
    );
    let behavior = if spec.op.is_write() {
        TransactionBehavior::Immediate
    } else {
        TransactionBehavior::Deferred
    };
    let Ok(tx) = Transaction::new_unchecked(&session.connection, behavior) else {
        let why = "the tracker database is busy or unreadable; nothing was written";
        return Ok(Reply::from(Refusal::new(codes::STORE_FAILED, why)));
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
    Ok(Reply::from(Refusal::new(codes::STORE_FAILED, why)))
}
