use super::super::launch::{LaunchError, LaunchEvent, failure, rate_limited};
use super::super::manifest::{self, DriverKind, Manifest, Refused};
use crate::adapters::{LINE_BYTES_MAX, clip_text};
use crate::app_plugins::yaml::Value as Yaml;
use crate::launch_context::EngineServer;
use serde_json::{Value, json};
use std::path::Path;

/// The Agent Client Protocol version this client speaks.
pub const PROTOCOL_VERSION: u64 = 1;
/// The JSON-RPC error code an ACP agent answers with when it needs its own sign-in first.
pub const AUTH_REQUIRED: i64 = -32000;
/// The JSON-RPC error code of a method this client does not serve.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// The longest session id or request id this client keeps or echoes.
pub const ID_BYTES_MAX: usize = 256;

/// Where an ACP conversation stands: the handshake, the session being opened, waiting for a
/// prompt, a prompt turn, or failed for good after a refused line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Initializing,
    Opening,
    Idle,
    Prompting,
    Failed,
}

/// The one request of this client the agent has not answered yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    Initialize,
    New,
    Load,
    Resume,
    Prompt,
}

/// One ACP agent conversation over JSON-RPC lines, without any IO: it writes the frames the
/// caller sends to the agent and turns the agent's lines into launch events. The client
/// offers the agent no file or terminal service and no MCP server but the engine's own,
/// answers a permission request from the launch's tools switch, and keeps at most one
/// request of its own open at a time.
#[derive(Debug)]
pub struct Acp {
    phase: Phase,
    cwd: String,
    tools: bool,
    servers: Vec<Value>,
    resume: Option<String>,
    session: Option<String>,
    next_id: u64,
    pending: Option<(u64, Pending)>,
    held: Option<String>,
    cancel_held: bool,
    cancelled: bool,
    breached: bool,
}

impl Acp {
    /// Starts a conversation. Inputs: the absolute working folder the session runs in, the
    /// tools switch that decides permission answers, and the agent session id to continue, if
    /// any. Output: the protocol and its `initialize` frame, or
    /// `launch_working_dir_unavailable` for a folder that is not absolute UTF-8 and
    /// `launch_identity` for a session id that is not 1 to `ID_BYTES_MAX` plain bytes.
    pub fn new(
        cwd: &Path,
        tools: bool,
        resume: Option<&str>,
    ) -> Result<(Self, Vec<u8>), LaunchError> {
        let folder = cwd
            .to_str()
            .filter(|_| cwd.is_absolute())
            .ok_or_else(|| failure("launch_working_dir_unavailable"))?;
        if resume.is_some_and(|id| !plain_id(id)) {
            return Err(failure("launch_identity"));
        }
        let mut acp = Self {
            phase: Phase::Initializing,
            cwd: folder.to_owned(),
            tools,
            servers: Vec::new(),
            resume: resume.map(str::to_owned),
            session: None,
            next_id: 0,
            pending: None,
            held: None,
            cancel_held: false,
            cancelled: false,
            breached: false,
        };
        let start = acp.request(
            Pending::Initialize,
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": {
                    "fs": {"readTextFile": false, "writeTextFile": false},
                    "terminal": false
                }
            }),
        );
        acp.assert_consistent();
        Ok((acp, start))
    }

    /// Gives the session the engine's own MCP servers, which `session/new`, `session/load`
    /// and `session/resume` carry in the ACP stdio form, and none with tools off. Input: the
    /// servers, before the first line is read. Output: the protocol.
    pub fn with_servers(mut self, servers: &[EngineServer]) -> Self {
        if self.tools {
            self.servers = servers
                .iter()
                .map(|server| {
                    json!({
                        "name": server.name,
                        "command": server.command,
                        "args": server.args,
                        "env": []
                    })
                })
                .collect();
        }
        self.assert_consistent();
        self
    }

    /// Where the conversation stands. Output: the phase.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The agent's session id once a session is open. Output: the id, or none before.
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    /// Starts a prompt turn, or holds one prompt until the session is open. Input: the
    /// prompt text, checked by the caller. Output: the `session/prompt` frame, none while
    /// held, or `launch_busy` during a turn or with a prompt already held.
    pub fn prompt(&mut self, text: &str) -> Result<Option<Vec<u8>>, LaunchError> {
        self.assert_consistent();
        let sent = match self.phase {
            Phase::Idle => Some(self.send_prompt(text)),
            Phase::Initializing | Phase::Opening if self.held.is_none() => {
                self.held = Some(text.to_owned());
                None
            }
            _ => return Err(failure("launch_busy")),
        };
        self.assert_consistent();
        Ok(sent)
    }

    /// Asks the agent to stop the running turn. A turn still held is cancelled as soon as it
    /// starts. Output: the `session/cancel` frame, none for a held turn, or
    /// `launch_not_running` with no turn or a cancel already asked.
    pub fn cancel(&mut self) -> Result<Option<Vec<u8>>, LaunchError> {
        self.assert_consistent();
        let sent = match self.phase {
            Phase::Prompting if !self.cancelled => Some(self.send_cancel()),
            Phase::Initializing | Phase::Opening if self.held.is_some() && !self.cancel_held => {
                self.cancel_held = true;
                None
            }
            _ => return Err(failure("launch_not_running")),
        };
        self.assert_consistent();
        Ok(sent)
    }

    /// Reads one line the agent wrote. A refused line fails the conversation for good.
    /// Input: the line, with or without its newline. Output: the events it gives and the
    /// frames to write back, `launch_protocol_failed` for a line that is too long, not one
    /// JSON-RPC 2.0 object or not one this conversation expects, `launch_request_failed` for
    /// a refused handshake, `launch_account_unsupported` for an agent that needs its own
    /// sign-in, or `launch_resume_unsupported` for a resume the agent does not offer.
    pub fn receive(
        &mut self,
        line: &[u8],
    ) -> Result<(Vec<LaunchEvent>, Vec<Vec<u8>>), LaunchError> {
        self.assert_consistent();
        if self.phase == Phase::Failed {
            return Err(failure("launch_protocol_failed"));
        }
        let mut events = Vec::new();
        let mut writes = Vec::new();
        let result = self.decode(line, &mut events, &mut writes);
        if result.is_err() {
            self.phase = Phase::Failed;
            self.cancelled = false;
            self.breached = false;
        }
        self.assert_consistent();
        result.map(|()| (events, writes))
    }

    fn decode(
        &mut self,
        line: &[u8],
        events: &mut Vec<LaunchEvent>,
        writes: &mut Vec<Vec<u8>>,
    ) -> Result<(), LaunchError> {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.len() > LINE_BYTES_MAX {
            return Err(failure("launch_protocol_failed"));
        }
        let value: Value =
            serde_json::from_slice(line).map_err(|_| failure("launch_protocol_failed"))?;
        if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(failure("launch_protocol_failed"));
        }
        let method = value.get("method").and_then(Value::as_str);
        if method.is_none() && value.get("method").is_some() {
            return Err(failure("launch_protocol_failed"));
        }
        let params = value.get("params").unwrap_or(&Value::Null);
        match (method, value.get("id")) {
            (Some(method), Some(id)) => {
                let id = echoed(id)?;
                writes.push(self.answer(&id, method, params, events));
            }
            (Some(method), None) => self.notice(method, params, events, writes),
            (None, Some(id)) => self.response(id, &value, events, writes)?,
            (None, None) => return Err(failure("launch_protocol_failed")),
        }
        Ok(())
    }

    fn answer(
        &self,
        id: &Value,
        method: &str,
        params: &Value,
        events: &mut Vec<LaunchEvent>,
    ) -> Vec<u8> {
        if method != "session/request_permission" {
            return frame(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": METHOD_NOT_FOUND, "message": "Method not found"}
            }));
        }
        let open = self.phase == Phase::Prompting
            && !self.cancelled
            && params.get("sessionId").and_then(Value::as_str) == self.session.as_deref();
        let choice = open
            .then(|| choose(params.get("options"), self.tools))
            .flatten();
        let outcome = match &choice {
            Some((option, _)) => json!({"outcome": "selected", "optionId": option}),
            None => json!({"outcome": "cancelled"}),
        };
        if !choice.is_some_and(|(_, allowed)| allowed) {
            events.push(LaunchEvent::ApprovalDenied);
        }
        frame(&json!({"jsonrpc": "2.0", "id": id, "result": {"outcome": outcome}}))
    }

    fn notice(
        &mut self,
        method: &str,
        params: &Value,
        events: &mut Vec<LaunchEvent>,
        writes: &mut Vec<Vec<u8>>,
    ) {
        if method != "session/update"
            || self.phase != Phase::Prompting
            || params.get("sessionId").and_then(Value::as_str) != self.session.as_deref()
        {
            return;
        }
        let update = params.get("update").unwrap_or(&Value::Null);
        let text = |path: &str| update.pointer(path).and_then(Value::as_str);
        match text("/sessionUpdate") {
            Some("agent_message_chunk") if text("/content/type") == Some("text") => {
                if let Some(chunk) = text("/content/text").filter(|chunk| !chunk.is_empty()) {
                    events.push(LaunchEvent::Text {
                        text: clip_text(chunk),
                    });
                }
            }
            Some(kind @ ("tool_call" | "tool_call_update")) => {
                if kind == "tool_call"
                    && let Some(title) = text("/title").filter(|title| !title.is_empty())
                {
                    events.push(LaunchEvent::ToolUse {
                        name: clip_text(title),
                    });
                }
                let ran = matches!(text("/status"), Some("in_progress" | "completed"));
                if !self.tools && ran && !self.breached {
                    self.breached = true;
                    events.push(LaunchEvent::ApprovalDenied);
                    if !self.cancelled {
                        writes.push(self.send_cancel());
                    }
                }
            }
            _ => {}
        }
    }

    fn response(
        &mut self,
        id: &Value,
        value: &Value,
        events: &mut Vec<LaunchEvent>,
        writes: &mut Vec<Vec<u8>>,
    ) -> Result<(), LaunchError> {
        let (expected, pending) = self
            .pending
            .filter(|(expected, _)| id.as_u64() == Some(*expected))
            .ok_or_else(|| failure("launch_protocol_failed"))?;
        assert!(expected < self.next_id);
        self.pending = None;
        let error = value.get("error");
        let result = value.get("result").unwrap_or(&Value::Null);
        match pending {
            Pending::Initialize => {
                if error.is_some() {
                    return Err(failure("launch_request_failed"));
                }
                if result.get("protocolVersion").and_then(Value::as_u64) != Some(PROTOCOL_VERSION) {
                    return Err(failure("launch_protocol_failed"));
                }
                let offers = |path: &str| {
                    result
                        .pointer(path)
                        .is_some_and(|offer| offer.is_object() || offer == &Value::Bool(true))
                };
                let open = match self.resume.clone() {
                    None => (Pending::New, "session/new", None),
                    Some(id) if offers("/agentCapabilities/sessionCapabilities/resume") => {
                        (Pending::Resume, "session/resume", Some(id))
                    }
                    Some(id) if offers("/agentCapabilities/loadSession") => {
                        (Pending::Load, "session/load", Some(id))
                    }
                    Some(_) => return Err(failure("launch_resume_unsupported")),
                };
                let mut params = json!({"cwd": self.cwd, "mcpServers": self.servers});
                if let Some(id) = open.2 {
                    params["sessionId"] = Value::String(id);
                }
                self.phase = Phase::Opening;
                writes.push(self.request(open.0, open.1, params));
            }
            Pending::New | Pending::Load | Pending::Resume => {
                if let Some(error) = error {
                    let code = error.get("code").and_then(Value::as_i64);
                    return Err(failure(if code == Some(AUTH_REQUIRED) {
                        "launch_account_unsupported"
                    } else {
                        "launch_request_failed"
                    }));
                }
                let session = match pending {
                    Pending::New => result
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .filter(|id| plain_id(id))
                        .ok_or_else(|| failure("launch_protocol_failed"))?
                        .to_owned(),
                    _ => self
                        .resume
                        .clone()
                        .ok_or_else(|| failure("launch_protocol_failed"))?,
                };
                self.session = Some(session.clone());
                self.phase = Phase::Idle;
                events.push(LaunchEvent::Ready {
                    native_session: Some(session),
                });
                if let Some(text) = self.held.take() {
                    writes.push(self.send_prompt(&text));
                    if std::mem::take(&mut self.cancel_held) {
                        writes.push(self.send_cancel());
                    }
                }
            }
            Pending::Prompt => {
                let breached = std::mem::take(&mut self.breached);
                self.phase = Phase::Idle;
                self.cancelled = false;
                if let Some(error) = error {
                    let message = error.get("message").and_then(Value::as_str).unwrap_or("");
                    events.push(LaunchEvent::TurnError {
                        rate_limited: rate_limited(message),
                    });
                    events.push(LaunchEvent::TurnFinished { success: false });
                    return Ok(());
                }
                match result.get("stopReason").and_then(Value::as_str) {
                    Some("end_turn") if !breached => {
                        events.push(LaunchEvent::TurnFinished { success: true })
                    }
                    Some("cancelled") if !breached => {
                        events.push(LaunchEvent::Interrupted);
                        events.push(LaunchEvent::TurnFinished { success: false });
                    }
                    Some(_) => events.push(LaunchEvent::TurnFinished { success: false }),
                    None => return Err(failure("launch_protocol_failed")),
                }
            }
        }
        Ok(())
    }

    fn send_prompt(&mut self, text: &str) -> Vec<u8> {
        assert_eq!(self.phase, Phase::Idle);
        let session = self.session.clone();
        self.phase = Phase::Prompting;
        self.cancelled = false;
        self.breached = false;
        self.request(
            Pending::Prompt,
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": text}]}),
        )
    }

    fn send_cancel(&mut self) -> Vec<u8> {
        assert_eq!(self.phase, Phase::Prompting);
        assert!(!self.cancelled);
        self.cancelled = true;
        frame(&json!({
            "jsonrpc": "2.0",
            "method": "session/cancel",
            "params": {"sessionId": self.session}
        }))
    }

    fn request(&mut self, pending: Pending, method: &str, params: Value) -> Vec<u8> {
        assert!(self.pending.is_none());
        let id = self.next_id;
        self.next_id = id.checked_add(1).expect("request ids never run out");
        self.pending = Some((id, pending));
        frame(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
    }

    fn assert_consistent(&self) {
        assert!(Path::new(&self.cwd).is_absolute());
        assert!(self.tools || self.servers.is_empty());
        assert!(self.resume.as_deref().is_none_or(plain_id));
        assert!(self.session.as_deref().is_none_or(plain_id));
        assert!(self.pending.is_none_or(|(id, _)| id < self.next_id));
        assert!(self.held.is_some() || !self.cancel_held);
        assert!(self.phase == Phase::Prompting || (!self.cancelled && !self.breached));
        let pending = self.pending.map(|(_, pending)| pending);
        match self.phase {
            Phase::Initializing => {
                assert_eq!(pending, Some(Pending::Initialize));
                assert!(self.session.is_none());
            }
            Phase::Opening => {
                assert!(matches!(
                    pending,
                    Some(Pending::New | Pending::Load | Pending::Resume)
                ));
                assert!(self.session.is_none());
            }
            Phase::Idle => {
                assert_eq!(pending, None);
                assert!(self.session.is_some() && self.held.is_none());
            }
            Phase::Prompting => {
                assert_eq!(pending, Some(Pending::Prompt));
                assert!(self.session.is_some() && self.held.is_none());
            }
            Phase::Failed => {}
        }
    }
}

/// Checks the tools-off promise of an ACP manifest. An ACP agent asks permission only when
/// it chooses to, so rejecting its permission requests never switches its tools off; a
/// manifest that does not say `tools_limited` names the agent's own switch, the arguments
/// `piped.tools_off` adds with tools off. Input: a manifest. Output: Ok for another driver,
/// a limited harness or one with a switch, else a refusal that names the setting.
pub fn check(manifest: &Manifest) -> Result<(), Refused> {
    if manifest.piped != Some(DriverKind::Acp) || manifest.tools_limited.is_some() {
        return Ok(());
    }
    let switch = manifest
        .block("piped")
        .and_then(|piped| piped.get("tools_off"));
    match switch {
        Some(Yaml::List(items))
            if !items.is_empty()
                && items
                    .iter()
                    .all(|item| matches!(item, Yaml::Str(text) if !text.is_empty())) =>
        {
            Ok(())
        }
        _ => Err(Refused {
            code: manifest::MISSING,
            message: "piped.tools_off: an acp harness switches tools off only by a setting of its own; name it here, or say tools_limited.".into(),
        }),
    }
}

/// The command line of an ACP agent process: `piped.argv`, with `piped.tools_off` before its
/// first `--` entry while tools are off, each `{home}`, `{cwd}` and `{model}` filled once.
/// Inputs: the manifest, the agent home, the working folder, the model, if any, and the
/// tools switch. Output: the arguments, `launch_model_unsupported` for `{model}` with no
/// model, `launch_account_home_unavailable` or `launch_working_dir_unavailable` for a folder
/// that is not UTF-8, or `launch_harness_unsupported` for a manifest that does not read as
/// an ACP harness or names any other placeholder.
pub fn arguments(
    manifest: &Manifest,
    home: &Path,
    cwd: &Path,
    model: Option<&str>,
    tools: bool,
) -> Result<Vec<String>, LaunchError> {
    let unsupported = |_: Refused| failure("launch_harness_unsupported");
    let piped = manifest
        .block("piped")
        .filter(|_| manifest.piped == Some(DriverKind::Acp))
        .ok_or_else(|| failure("launch_harness_unsupported"))?;
    let argv = super::jsonl_turn::arguments(piped, "argv")
        .map_err(unsupported)?
        .unwrap_or_default();
    let off = if tools {
        Vec::new()
    } else {
        super::jsonl_turn::arguments(piped, "tools_off")
            .map_err(unsupported)?
            .unwrap_or_default()
    };
    let at = super::positional(&argv, &[], &off).map_err(unsupported)?;
    let (home, cwd) = (home.to_str(), cwd.to_str());
    let value = |name: &str| match name {
        "home" => home.ok_or_else(|| failure("launch_account_home_unavailable")),
        "cwd" => cwd.ok_or_else(|| failure("launch_working_dir_unavailable")),
        "model" => model.ok_or_else(|| failure("launch_model_unsupported")),
        _ => Err(failure("launch_harness_unsupported")),
    };
    let filled = super::ordered(&argv, at, &[], &off)
        .map(|argument| super::fill(argument, &value))
        .collect::<Result<Vec<String>, LaunchError>>()?;
    assert_eq!(filled.len(), argv.len() + off.len());
    Ok(filled)
}

/// Picks the permission option the launch's tools switch allows: with tools on the
/// `allow_once` option, else a reject option, once before always. An `allow_always` option is
/// never picked, so the agent remembers no allow for a later run on the same home. Inputs:
/// the options the agent sent and the tools switch. Output: the option id and whether it
/// allows, or none when no option fits.
fn choose(options: Option<&Value>, tools: bool) -> Option<(String, bool)> {
    let options = options?.as_array()?;
    let kinds: &[&str] = if tools {
        &["allow_once", "reject_once", "reject_always"]
    } else {
        &["reject_once", "reject_always"]
    };
    kinds.iter().find_map(|kind| {
        options.iter().find_map(|option| {
            let id = option
                .get("optionId")
                .and_then(Value::as_str)
                .filter(|id| plain_id(id))?;
            (option.get("kind").and_then(Value::as_str) == Some(*kind))
                .then(|| (id.to_owned(), kind.starts_with("allow")))
        })
    })
}

/// Checks a request id the agent sent and gives it back to echo: a whole number, or text
/// of 1 to `ID_BYTES_MAX` bytes with no control character. Input: the id. Output: the same
/// id, or `launch_protocol_failed`.
fn echoed(id: &Value) -> Result<Value, LaunchError> {
    let plain = id.is_i64() || id.is_u64() || id.as_str().is_some_and(plain_id);
    if plain {
        Ok(id.clone())
    } else {
        Err(failure("launch_protocol_failed"))
    }
}

fn plain_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= ID_BYTES_MAX && !id.chars().any(char::is_control)
}

fn frame(value: &Value) -> Vec<u8> {
    let mut bytes = value.to_string().into_bytes();
    bytes.push(b'\n');
    bytes
}

#[cfg(test)]
#[path = "acp_tests.rs"]
mod tests;
