//! The connector server an agent's harness starts: `crazyagent connector-mcp <launch
//! folder>`. It speaks MCP over stdio and passes each tool list and tool call to the
//! engine through files in the launch folder. It holds no key and reaches no vendor.

use crate::team::tool::{Mailbox, call_id, failure, rpc, tool_result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The engine subcommand that runs this server.
pub const SERVER_COMMAND: &str = "connector-mcp";
/// The name the server gives itself and the name of its entry in a harness's config.
pub const SERVER_NAME: &str = "ca-connectors";
/// The folder inside a launch folder that holds the calls and their answers.
pub const MAILBOX: &str = "calls";
/// The largest call file the engine reads.
pub const ASK_BYTES_MAX: u64 = 96 * 1024;
/// The largest answer file the server reads.
pub const ANSWER_BYTES_MAX: u64 = 6 * 1024 * 1024;
/// The most calls one launch may have waiting for the engine.
pub const PENDING_MAX: usize = 16;
/// How long the server waits for the engine's answer to one call: four vendor requests
/// at curl's longest wait, the most a call that opens a session takes when nothing queues.
pub(crate) const REPLY_WAIT: Duration = Duration::from_secs(240);
/// How long the server waits for a call the engine holds for the owner's answer: past the
/// longest approval wait, 30 minutes, so the owner's answer or its end always comes first.
pub(crate) const HELD_WAIT: Duration = Duration::from_secs(35 * 60);

pub(crate) const PART_SUFFIX: &str = ".part";
pub(crate) const CALL_SUFFIX: &str = ".call";
pub(crate) const TAKEN_SUFFIX: &str = ".taken";
pub(crate) const ANSWER_PART_SUFFIX: &str = ".reply-part";
pub(crate) const REPLY_SUFFIX: &str = ".reply";
pub(crate) const HELD_SUFFIX: &str = ".held";

const POLL: Duration = Duration::from_millis(25);
const NAME_BYTES_MAX: usize = 256;
const PROTOCOL_VERSIONS: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
const BUSY: &str = "connector_busy: too many connector calls are waiting; try again shortly.";
const UNSENT: &str = "connector_gateway_unavailable: the call could not be handed to the engine.";
const TOO_LARGE: &str = "connector_request_too_large: the call's arguments are too large.";
const NOT_TAKEN: &str = "connector_gateway_unavailable: the engine did not take the call in time; nothing was sent to the vendor.";
const NOT_ANSWERED: &str = "connector_gateway_unavailable: the engine took the call and did not answer in time; the vendor may still have run it.";
const UNREAD: &str = "connector_gateway_unavailable: the engine's answer could not be read.";

const _: () = assert!(REPLY_WAIT.as_secs() >= 4 * 60);
const _: () = assert!(HELD_WAIT.as_secs() > 30 * 60);
const _: () = assert!(HELD_WAIT.as_secs() > REPLY_WAIT.as_secs());
const _: () = assert!(ASK_BYTES_MAX < ANSWER_BYTES_MAX);

/// What the server asks the engine: the tools this launch may use, or one tool call by
/// its shown name with its arguments. Nothing else of an agent's request is passed on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Ask {
    List,
    Call {
        tool: String,
        arguments: Map<String, Value>,
    },
}

/// What the engine answers: the tools as MCP tool entries, or one result as text with
/// whether it is an error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Answer {
    Tools { tools: Vec<Value> },
    Result { text: String, error: bool },
}

/// Serves the connector tools over stdio until the client closes input. Inputs: the
/// client's input and output and the absolute launch folder the engine made. Output: an
/// input or output failure, or a refusal of a launch folder that is not a plain folder.
pub fn serve(input: impl BufRead, output: impl Write + Send, folder: &Path) -> std::io::Result<()> {
    serve_with(input, output, folder, REPLY_WAIT)
}

/// [`serve`] with the wait for one answer given, so a row does not wait four minutes.
/// Each tool call is answered on a thread of its own, at most `PENDING_MAX` at once, so a
/// write the engine holds for the owner never holds up a later call; every other request
/// is answered in turn, and each reply is one whole line.
pub(crate) fn serve_with(
    input: impl BufRead,
    output: impl Write + Send,
    folder: &Path,
    wait: Duration,
) -> std::io::Result<()> {
    let plain = std::fs::symlink_metadata(folder).is_ok_and(|metadata| metadata.is_dir());
    if !folder.is_absolute() || !plain {
        return Err(std::io::Error::other(
            "the launch folder must be an absolute plain folder",
        ));
    }
    let mailbox = Mailbox::open(&folder.join(MAILBOX))?;
    let output = Mutex::new(output);
    let running = AtomicUsize::new(0);
    let failed: Mutex<Option<std::io::Error>> = Mutex::new(None);
    let shared = (&mailbox, &output, &running, &failed);
    std::thread::scope(|scope| {
        rpc(input, Shared(&output), move |request| {
            let (mailbox, output, running, failed) = shared;
            if !is_call(request) {
                return answer(request, mailbox, wait);
            }
            if running.load(Ordering::SeqCst) >= PENDING_MAX {
                return busy(request);
            }
            running.fetch_add(1, Ordering::SeqCst);
            let request = request.clone();
            scope.spawn(move || {
                let reply = answer(&request, mailbox, wait);
                if let Some(reply) = reply
                    && let Err(problem) = send(output, &reply)
                {
                    locked(failed).get_or_insert(problem);
                }
                running.fetch_sub(1, Ordering::SeqCst);
            });
            None
        })
    })?;
    assert_eq!(running.load(Ordering::SeqCst), 0);
    match failed.into_inner().unwrap_or_else(PoisonError::into_inner) {
        Some(problem) => Err(problem),
        None => Ok(()),
    }
}

/// The server's output written through its lock one write at a time, so a reply line
/// from a call answered on its own thread never lands inside another.
struct Shared<'a, W>(&'a Mutex<W>);

impl<W: Write> Write for Shared<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        locked(self.0).write(bytes)
    }

    fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        locked(self.0).write_all(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        locked(self.0).flush()
    }
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn is_call(request: &Value) -> bool {
    request.get("id").is_some()
        && request.get("method").and_then(Value::as_str) == Some("tools/call")
}

fn busy(request: &Value) -> Option<Value> {
    let id = request.get("id")?.clone();
    let result = tool_result(BUSY, true);
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// Writes one reply as one whole line under the output's lock. Inputs: the output and the
/// reply. Output: the write failure, if any.
fn send(output: &Mutex<impl Write>, message: &Value) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    line.push(b'\n');
    let mut output = locked(output);
    output.write_all(&line)?;
    output.flush()
}

fn initialized(params: Option<&Value>) -> Value {
    let asked = params
        .and_then(|params| params.get("protocolVersion"))
        .and_then(Value::as_str);
    let version = asked
        .filter(|asked| PROTOCOL_VERSIONS.contains(asked))
        .unwrap_or(PROTOCOL_VERSIONS[PROTOCOL_VERSIONS.len() - 1]);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
    })
}

/// Answers one MCP request: the handshake and ping here, a tool list or call through the
/// engine. Only a call's tool name and its arguments object go to the engine. Inputs: the
/// request, the mailbox and the wait. Output: the reply, none for a notice.
fn answer(request: &Value, mailbox: &Mailbox, wait: Duration) -> Option<Value> {
    let id = request.get("id")?.clone();
    let params = request.get("params");
    let result = match request.get("method").and_then(Value::as_str) {
        Some("initialize") => initialized(params),
        Some("ping") => json!({}),
        Some("tools/list") => match ask(mailbox, &Ask::List, wait) {
            Ok(Answer::Tools { tools }) => json!({"tools": tools}),
            Ok(Answer::Result { text, .. }) | Err(text) => {
                return Some(failure(id, -32603, &text));
            }
        },
        Some("tools/call") => {
            let name = params
                .and_then(|params| params.get("name"))
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty() && name.len() <= NAME_BYTES_MAX);
            let arguments = match params.and_then(|params| params.get("arguments")) {
                None | Some(Value::Null) => Some(Map::new()),
                Some(Value::Object(fields)) => Some(fields.clone()),
                Some(_) => None,
            };
            let (Some(name), Some(arguments)) = (name, arguments) else {
                return Some(failure(id, -32602, "Invalid params"));
            };
            let call = Ask::Call {
                tool: name.to_owned(),
                arguments,
            };
            match ask(mailbox, &call, wait) {
                Ok(Answer::Result { text, error }) => tool_result(&text, error),
                Ok(Answer::Tools { .. }) => tool_result(UNREAD, true),
                Err(text) => tool_result(&text, true),
            }
        }
        _ => return Some(failure(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// Hands one ask to the engine and waits for its answer: the ask is written under a part
/// name and renamed into place, so the engine never reads half of one. While the engine
/// marks the ask held for the owner, the wait is `HELD_WAIT` from its start. An ask still
/// waiting at the deadline is withdrawn, so the engine never runs it. Inputs: the
/// mailbox, the ask and the wait. Output: the answer, or the text the agent reads.
fn ask(mailbox: &Mailbox, ask: &Ask, wait: Duration) -> Result<Answer, String> {
    if mailbox.entries(CALL_SUFFIX).len() >= PENDING_MAX {
        return Err(BUSY.into());
    }
    let id = call_id().map_err(|_| UNSENT.to_owned())?;
    let mut bytes = serde_json::to_vec(ask).map_err(|_| UNSENT.to_owned())?;
    bytes.push(b'\n');
    if bytes.len() as u64 > ASK_BYTES_MAX {
        return Err(TOO_LARGE.into());
    }
    let part = format!("{id}{PART_SUFFIX}");
    let call = format!("{id}{CALL_SUFFIX}");
    let written = mailbox
        .create(&part)
        .and_then(|mut file| file.write_all(&bytes))
        .and_then(|()| mailbox.rename(&part, &call));
    if written.is_err() {
        let _ = mailbox.remove(&part);
        return Err(UNSENT.into());
    }
    let reply = format!("{id}{REPLY_SUFFIX}");
    let held = format!("{id}{HELD_SUFFIX}");
    let start = Instant::now();
    loop {
        if let Some(found) = read_answer(mailbox, &reply) {
            let _ = mailbox.remove(&reply);
            return found;
        }
        let limit = if mailbox.read(&held).is_ok() {
            HELD_WAIT.max(wait)
        } else {
            wait
        };
        if start.elapsed() >= limit {
            break;
        }
        std::thread::sleep(POLL);
    }
    if mailbox.remove(&call).is_ok() {
        return Err(NOT_TAKEN.into());
    }
    Err(NOT_ANSWERED.into())
}

/// Reads the engine's answer when it is there. Inputs: the mailbox and the answer's name.
/// Output: none while there is no answer, else the answer or the unreadable text.
fn read_answer(mailbox: &Mailbox, name: &str) -> Option<Result<Answer, String>> {
    let file = match mailbox.read(name) {
        Ok(file) => file,
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(Err(UNREAD.into())),
    };
    let fits = file
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() <= ANSWER_BYTES_MAX);
    if !fits {
        return Some(Err(UNREAD.into()));
    }
    let mut bytes = Vec::new();
    if (&file)
        .take(ANSWER_BYTES_MAX)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Some(Err(UNREAD.into()));
    }
    Some(serde_json::from_slice::<Answer>(&bytes).map_err(|_| UNREAD.to_owned()))
}
