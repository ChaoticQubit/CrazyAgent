use super::manifest::{
    DESCRIPTION_BYTES_MAX, Fields, HEADER_VALUE_BYTES_MAX, HOSTS_MAX, RESERVED_HEADERS,
    URL_BYTES_MAX, problem, valid_header_name, valid_host, valid_secret,
};
use super::yaml::Value;
use super::{ConnectorTrust, KeyTrust, OAuthTrust, Problem, ReadOnlyTrust};

/// The most OAuth scopes a connector lists for reading, and for writing.
pub const SCOPES_MAX: usize = 16;
/// The most read tool names a connector lists.
pub const READ_TOOLS_MAX: usize = 256;
/// The code of a connector whose only way in is an OAuth sign-in this build cannot do yet.
pub const OAUTH_NOT_YET: &str = "app_plugin_connector_oauth_not_yet";
const SCOPE_BYTES_MAX: usize = 128;
const TOOL_BYTES_MAX: usize = 128;
const WORD_BYTES_MAX: usize = 128;
/// The start of every header the MCP client mirrors a tool argument into; no manifest
/// header may begin with it.
pub const MIRROR_PREFIX: &str = "mcp-param-";
const OWN_HEADERS: [&str; 10] = [
    "authorization",
    "accept",
    "mcp-session-id",
    "mcp-protocol-version",
    "mcp-method",
    "mcp-name",
    "last-event-id",
    "cookie",
    "origin",
    "user-agent",
];

/// What a connector plugin declares: the vendor's remote MCP server, how the engine signs
/// in to it (a key, OAuth or both) and the tools the file says only read. It holds names
/// and addresses only, never a secret value; the engine keeps those and no agent sees them.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectorSpec {
    pub mcp: McpEndpoint,
    pub key: Option<KeyAuth>,
    pub oauth: Option<OAuthSpec>,
    pub read_tools: Vec<String>,
}

/// The vendor's MCP server: its https URL, the vendor's own read-only mode when it has
/// one, and the exact public hosts those addresses are on.
#[derive(Debug, Clone, PartialEq)]
pub struct McpEndpoint {
    pub url: url::Url,
    pub read_only: ReadOnly,
    pub hosts: Vec<String>,
}

#[cfg(test)]
impl McpEndpoint {
    /// A stand-in vendor on this machine for rows only: plain http to `127.0.0.1` at the
    /// port given, which no manifest can name and which the client takes in test builds
    /// alone. Input: the port. Output: the endpoint, with no read-only mode.
    pub(crate) fn loopback_for_rows(port: u16) -> Self {
        Self {
            url: url::Url::parse(&format!("http://127.0.0.1:{port}/mcp"))
                .expect("a loopback address parses"),
            read_only: ReadOnly::None,
            hosts: vec!["127.0.0.1".to_owned()],
        }
    }
}

/// The vendor's own read-only mode, used while writes are off as a second lock beside the
/// engine's own filter: another URL, one fixed header on every call, or none.
#[derive(Debug, Clone, PartialEq)]
pub enum ReadOnly {
    Url(url::Url),
    Header { name: String, value: String },
    None,
}

/// A key the owner enters: the secret names it is saved under (the key first, then the
/// user name of a Basic key), the header it is sent in, its scheme and one line of help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyAuth {
    pub secrets: Vec<String>,
    pub header: String,
    pub scheme: Scheme,
    pub help: String,
}

impl KeyAuth {
    /// The name of the secret that holds the key itself.
    pub fn secret(&self) -> &str {
        &self.secrets[0]
    }

    /// The name of the secret that holds the user name of a Basic key, none for Bearer.
    pub fn user_secret(&self) -> Option<&str> {
        self.secrets.get(1).map(String::as_str)
    }
}

/// How a key is sent: `Bearer <key>`, or `Basic` with the user name and the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Bearer,
    Basic,
}

/// The OAuth sign-in a connector declares: the exact public hosts it happens on, the ways
/// the app may register as a client in the order to try them, and the scopes to ask for
/// while only reading and when writing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthSpec {
    pub hosts: Vec<String>,
    pub registration: Vec<Registration>,
    pub scopes_read: Vec<String>,
    pub scopes_write: Vec<String>,
}

/// One way the app becomes an OAuth client: a client metadata document, dynamic client
/// registration, or the owner's own client id and secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Registration {
    Cimd,
    Dcr,
    Own,
}

impl ConnectorSpec {
    /// What the trust screen shows of this connector beyond its address and hosts: every
    /// field the engine acts on. Output: the read-only mode, the key's header, scheme and
    /// secret names, the OAuth sign-in and the read tools.
    pub fn trust(&self) -> ConnectorTrust {
        assert!(self.read_tools.len() <= READ_TOOLS_MAX);
        ConnectorTrust {
            read_only: match &self.mcp.read_only {
                ReadOnly::Url(url) => ReadOnlyTrust::Url {
                    url: url.as_str().to_owned(),
                },
                ReadOnly::Header { name, value } => ReadOnlyTrust::Header {
                    name: name.clone(),
                    value: value.clone(),
                },
                ReadOnly::None => ReadOnlyTrust::None,
            },
            key: self.key.as_ref().map(|key| KeyTrust {
                header: key.header.clone(),
                scheme: match key.scheme {
                    Scheme::Bearer => "Bearer".to_owned(),
                    Scheme::Basic => "Basic".to_owned(),
                },
                secrets: key.secrets.clone(),
            }),
            oauth: self.oauth.as_ref().map(|oauth| OAuthTrust {
                hosts: oauth.hosts.clone(),
                registration: oauth
                    .registration
                    .iter()
                    .map(|way| match way {
                        Registration::Cimd => "cimd".to_owned(),
                        Registration::Dcr => "dcr".to_owned(),
                        Registration::Own => "own".to_owned(),
                    })
                    .collect(),
                scopes_read: oauth.scopes_read.clone(),
                scopes_write: oauth.scopes_write.clone(),
            }),
            read_tools: self.read_tools.clone(),
        }
    }
}

/// Why a connector cannot run yet although its file is fine: one with OAuth and no key
/// waits for a sign-in this build does not have. Input: the connector. Output: the
/// refusal code, none when a key is its way in.
pub fn sign_in_refusal(spec: &ConnectorSpec) -> Option<&'static str> {
    assert!(spec.key.is_some() || spec.oauth.is_some());
    spec.key.is_none().then_some(OAUTH_NOT_YET)
}

/// Reads and checks the connector part of a manifest: `mcp`, `key`, `oauth` and `tools`.
/// Input: the manifest's top-level fields, already limited to a connector's keys. Output:
/// the connector, or the first problem with its field path.
pub(super) fn spec(fields: &Fields<'_>) -> Result<ConnectorSpec, Problem> {
    let mcp = endpoint(fields.required("mcp")?)?;
    let key = fields.optional("key").map(key_auth).transpose()?;
    if let (ReadOnly::Header { name, .. }, Some(key)) = (&mcp.read_only, &key)
        && name.eq_ignore_ascii_case(&key.header)
    {
        return Err(problem(
            "app_plugin_header_reserved",
            "mcp.read_only_header.name",
        ));
    }
    let oauth = fields.optional("oauth").map(oauth_spec).transpose()?;
    if key.is_none() && oauth.is_none() {
        return Err(problem("app_plugin_connector_auth_missing", "key"));
    }
    let read_tools = fields
        .optional("tools")
        .map_or_else(|| Ok(Vec::new()), read_tools)?;
    assert!((1..=HOSTS_MAX).contains(&mcp.hosts.len()));
    assert!(
        key.as_ref()
            .is_none_or(|key| (1..=2).contains(&key.secrets.len()))
    );
    assert!(read_tools.len() <= READ_TOOLS_MAX);
    Ok(ConnectorSpec {
        mcp,
        key,
        oauth,
        read_tools,
    })
}

fn endpoint(value: &Value) -> Result<McpEndpoint, Problem> {
    let fields = Fields::new(value, "mcp")?;
    fields.only(&["url", "read_only_url", "read_only_header", "hosts"])?;
    let url = address(&fields, "url")?;
    let hosts = hosts(&fields)?;
    declared(&url, &hosts, "mcp.url")?;
    let read_only = match (
        fields.optional("read_only_url"),
        fields.optional("read_only_header"),
    ) {
        (Some(_), Some(_)) => {
            return Err(problem(
                "app_plugin_connector_read_only_both",
                "mcp.read_only_header",
            ));
        }
        (Some(_), None) => {
            let read_only = address(&fields, "read_only_url")?;
            if read_only == url {
                return Err(problem("app_plugin_field_invalid", "mcp.read_only_url"));
            }
            declared(&read_only, &hosts, "mcp.read_only_url")?;
            ReadOnly::Url(read_only)
        }
        (None, Some(header)) => read_only_header(header)?,
        (None, None) => ReadOnly::None,
    };
    let read_only_host = match &read_only {
        ReadOnly::Url(read_only) => read_only.host_str(),
        ReadOnly::Header { .. } | ReadOnly::None => None,
    };
    let called = |host: &String| {
        url.host_str() == Some(host.as_str()) || read_only_host == Some(host.as_str())
    };
    if !hosts.iter().all(called) {
        return Err(problem("app_plugin_host_unused", "mcp.hosts"));
    }
    Ok(McpEndpoint {
        url,
        read_only,
        hosts,
    })
}

fn address(fields: &Fields<'_>, key: &str) -> Result<url::Url, Problem> {
    let field = fields.path(key);
    let raw = fields.text(key, URL_BYTES_MAX)?;
    let invalid = || problem("app_plugin_url_invalid", &field);
    if raw.contains("{{") || raw.contains("}}") {
        return Err(invalid());
    }
    let url = url::Url::parse(&raw).map_err(|_| invalid())?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
    {
        return Err(invalid());
    }
    if url.port() == Some(0) {
        return Err(problem("app_plugin_port_invalid", &field));
    }
    if url.scheme() != "https" {
        return Err(problem("app_plugin_connector_url_not_https", &field));
    }
    if !matches!(url.host(), Some(url::Host::Domain(name)) if public_host(name)) {
        return Err(problem("app_plugin_connector_host_public", &field));
    }
    Ok(url)
}

/// Whether a host is a public domain name: a valid host with a dot that a URL reads as a
/// name and not as an address, and that is not localhost. Input: the host. Output: true
/// when it is; the call-time guard still refuses a name that resolves to a private range.
fn public_host(host: &str) -> bool {
    valid_host(host)
        && host.contains('.')
        && !host.ends_with(".localhost")
        && url::Url::parse(&format!("https://{host}/"))
            .ok()
            .is_some_and(|url| matches!(url.host(), Some(url::Host::Domain(name)) if name == host))
}

fn hosts(fields: &Fields<'_>) -> Result<Vec<String>, Problem> {
    let field = fields.path("hosts");
    fields.required("hosts")?;
    let names = fields.list("hosts")?;
    if names.len() > HOSTS_MAX {
        return Err(problem("app_plugin_hosts_limit", &field));
    }
    let mut hosts: Vec<String> = Vec::with_capacity(names.len());
    for name in names {
        if !valid_host(name) || hosts.iter().any(|known| known == name) {
            return Err(problem("app_plugin_host_invalid", &field));
        }
        if !public_host(name) {
            return Err(problem("app_plugin_connector_host_public", &field));
        }
        hosts.push(name.to_owned());
    }
    if hosts.is_empty() {
        return Err(problem("app_plugin_host_invalid", &field));
    }
    Ok(hosts)
}

fn declared(url: &url::Url, hosts: &[String], field: &str) -> Result<(), Problem> {
    if hosts
        .iter()
        .any(|host| url.host_str() == Some(host.as_str()))
    {
        Ok(())
    } else {
        Err(problem("app_plugin_host_undeclared", field))
    }
}

fn reserved_header(name: &str, key_header: bool) -> bool {
    let mirrored = name
        .as_bytes()
        .get(..MIRROR_PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(MIRROR_PREFIX.as_bytes()));
    if mirrored {
        return true;
    }
    RESERVED_HEADERS
        .iter()
        .chain(&OWN_HEADERS)
        .filter(|reserved| !(key_header && **reserved == "authorization"))
        .any(|reserved| reserved.eq_ignore_ascii_case(name))
}

fn read_only_header(value: &Value) -> Result<ReadOnly, Problem> {
    let fields = Fields::new(value, "mcp.read_only_header")?;
    fields.only(&["name", "value"])?;
    let name = fields.text("name", WORD_BYTES_MAX)?;
    if !valid_header_name(&name) {
        return Err(problem(
            "app_plugin_header_invalid",
            "mcp.read_only_header.name",
        ));
    }
    if reserved_header(&name, false) {
        return Err(problem(
            "app_plugin_header_reserved",
            "mcp.read_only_header.name",
        ));
    }
    let value = match fields.required("value")? {
        Value::Str(text) => text.clone(),
        Value::Int(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        _ => String::new(),
    };
    if value.is_empty()
        || value.len() > HEADER_VALUE_BYTES_MAX
        || !value.bytes().all(|byte| (0x20..0x7f).contains(&byte))
        || value.contains("{{")
        || value.contains("}}")
    {
        return Err(problem(
            "app_plugin_header_invalid",
            "mcp.read_only_header.value",
        ));
    }
    Ok(ReadOnly::Header { name, value })
}

fn secret_name(fields: &Fields<'_>, key: &str) -> Result<String, Problem> {
    let name = fields.text(key, WORD_BYTES_MAX)?;
    if valid_secret(&name) {
        Ok(name)
    } else {
        Err(problem("app_plugin_secret_invalid", &fields.path(key)))
    }
}

fn key_auth(value: &Value) -> Result<KeyAuth, Problem> {
    let fields = Fields::new(value, "key")?;
    fields.only(&["secret", "user_secret", "header", "scheme", "help"])?;
    let mut secrets = vec![secret_name(&fields, "secret")?];
    let header = fields.text("header", WORD_BYTES_MAX)?;
    if !valid_header_name(&header) {
        return Err(problem("app_plugin_header_invalid", "key.header"));
    }
    if reserved_header(&header, true) {
        return Err(problem("app_plugin_header_reserved", "key.header"));
    }
    let scheme = match fields.text("scheme", WORD_BYTES_MAX)?.as_str() {
        "Bearer" => Scheme::Bearer,
        "Basic" => Scheme::Basic,
        _ => return Err(problem("app_plugin_connector_scheme_unknown", "key.scheme")),
    };
    match (scheme, fields.optional("user_secret")) {
        (Scheme::Bearer, None) => {}
        (Scheme::Bearer, Some(_)) => {
            return Err(problem(
                "app_plugin_connector_user_secret",
                "key.user_secret",
            ));
        }
        (Scheme::Basic, None) => {
            return Err(problem("app_plugin_field_missing", "key.user_secret"));
        }
        (Scheme::Basic, Some(_)) => {
            let user = secret_name(&fields, "user_secret")?;
            if user == secrets[0] {
                return Err(problem("app_plugin_secret_duplicate", "key.user_secret"));
            }
            secrets.push(user);
        }
    }
    let help = match fields.optional("help") {
        None | Some(Value::Null) => String::new(),
        Some(_) => fields.text("help", DESCRIPTION_BYTES_MAX)?,
    };
    Ok(KeyAuth {
        secrets,
        header,
        scheme,
        help,
    })
}

fn oauth_spec(value: &Value) -> Result<OAuthSpec, Problem> {
    let fields = Fields::new(value, "oauth")?;
    fields.only(&["hosts", "registration", "scopes_read", "scopes_write"])?;
    let hosts = hosts(&fields)?;
    let invalid = || {
        problem(
            "app_plugin_connector_registration_invalid",
            "oauth.registration",
        )
    };
    fields.required("registration")?;
    let mut registration: Vec<Registration> = Vec::with_capacity(3);
    for word in fields.list("registration")? {
        let way = match word {
            "cimd" => Registration::Cimd,
            "dcr" => Registration::Dcr,
            "own" => Registration::Own,
            _ => return Err(invalid()),
        };
        if registration.contains(&way) {
            return Err(invalid());
        }
        registration.push(way);
    }
    if registration.is_empty() {
        return Err(invalid());
    }
    assert!(registration.len() <= 3);
    Ok(OAuthSpec {
        hosts,
        registration,
        scopes_read: scopes(&fields, "scopes_read")?,
        scopes_write: scopes(&fields, "scopes_write")?,
    })
}

fn scopes(fields: &Fields<'_>, key: &str) -> Result<Vec<String>, Problem> {
    let invalid = || problem("app_plugin_connector_scope_invalid", &fields.path(key));
    let names = fields.list(key)?;
    if names.len() > SCOPES_MAX {
        return Err(invalid());
    }
    let mut scopes: Vec<String> = Vec::with_capacity(names.len());
    for name in names {
        let valid = !name.is_empty()
            && name.len() <= SCOPE_BYTES_MAX
            && name
                .bytes()
                .all(|byte| (0x21..0x7f).contains(&byte) && byte != b'"' && byte != b'\\');
        if !valid || scopes.iter().any(|known| known == name) {
            return Err(invalid());
        }
        scopes.push(name.to_owned());
    }
    Ok(scopes)
}

fn read_tools(value: &Value) -> Result<Vec<String>, Problem> {
    let fields = Fields::new(value, "tools")?;
    fields.only(&["read"])?;
    let names = fields.list("read")?;
    if names.len() > READ_TOOLS_MAX {
        return Err(problem("app_plugin_connector_tools_limit", "tools.read"));
    }
    let mut tools: Vec<String> = Vec::with_capacity(names.len());
    for name in names {
        let valid = !name.is_empty()
            && name.len() <= TOOL_BYTES_MAX
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
        if !valid || tools.iter().any(|known| known == name) {
            return Err(problem("app_plugin_connector_tool_invalid", "tools.read"));
        }
        tools.push(name.to_owned());
    }
    Ok(tools)
}

#[cfg(test)]
#[path = "connector_tests.rs"]
mod tests;
