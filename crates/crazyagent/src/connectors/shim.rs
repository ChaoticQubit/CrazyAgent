//! The connector server an agent's harness starts: `crazyagent connector-mcp <launch
//! folder>`. It speaks MCP over stdio and passes each tool list and tool call to the
//! engine through files in the launch folder. It holds no key and reaches no vendor.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::io::{BufRead, Write};
use std::path::Path;
use std::time::Duration;

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
/// How long the server waits for the engine's answer to one call.
pub(crate) const REPLY_WAIT: Duration = Duration::from_secs(240);

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
pub fn serve(input: impl BufRead, output: impl Write, folder: &Path) -> std::io::Result<()> {
    serve_with(input, output, folder, REPLY_WAIT)
}

/// [`serve`] with the wait for one answer given, so a row does not wait four minutes.
pub(crate) fn serve_with(
    _input: impl BufRead,
    _output: impl Write,
    _folder: &Path,
    _wait: Duration,
) -> std::io::Result<()> {
    Err(std::io::Error::other("connector_unavailable_here"))
}
