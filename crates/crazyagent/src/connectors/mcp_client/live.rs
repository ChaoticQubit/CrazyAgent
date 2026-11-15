use super::clean::{self, Mirror};
use super::guard::{self, Target};
use super::wire::{self, Body, Reply, Request, Verb};
use super::{
    Access, Credential, LEGACY_VERSIONS, MODERN_VERSION, McpError, REQUEST_BYTES_MAX, TOOLS_MAX,
    ToolResult, VendorTool,
};
use crate::app_plugins::connector::McpEndpoint;
use crate::connectors::settings::SizeCap;
use crate::provider_reach::ProviderReach;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The most pages of one tool list.
const PAGES_MAX: usize = 64;
const CURSOR_BYTES_MAX: usize = 4096;
const SESSION_BYTES_MAX: usize = 512;
const CLIENT: &str = "CrazyAgent";
const DISCOVER: &str = "server/discover";
const VERSION_UNSUPPORTED: i64 = -32022;
const HEADER_MISMATCH: i64 = -32020;
const CAPABILITY_MISSING: i64 = -32021;
const NOT_FOUND: u16 = 404;

fn refused(code: &'static str) -> McpError {
    McpError::Refused(code)
}

fn invalid() -> McpError {
    refused("connector_reply_invalid")
}

/// Which protocol era a vendor's server answered in: metadata on every request and no
/// session, or the `initialize` handshake with an optional session id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Era {
    Modern,
    Legacy,
}

/// How one request names its protocol: per-request metadata and headers, nothing yet
/// (the handshake's first message), or the negotiated version with the session id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    Modern,
    Hello,
    Legacy,
}

/// One JSON-RPC message to send: how it names its protocol, its method and parameters,
/// the tool name and mirrored headers of a call, and whether it is a notice with no reply.
struct Ask<'a> {
    style: Style,
    method: &'a str,
    params: Map<String, Value>,
    name: Option<&'a str>,
    mirrored: &'a [(String, String)],
    notice: bool,
}

impl<'a> Ask<'a> {
    fn request(style: Style, method: &'a str, params: Map<String, Value>) -> Self {
        Self {
            style,
            method,
            params,
            name: None,
            mirrored: &[],
            notice: false,
        }
    }
}

/// A session's state: the engine root, the checked address, where the key is saved, the
/// size setting, the name lookup, the server's era with its version and session id, the
/// tools of the last list with their header marks, and the next request id.
pub(super) struct Live {
    root: PathBuf,
    target: Target,
    auth: Credential,
    cap: SizeCap,
    reach: ProviderReach,
    era: Era,
    version: String,
    session: Option<String>,
    known: BTreeMap<String, Vec<Mirror>>,
    listed: bool,
    left_out: usize,
    next_id: u64,
    #[cfg(test)]
    vet: bool,
}

fn client() -> Value {
    json!({"name": CLIENT, "version": env!("CARGO_PKG_VERSION")})
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(fields) => fields,
        _ => Map::new(),
    }
}

/// Reads the result out of a reply to a request. A JSON-RPC error is the vendor's own
/// error whatever the status; any other reply outside 2xx is named by its status; a 2xx
/// reply must hold a result object under the request's id. Inputs: the reply and the id.
/// Output: the result, the vendor's error, or `connector_reply_invalid`.
fn result(reply: Reply, id: u64) -> Result<Value, McpError> {
    let status = reply.status;
    let named = || McpError::Vendor(format!("HTTP {status}"));
    match reply.body {
        Body::Message(message) if message.get("error").is_some_and(Value::is_object) => {
            let text = message["error"]["message"].as_str().unwrap_or("");
            let text = clean::vendor_text(text);
            Err(if text.is_empty() {
                named()
            } else {
                McpError::Vendor(text)
            })
        }
        _ if !(200..300).contains(&status) => Err(named()),
        Body::Message(mut message) => {
            if message.get("id") != Some(&json!(id)) {
                return Err(invalid());
            }
            match message.get_mut("result").map(Value::take) {
                Some(result) if result.is_object() => Ok(result),
                _ => Err(invalid()),
            }
        }
        Body::Empty | Body::Unreadable => Err(invalid()),
    }
}

fn versions(value: Option<&Value>) -> Vec<&str> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn era_of(supported: &[&str]) -> Result<Era, McpError> {
    if supported.contains(&MODERN_VERSION) {
        Ok(Era::Modern)
    } else if supported
        .iter()
        .any(|version| LEGACY_VERSIONS.contains(version))
    {
        Ok(Era::Legacy)
    } else {
        Err(refused("connector_protocol_unsupported"))
    }
}

impl Live {
    /// Opens a session: checks the address, asks the server which era it speaks with one
    /// per-request `server/discover`, and runs the handshake for an older server. Inputs:
    /// as [`super::McpSession::open`], plus the name lookup. Output: the session state.
    pub(super) fn open(
        root: &Path,
        endpoint: &McpEndpoint,
        auth: &Credential,
        access: Access,
        cap: SizeCap,
        reach: ProviderReach,
    ) -> Result<Self, McpError> {
        Self::new(root, endpoint, auth, access, cap, reach)?.begin()
    }

    /// [`Self::open`] for rows that count lookups: the address of the stand-in on this
    /// machine is looked up and vetted before every request like a vendor's name, and
    /// the result is still never handed to curl.
    #[cfg(test)]
    pub(super) fn open_vetting(
        root: &Path,
        endpoint: &McpEndpoint,
        auth: &Credential,
        access: Access,
        cap: SizeCap,
        reach: ProviderReach,
    ) -> Result<Self, McpError> {
        let mut live = Self::new(root, endpoint, auth, access, cap, reach)?;
        live.vet = true;
        live.begin()
    }

    fn new(
        root: &Path,
        endpoint: &McpEndpoint,
        auth: &Credential,
        access: Access,
        cap: SizeCap,
        reach: ProviderReach,
    ) -> Result<Self, McpError> {
        let target = guard::target(endpoint, access)?;
        Ok(Self {
            root: root.to_path_buf(),
            target,
            auth: auth.clone(),
            cap,
            reach,
            era: Era::Modern,
            version: String::new(),
            session: None,
            known: BTreeMap::new(),
            listed: false,
            left_out: 0,
            next_id: 1,
            #[cfg(test)]
            vet: false,
        })
    }

    fn begin(mut self) -> Result<Self, McpError> {
        self.era = self.probe()?;
        if self.era == Era::Legacy {
            self.hello()?;
        }
        Ok(self)
    }

    pub(super) fn left_out(&self) -> usize {
        self.left_out
    }

    fn pins(&self) -> Result<Option<String>, McpError> {
        let address = || refused("connector_address_refused");
        let host = self.target.url.host_str().ok_or_else(address)?;
        let port = self
            .target
            .url
            .port_or_known_default()
            .ok_or_else(address)?;
        if self.target.loopback {
            #[cfg(test)]
            if self.vet {
                guard::pins(host, port, &self.reach)?;
            }
            return Ok(None);
        }
        guard::pins(host, port, &self.reach).map(Some)
    }

    /// Sends one JSON-RPC message. The body is checked against `REQUEST_BYTES_MAX`, then
    /// the vendor's name is looked up and vetted again, so each request is pinned to
    /// addresses checked for it. Input: the message. Output: the reply and the id it
    /// went under.
    fn post(&mut self, ask: &Ask<'_>) -> Result<(Reply, u64), McpError> {
        let id = self.next_id;
        self.next_id += 1;
        let mut params = ask.params.clone();
        let mut headers: Vec<(String, String)> = Vec::with_capacity(4 + ask.mirrored.len());
        match ask.style {
            Style::Modern => {
                params.insert(
                    "_meta".to_owned(),
                    json!({
                        "io.modelcontextprotocol/protocolVersion": MODERN_VERSION,
                        "io.modelcontextprotocol/clientInfo": client(),
                        "io.modelcontextprotocol/clientCapabilities": {},
                    }),
                );
                headers.push(("MCP-Protocol-Version".to_owned(), MODERN_VERSION.to_owned()));
                headers.push(("Mcp-Method".to_owned(), ask.method.to_owned()));
                if let Some(name) = ask.name {
                    headers.push(("Mcp-Name".to_owned(), clean::header_value(name)));
                }
                headers.extend(ask.mirrored.iter().cloned());
            }
            Style::Hello => {}
            Style::Legacy => {
                headers.push(("MCP-Protocol-Version".to_owned(), self.version.clone()));
                if let Some(session) = &self.session {
                    headers.push(("Mcp-Session-Id".to_owned(), session.clone()));
                }
            }
        }
        if let Some(header) = &self.target.header {
            headers.push(header.clone());
        }
        let mut message = Map::new();
        message.insert("jsonrpc".to_owned(), json!("2.0"));
        if !ask.notice {
            message.insert("id".to_owned(), json!(id));
        }
        message.insert("method".to_owned(), json!(ask.method));
        if !params.is_empty() {
            message.insert("params".to_owned(), Value::Object(params));
        }
        let body = serde_json::to_vec(&Value::Object(message))
            .map_err(|_| refused("connector_call_failed"))?;
        if body.len() > REQUEST_BYTES_MAX {
            return Err(refused("connector_request_too_large"));
        }
        let pins = self.pins()?;
        let reply = wire::send(
            &self.root,
            &Request {
                url: &self.target.url,
                verb: Verb::Post(&body),
                pins: pins.as_deref(),
                headers: &headers,
                auth: &self.auth,
                wait: (!ask.notice).then(|| json!(id)),
            },
        )?;
        Ok((reply, id))
    }

    /// Finds the server's era. A result that lists the per-request version, or an older
    /// one, decides it; an unsupported-version error decides it from the versions it
    /// lists; any other error or 4xx reply marks a server of the handshake era, as the
    /// protocol's own compatibility rule says. A refused key never falls back.
    fn probe(&mut self) -> Result<Era, McpError> {
        let (reply, id) = self.post(&Ask::request(Style::Modern, DISCOVER, Map::new()))?;
        let status = reply.status;
        if let Body::Message(message) = &reply.body
            && let Some(error) = message.get("error").filter(|error| error.is_object())
        {
            return match error.get("code").and_then(Value::as_i64) {
                Some(VERSION_UNSUPPORTED) => {
                    let data = error.get("data");
                    era_of(&versions(data.and_then(|data| data.get("supported"))))
                }
                Some(HEADER_MISMATCH | CAPABILITY_MISSING) => {
                    Err(refused("connector_protocol_unsupported"))
                }
                _ => Ok(Era::Legacy),
            };
        }
        if (400..500).contains(&status) {
            return Ok(Era::Legacy);
        }
        let found = result(reply, id)?;
        let supported = found
            .get("supportedVersions")
            .filter(|value| value.is_array())
            .ok_or_else(invalid)?;
        era_of(&versions(Some(supported)))
    }

    /// Runs the handshake of an older server: `initialize` with the newest version this
    /// client speaks, the server's version checked against the ones it speaks, the session
    /// id taken only when it is 1 to 512 visible ASCII bytes, then the `initialized` notice.
    fn hello(&mut self) -> Result<(), McpError> {
        let params = object(json!({
            "protocolVersion": LEGACY_VERSIONS[0],
            "capabilities": {},
            "clientInfo": client(),
        }));
        let (reply, id) = self.post(&Ask::request(Style::Hello, "initialize", params))?;
        let session = reply.session.clone();
        let found = result(reply, id)?;
        let version = found
            .get("protocolVersion")
            .and_then(Value::as_str)
            .filter(|version| LEGACY_VERSIONS.contains(version))
            .ok_or_else(|| refused("connector_protocol_unsupported"))?;
        if let Some(session) = &session {
            let visible = session.bytes().all(|byte| (0x21..=0x7e).contains(&byte));
            if session.is_empty() || session.len() > SESSION_BYTES_MAX || !visible {
                return Err(invalid());
            }
        }
        self.version = version.to_owned();
        self.session = session;
        self.era = Era::Legacy;
        let notice = Ask {
            notice: true,
            ..Ask::request(Style::Legacy, "notifications/initialized", Map::new())
        };
        let (reply, _) = self.post(&notice)?;
        if !(200..300).contains(&reply.status) {
            return Err(McpError::Vendor(format!("HTTP {}", reply.status)));
        }
        Ok(())
    }

    /// Sends one request in the session's era and reads its result. An older server that
    /// answers 404 to a session id has ended that session: the handshake runs again and
    /// the request is sent once more, never a third time.
    fn ask(
        &mut self,
        method: &str,
        params: Map<String, Value>,
        name: Option<&str>,
        mirrored: &[(String, String)],
    ) -> Result<Value, McpError> {
        let style = match self.era {
            Era::Modern => Style::Modern,
            Era::Legacy => Style::Legacy,
        };
        let ask = Ask {
            name,
            mirrored,
            ..Ask::request(style, method, params)
        };
        let had_session = self.session.is_some();
        let (reply, id) = self.post(&ask)?;
        if self.era == Era::Legacy && had_session && reply.status == NOT_FOUND {
            self.session = None;
            self.hello()?;
            let (reply, id) = self.post(&ask)?;
            return result(reply, id);
        }
        result(reply, id)
    }

    /// Lists the vendor's tools page by page. A tool that cannot be shown or called
    /// safely is left out and counted; a name listed twice, a cursor sent before, more
    /// than `TOOLS_MAX` entries or `PAGES_MAX` pages refuse the whole list.
    pub(super) fn list_tools(&mut self) -> Result<Vec<VendorTool>, McpError> {
        let modern = self.era == Era::Modern;
        let mut tools = Vec::new();
        let mut known = BTreeMap::new();
        let mut left_out = 0;
        let mut entries = 0;
        let mut sent: Vec<String> = Vec::new();
        loop {
            let mut params = Map::new();
            if let Some(cursor) = sent.last() {
                params.insert("cursor".to_owned(), json!(cursor));
            }
            let found = self.ask("tools/list", params, None, &[])?;
            let page = found
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(invalid)?;
            entries += page.len();
            if entries > TOOLS_MAX {
                return Err(refused("connector_tools_limit"));
            }
            for raw in page {
                let Some((tool, mirrors)) = clean::tool(raw, modern) else {
                    left_out += 1;
                    continue;
                };
                if known.insert(tool.name.clone(), mirrors).is_some() {
                    return Err(invalid());
                }
                tools.push(tool);
            }
            match found.get("nextCursor") {
                None | Some(Value::Null) => break,
                Some(Value::String(next)) => {
                    if next.len() > CURSOR_BYTES_MAX || sent.contains(next) {
                        return Err(invalid());
                    }
                    if sent.len() + 1 >= PAGES_MAX {
                        return Err(refused("connector_tools_limit"));
                    }
                    sent.push(next.clone());
                }
                Some(_) => return Err(invalid()),
            }
        }
        assert!(tools.len() <= TOOLS_MAX && tools.len() == known.len());
        self.known = known;
        self.listed = true;
        self.left_out = left_out;
        Ok(tools)
    }

    /// Calls one tool. The name must be callable and the arguments an object; at a server
    /// of the per-request era the tool must be in the last list, which is read first when
    /// there is none, and its marked arguments are mirrored into headers.
    pub(super) fn call(&mut self, name: &str, args: &Value) -> Result<ToolResult, McpError> {
        if !clean::valid_name(name) {
            return Err(refused("connector_tool_name_invalid"));
        }
        let Value::Object(arguments) = args else {
            return Err(refused("connector_arguments_invalid"));
        };
        let mut mirrored = Vec::new();
        if self.era == Era::Modern {
            if !self.listed {
                self.list_tools()?;
            }
            let marks = self
                .known
                .get(name)
                .ok_or_else(|| refused("connector_tool_unknown"))?;
            mirrored = clean::mirrored(marks, arguments)?;
        }
        let params = object(json!({"name": name, "arguments": arguments}));
        let found = self.ask("tools/call", params, Some(name), &mirrored)?;
        if found.get("resultType").and_then(Value::as_str) == Some("input_required") {
            return Err(refused("connector_needs_input"));
        }
        Ok(clean::result(&found, self.cap))
    }

    /// Ends the session at an older server that gave it an id, with one DELETE whose
    /// outcome is not read. A server of the per-request era has no session to end.
    pub(super) fn close(self) {
        let Some(session) = &self.session else {
            return;
        };
        if self.era != Era::Legacy {
            return;
        }
        let mut headers = vec![
            ("MCP-Protocol-Version".to_owned(), self.version.clone()),
            ("Mcp-Session-Id".to_owned(), session.clone()),
        ];
        if let Some(header) = &self.target.header {
            headers.push(header.clone());
        }
        let Ok(pins) = self.pins() else {
            return;
        };
        let _ = wire::send(
            &self.root,
            &Request {
                url: &self.target.url,
                verb: Verb::Delete,
                pins: pins.as_deref(),
                headers: &headers,
                auth: &self.auth,
                wait: None,
            },
        );
    }
}
