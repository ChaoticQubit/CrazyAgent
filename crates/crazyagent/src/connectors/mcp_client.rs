//! The engine as an MCP client to a vendor's remote server (Streamable HTTP through the app
//! plugins curl runner). It speaks both protocol eras: per-request metadata (2026-07-28) and
//! the older `initialize` handshake with its session id. The key is read for one request
//! and goes only to curl's stdin; this module writes it nowhere else.

use crate::app_plugins::connector::{KeyAuth, McpEndpoint, Scheme};
use crate::app_plugins::store::Bound;
use crate::connectors::settings::SizeCap;
use crate::provider_reach::ProviderReach;
use serde_json::Value;
use std::path::Path;

#[cfg(unix)]
mod clean;
#[cfg(unix)]
mod guard;
#[cfg(unix)]
mod live;
#[cfg(unix)]
mod wire;

/// The most tools one connector may list; a longer list is refused, never cut.
pub const TOOLS_MAX: usize = 512;
/// The largest request body sent to a vendor.
pub const REQUEST_BYTES_MAX: usize = 64 * 1024;
/// The most bytes of one vendor reply the engine reads before it gives up on the call.
pub const REPLY_BYTES_MAX: usize = 8 * 1024 * 1024;
/// The protocol revision spoken with per-request metadata and no handshake.
pub const MODERN_VERSION: &str = "2026-07-28";
/// The protocol revisions spoken after an `initialize` handshake, newest first.
pub const LEGACY_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];
/// The line that ends a tool result the size setting cut.
pub const CUT_MARK: &str =
    "\n[Cut here: the reply was larger than the size limit in Settings > Connectors.]";

/// Every refusal code this client gives, sorted; each has a plain sentence.
pub const MCP_CODES: [&str; 19] = [
    "connector_address_refused",
    "connector_arguments_invalid",
    "connector_call_failed",
    "connector_host_private",
    "connector_host_unknown",
    "connector_key_missing",
    "connector_needs_input",
    "connector_protocol_unsupported",
    "connector_redirect_refused",
    "connector_reply_invalid",
    "connector_reply_too_large",
    "connector_request_too_large",
    "connector_sign_in_needed",
    "connector_timeout",
    "connector_tool_name_invalid",
    "connector_tool_unknown",
    "connector_tools_limit",
    "connector_unavailable_here",
    "connector_vendor_error",
];

/// How the engine signs in to a vendor: today a key the owner entered, named by where it is
/// saved and never held here by value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    Key(KeyRef),
}

/// Where a connector's key is saved and how it is sent: the header, the scheme, and the
/// bindings in declaration order (the key first, then the user name of a Basic key).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))]
pub struct KeyRef {
    header: String,
    scheme: Scheme,
    bindings: Vec<Bound>,
}

impl Credential {
    /// Names a connector's key. Inputs: the manifest's key block and the bindings of a
    /// connector from `store::connector_ready`, one per declared secret. Output: the
    /// credential, which holds no key value.
    pub fn key(auth: &KeyAuth, bindings: &[Bound]) -> Self {
        assert_eq!(
            auth.secrets.len(),
            bindings.len(),
            "one binding per declared secret"
        );
        Self::Key(KeyRef {
            header: auth.header.clone(),
            scheme: auth.scheme,
            bindings: bindings.to_vec(),
        })
    }
}

/// Which of a vendor's addresses a session calls: the normal one, or while writes are off
/// the vendor's own read-only address or header where it has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Full,
    ReadOnly,
}

/// One tool a vendor lists, with hidden characters taken out of its texts: its name as the
/// vendor calls it, what it says it does, its argument schema and the vendor's own hints,
/// which are claims, not facts.
#[derive(Debug, Clone, PartialEq)]
pub struct VendorTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub read_only_hint: Option<bool>,
    pub destructive_hint: Option<bool>,
}

/// What one tool call gave back, as text only: the vendor's text with hidden characters
/// taken out, whether the vendor marked it an error, and whether the size setting cut it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
    pub cut: bool,
}

/// Why a vendor call gave nothing: the key or its scope was refused, the reply was too
/// large to read, the vendor took too long, the engine refused (a code from `MCP_CODES`),
/// or the vendor answered with an error of its own, hidden characters taken out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpError {
    Unauthorized { scope: Option<String> },
    TooLarge,
    Timeout,
    Refused(&'static str),
    Vendor(String),
}

impl McpError {
    /// The code a page shows for this error, one of `MCP_CODES`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized { .. } => "connector_sign_in_needed",
            Self::TooLarge => "connector_reply_too_large",
            Self::Timeout => "connector_timeout",
            Self::Refused(code) => code,
            Self::Vendor(_) => "connector_vendor_error",
        }
    }
}

/// One conversation with one vendor server for one launch: where it calls, how it signs in
/// and which protocol era the server answered in. It holds no key value.
pub struct McpSession {
    #[cfg(unix)]
    live: live::Live,
}

impl std::fmt::Debug for McpSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpSession")
            .field("left_out", &self.left_out())
            .finish_non_exhaustive()
    }
}

impl McpSession {
    /// Opens a session: checks the address, finds the server's protocol era and, for an
    /// older server, runs the handshake. Blocking; never call it on the engine tick. Inputs:
    /// the engine root, the connector's endpoint and credential, which address to use and
    /// the size setting. Output: the session, or why the vendor could not be reached.
    pub fn open(
        root: &Path,
        endpoint: &McpEndpoint,
        auth: &Credential,
        access: Access,
        cap: SizeCap,
    ) -> Result<Self, McpError> {
        Self::open_with(root, endpoint, auth, access, cap, ProviderReach::system())
    }
}

#[cfg(unix)]
impl McpSession {
    /// [`Self::open`] with the name lookup given, so rows do no real DNS.
    pub(crate) fn open_with(
        root: &Path,
        endpoint: &McpEndpoint,
        auth: &Credential,
        access: Access,
        cap: SizeCap,
        reach: ProviderReach,
    ) -> Result<Self, McpError> {
        live::Live::open(root, endpoint, auth, access, cap, reach).map(|live| Self { live })
    }

    /// [`Self::open_with`] for rows that count lookups: the stand-in's address on this
    /// machine is looked up and vetted before every request, and never handed to curl.
    #[cfg(test)]
    pub(crate) fn open_vetting(
        root: &Path,
        endpoint: &McpEndpoint,
        auth: &Credential,
        access: Access,
        cap: SizeCap,
        reach: ProviderReach,
    ) -> Result<Self, McpError> {
        live::Live::open_vetting(root, endpoint, auth, access, cap, reach).map(|live| Self { live })
    }

    /// Lists the vendor's tools, every page, cleaned. Output: at most `TOOLS_MAX` tools, or
    /// `connector_tools_limit` for a longer list.
    pub fn list_tools(&mut self) -> Result<Vec<VendorTool>, McpError> {
        self.live.list_tools()
    }

    /// How many tools the last list left out because their name or schema could not be
    /// shown or called safely.
    pub fn left_out(&self) -> usize {
        self.live.left_out()
    }

    /// Calls one tool. Inputs: the vendor's tool name and the arguments, a JSON object.
    /// Output: the result as text, cut to the size setting, or why there is none.
    pub fn call(&mut self, name: &str, args: &Value) -> Result<ToolResult, McpError> {
        self.live.call(name, args)
    }

    /// Ends the session at an older server that gave it an id; best effort.
    pub fn close(self) {
        self.live.close();
    }
}

#[cfg(not(unix))]
impl McpSession {
    /// No vendor call where the private request file cannot be made. Output:
    /// `connector_unavailable_here`.
    pub(crate) fn open_with(
        _root: &Path,
        _endpoint: &McpEndpoint,
        _auth: &Credential,
        _access: Access,
        _cap: SizeCap,
        _reach: ProviderReach,
    ) -> Result<Self, McpError> {
        Err(McpError::Refused("connector_unavailable_here"))
    }

    /// No session exists here. Output: `connector_unavailable_here`.
    pub fn list_tools(&mut self) -> Result<Vec<VendorTool>, McpError> {
        Err(McpError::Refused("connector_unavailable_here"))
    }

    /// No session exists here. Output: zero.
    pub fn left_out(&self) -> usize {
        0
    }

    /// No session exists here. Output: `connector_unavailable_here`.
    pub fn call(&mut self, _name: &str, _args: &Value) -> Result<ToolResult, McpError> {
        Err(McpError::Refused("connector_unavailable_here"))
    }

    /// No session exists here.
    pub fn close(self) {}
}

#[cfg(all(test, unix))]
#[path = "mcp_client_tests.rs"]
pub(super) mod tests;
