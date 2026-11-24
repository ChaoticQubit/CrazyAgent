use toml::{Table, Value};

use crate::Refused;
use crate::manifest::{self, MANIFEST_BYTES_MAX, line_of};
use crate::problem::Problem;
use crate::views::{PluginKind, RunsView, TrustView};

/// The one manifest version this app reads; a later one is refused, never guessed at.
pub const SCHEMA_VERSION: i64 = 1;
/// The batch size of an HTTP plugin that does not set one.
pub const BATCH_DEFAULT: u16 = 32;
/// The most inputs one HTTP plugin request carries.
pub const BATCH_MAX: u16 = 128;
/// The fewest numbers in one embedding vector.
pub const DIMENSIONS_MIN: u16 = 64;
/// The most numbers in one embedding vector.
pub const DIMENSIONS_MAX: u16 = 1024;
/// The most hosts one HTTP plugin declares.
pub const HOSTS_MAX: usize = 4;
/// The most bytes of a plugin's description.
pub const DESCRIPTION_BYTES_MAX: usize = 400;
/// The most bytes of a plugin's URL.
pub const URL_BYTES_MAX: usize = 2048;
/// The most bytes of one header value.
pub const HEADER_VALUE_BYTES_MAX: usize = 1024;
/// The headers the app sets itself, which no plugin may set.
pub const RESERVED_HEADERS: [&str; 12] = [
    "host",
    "content-length",
    "content-type",
    "transfer-encoding",
    "connection",
    "expect",
    "upgrade",
    "te",
    "trailer",
    "keep-alive",
    "proxy-authorization",
    "proxy-connection",
];
const SECRETS_MAX: usize = 4;
const HEADERS_MAX: usize = 16;
const NAME_BYTES_MAX: usize = 40;
const TITLE_BYTES_MAX: usize = 64;
const VERSION_BYTES_MAX: usize = 32;
const HOST_BYTES_MAX: usize = 253;
const SECRET_BYTES_MAX: usize = 32;
const HEADER_NAME_BYTES_MAX: usize = 64;
const VECTOR_STEPS_MAX: usize = 8;
const INPUTS: &str = "{{inputs}}";
const PURPOSE: &str = "{{purpose}}";
const LOOPBACK_HOSTS: [&str; 3] = ["127.0.0.1", "[::1]", "localhost"];
const EMBEDDING: &str = "embedding";
const EMBEDDING_TIER: &str = "t2";
const PLAIN_KEY_BYTES_MAX: usize = 64;
const HTTP_KEYS: [&str; 8] = [
    "url",
    "hosts",
    "secrets",
    "headers",
    "body",
    "vectors",
    "dimensions",
    "batch",
];

/// Whether a mark reorders, hides or breaks text on the trust screen without showing:
/// bidi embeddings, overrides, isolates and marks, zero-width and invisible characters, line
/// and paragraph separators, the soft hyphen, interlinear annotation marks and tag
/// characters. Input: the mark. Output: the answer.
fn hidden_mark(mark: char) -> bool {
    matches!(
        mark,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{E0000}'..='\u{E007F}'
    )
}

/// Whether a request embeds stored text or an agent's search query; `{{purpose}}` becomes
/// `document` or `query`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Query,
    Document,
}

impl Purpose {
    fn word(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Document => "document",
        }
    }
}

/// One piece of a header value: plain text, or the value of the declared secret at this
/// index, filled in by the engine at call time only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Text(String),
    Secret(usize),
}

/// One header an HTTP plugin sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub name: String,
    pub value: Vec<Piece>,
}

/// One step of the path to the vectors in a reply: a key, or every item of a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Key(String),
    Each,
}

/// The HTTP API an embedding plugin describes: the URL, its exact hosts, the secrets it
/// asks for by name, headers with secret placeholders, the request body with `{{inputs}}`
/// and `{{purpose}}`, the path to the vectors in the reply, their size and the batch size.
/// `local_unencrypted` is the plain http case to a literal loopback host with no secrets.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpSpec {
    pub url: url::Url,
    pub hosts: Vec<String>,
    pub secrets: Vec<String>,
    pub headers: Vec<Header>,
    pub body: serde_json::Value,
    pub vectors: Vec<Step>,
    pub dimensions: u16,
    pub batch: u16,
    pub local_unencrypted: bool,
}

impl HttpSpec {
    /// Builds one request body. Inputs: the texts (at most `batch`) and their purpose.
    /// Output: the body JSON with `{{inputs}}` as the list of texts and `{{purpose}}` as
    /// `query` or `document`.
    pub fn body(&self, inputs: &[&str], purpose: Purpose) -> serde_json::Value {
        assert!(!inputs.is_empty() && inputs.len() <= usize::from(self.batch));
        fill(&self.body, inputs, purpose)
    }
}

fn fill(value: &serde_json::Value, inputs: &[&str], purpose: Purpose) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) if text == INPUTS => serde_json::Value::Array(
            inputs
                .iter()
                .map(|input| serde_json::Value::String((*input).to_owned()))
                .collect(),
        ),
        serde_json::Value::String(text) if text == PURPOSE => {
            serde_json::Value::String(purpose.word().to_owned())
        }
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .map(|item| fill(item, inputs, purpose))
                .collect(),
        ),
        serde_json::Value::Object(entries) => serde_json::Value::Object(
            entries
                .iter()
                .map(|(key, item)| (key.clone(), fill(item, inputs, purpose)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// A validated embedding plugin's `plugin.toml`: its id, title, release, description,
/// kind, the HTTP API it describes and the SHA-256 of its exact bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub name: String,
    pub title: String,
    pub version: String,
    pub description: String,
    pub kind: PluginKind,
    pub http: HttpSpec,
    pub sha256: String,
}

impl Manifest {
    /// How the plugin runs, as screens show it.
    pub fn runs_view(&self) -> RunsView {
        RunsView::Http {
            url: self.http.url.as_str().to_owned(),
            hosts: self.http.hosts.clone(),
            local_unencrypted: self.http.local_unencrypted,
            dimensions: self.http.dimensions,
        }
    }

    /// The declared secret names.
    pub fn secrets(&self) -> &[String] {
        &self.http.secrets
    }

    /// The plugin's trust screen. Input: whether it replaces a user plugin of that name.
    pub fn trust_view(&self, replaces: bool) -> TrustView {
        TrustView {
            name: self.name.clone(),
            title: self.title.clone(),
            version: self.version.clone(),
            description: self.description.clone(),
            kind: self.kind,
            runs: self.runs_view(),
            secrets: self.secrets().to_vec(),
            sha256: self.sha256.clone(),
            replaces,
            changed: false,
            grew_hosts: Vec::new(),
        }
    }
}

fn problem(code: &'static str, field: &str) -> Problem {
    Problem::new(code, None, Some(field))
}

fn at_line(code: &'static str, line: usize) -> Problem {
    Problem::new(code, Some(u32::try_from(line).unwrap_or(u32::MAX)), None)
}

fn refused(refused: Refused) -> Problem {
    at_line(refused.code, refused.line)
}

fn syntax_line(text: &str, error: &toml::de::Error) -> usize {
    error.span().map_or(1, |span| {
        text.get(..span.start)
            .map_or(1, |before| before.matches('\n').count() + 1)
    })
}

/// The kind a manifest's text declares, read with no other key checked and, when the TOML
/// breaks at a line, from the lines before it, so a file refused after its `kind` is still
/// listed on its own kind's page. Input: the text. Output: the kind, or none when `kind` is
/// not a known word read before any break.
pub fn declared_kind(text: &str) -> Option<PluginKind> {
    if text.len() > MANIFEST_BYTES_MAX {
        return None;
    }
    let table = match text.parse::<Table>() {
        Ok(table) => table,
        Err(error) => {
            let kept = syntax_line(text, &error)
                .checked_sub(1)
                .filter(|kept| *kept > 0)?;
            let before: String = text.split_inclusive('\n').take(kept).collect();
            before.parse::<Table>().ok()?
        }
    };
    match table.get("kind") {
        Some(Value::String(kind)) if kind == EMBEDDING => Some(PluginKind::Embedding),
        _ => None,
    }
}

/// Reads and checks one embedding plugin's `plugin.toml`: the TOML, then the manifest
/// version, then the kind, then the five-key check with the embedding keys, then every
/// embedding key. A later version or an unknown kind is refused as such before its keys are
/// judged. No part of it runs. Input: the text. Output: the manifest, or the first problem
/// with its line or key path.
pub fn parse(text: &str) -> Result<Manifest, Problem> {
    if text.len() > MANIFEST_BYTES_MAX {
        return Err(at_line("plugin_manifest_invalid", 1));
    }
    let table = text
        .parse::<Table>()
        .map_err(|error| at_line("plugin_manifest_invalid", syntax_line(text, &error)))?;
    let fields = Fields::new(&table, "");
    match fields.required("version")? {
        Value::Integer(SCHEMA_VERSION) => {}
        Value::Integer(_) => return Err(problem("app_plugin_schema_unknown", "version")),
        _ => return Err(problem("app_plugin_field_invalid", "version")),
    }
    let kind = match fields.required("kind")? {
        Value::String(kind) if kind == EMBEDDING => PluginKind::Embedding,
        Value::String(_) => return Err(problem("app_plugin_kind_unknown", "kind")),
        _ => return Err(problem("app_plugin_field_invalid", "kind")),
    };
    if table.contains_key("http") && table.contains_key("net") {
        return Err(at_line("plugin_manifest_invalid", line_of(text, "[net]")));
    }
    let base = manifest::parse(text).map_err(refused)?;
    assert_eq!(base.kind, EMBEDDING, "the kind was read above");
    if !valid_name(&base.plugin) {
        return Err(problem("app_plugin_name_invalid", "plugin"));
    }
    if base.tier != EMBEDDING_TIER {
        return Err(problem("app_plugin_field_invalid", "tier"));
    }
    if !base.permissions.is_empty() {
        return Err(problem("app_plugin_field_invalid", "permissions"));
    }
    let title = fields.text("title", TITLE_BYTES_MAX)?;
    if title.trim().is_empty() {
        return Err(problem("app_plugin_field_invalid", "title"));
    }
    let version = match fields.optional("release") {
        None => String::new(),
        Some(Value::String(release))
            if !release.is_empty()
                && release.len() <= VERSION_BYTES_MAX
                && release.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-')
                }) =>
        {
            release.clone()
        }
        Some(_) => return Err(problem("app_plugin_field_invalid", "release")),
    };
    let description = match fields.optional("description") {
        None => String::new(),
        Some(_) => fields.text("description", DESCRIPTION_BYTES_MAX)?,
    };
    let http = http_spec(fields.required("http")?)?;
    Ok(Manifest {
        name: base.plugin,
        title,
        version,
        description,
        kind,
        http,
        sha256: kernel_common::hash(text.as_bytes()),
    })
}

/// Whether a plugin name is 1 to 40 lowercase letters, digits or `-`, starting with a
/// letter, and no name Windows opens as a device. Input: the name. Output: true when valid.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_BYTES_MAX
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !kernel_common::windows_device(name)
}

/// The keys of one manifest table with the key path they sit under, read by key with each
/// refusal naming the full path.
struct Fields<'a> {
    entries: &'a Table,
    prefix: String,
}

impl<'a> Fields<'a> {
    fn new(entries: &'a Table, path: &str) -> Self {
        Self {
            entries,
            prefix: if path.is_empty() {
                String::new()
            } else {
                format!("{path}.")
            },
        }
    }

    fn table(value: &'a Value, path: &str) -> Result<Self, Problem> {
        value
            .as_table()
            .map(|entries| Self::new(entries, path))
            .ok_or_else(|| problem("app_plugin_field_invalid", path))
    }

    fn path(&self, key: &str) -> String {
        format!("{}{key}", self.prefix)
    }

    fn parent(&self) -> &str {
        self.prefix.strip_suffix('.').unwrap_or(&self.prefix)
    }

    fn only(&self, known: &[&str]) -> Result<(), Problem> {
        match self
            .entries
            .keys()
            .find(|key| !known.contains(&key.as_str()))
        {
            Some(key) if !plain_key(key) => Err(problem("app_plugin_field_unknown", self.parent())),
            Some(key) => Err(problem("app_plugin_field_unknown", &self.path(key))),
            None => Ok(()),
        }
    }

    fn optional(&self, key: &str) -> Option<&'a Value> {
        self.entries.get(key)
    }

    fn required(&self, key: &str) -> Result<&'a Value, Problem> {
        self.optional(key)
            .ok_or_else(|| problem("app_plugin_field_missing", &self.path(key)))
    }

    fn text(&self, key: &str, bytes_max: usize) -> Result<String, Problem> {
        match self.required(key)? {
            Value::String(text)
                if !text.is_empty()
                    && text.len() <= bytes_max
                    && !text
                        .chars()
                        .any(|mark| mark.is_control() || hidden_mark(mark)) =>
            {
                Ok(text.clone())
            }
            _ => Err(problem("app_plugin_field_invalid", &self.path(key))),
        }
    }

    fn list(&self, key: &str) -> Result<Vec<&'a str>, Problem> {
        match self.optional(key) {
            None => Ok(Vec::new()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_str()
                        .ok_or_else(|| problem("app_plugin_field_invalid", &self.path(key)))
                })
                .collect(),
            Some(_) => Err(problem("app_plugin_field_invalid", &self.path(key))),
        }
    }

    fn number(
        &self,
        key: &str,
        code: &'static str,
        range: std::ops::RangeInclusive<u16>,
    ) -> Result<Option<u16>, Problem> {
        match self.optional(key) {
            None => Ok(None),
            Some(Value::Integer(number)) => u16::try_from(*number)
                .ok()
                .filter(|number| range.contains(number))
                .map(Some)
                .ok_or_else(|| problem(code, &self.path(key))),
            Some(_) => Err(problem(code, &self.path(key))),
        }
    }
}

fn http_spec(value: &Value) -> Result<HttpSpec, Problem> {
    let fields = Fields::table(value, "http")?;
    fields.only(&HTTP_KEYS)?;
    let raw = fields.text("url", URL_BYTES_MAX)?;
    if raw.contains("{{") {
        let code = if raw.contains("{{secret.") {
            "app_plugin_secret_outside_headers"
        } else {
            "app_plugin_placeholder_unknown"
        };
        return Err(problem(code, "http.url"));
    }
    let url = url::Url::parse(&raw).map_err(|_| problem("app_plugin_url_invalid", "http.url"))?;
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(problem("app_plugin_url_invalid", "http.url"));
    }
    if url.port() == Some(0) {
        return Err(problem("app_plugin_port_invalid", "http.url"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| problem("app_plugin_url_invalid", "http.url"))?
        .to_owned();
    let address = match url.host() {
        Some(url::Host::Ipv4(ip)) => Some(std::net::IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => Some(std::net::IpAddr::V6(ip)),
        _ => None,
    };
    if address.is_some_and(kernel_common::net::metadata_address) {
        return Err(problem("app_plugin_host_metadata", "http.url"));
    }
    let secrets = secret_names(&fields)?;
    let local_unencrypted = match url.scheme() {
        "https" => false,
        "http" if LOOPBACK_HOSTS.contains(&host.as_str()) => {
            if !secrets.is_empty() {
                return Err(problem("app_plugin_secret_needs_https", "http.url"));
            }
            true
        }
        _ => return Err(problem("app_plugin_url_not_https", "http.url")),
    };
    let hosts = host_names(&fields, &host)?;
    let headers = headers(&fields, &secrets)?;
    let body = body(fields.required("body")?)?;
    let vectors = match fields.required("vectors")? {
        Value::String(path) => vector_steps(path)?,
        _ => return Err(problem("app_plugin_vectors_invalid", "http.vectors")),
    };
    let dimensions = fields
        .number(
            "dimensions",
            "app_plugin_dimensions_invalid",
            DIMENSIONS_MIN..=DIMENSIONS_MAX,
        )?
        .ok_or_else(|| problem("app_plugin_field_missing", "http.dimensions"))?;
    let batch = fields
        .number("batch", "app_plugin_batch_invalid", 1..=BATCH_MAX)?
        .unwrap_or(BATCH_DEFAULT);
    Ok(HttpSpec {
        url,
        hosts,
        secrets,
        headers,
        body,
        vectors,
        dimensions,
        batch,
        local_unencrypted,
    })
}

fn secret_names(fields: &Fields<'_>) -> Result<Vec<String>, Problem> {
    let names = fields.list("secrets")?;
    if names.len() > SECRETS_MAX {
        return Err(problem("app_plugin_secrets_limit", "http.secrets"));
    }
    let mut secrets: Vec<String> = Vec::with_capacity(names.len());
    for name in names {
        if !valid_secret(name) {
            return Err(problem("app_plugin_secret_invalid", "http.secrets"));
        }
        if secrets.iter().any(|known| known == name) {
            return Err(problem("app_plugin_secret_duplicate", "http.secrets"));
        }
        secrets.push(name.to_owned());
    }
    Ok(secrets)
}

/// Whether a manifest key may be named in a refusal: an ASCII letter or digit first, then
/// letters, digits, `_`, `.` or `-`, at most 64 bytes, the YAML reader's plain key rule. A
/// key outside it is refused naming only its table, so no key text reaches a screen.
/// Input: the key. Output: true when plain.
fn plain_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    key.len() <= PLAIN_KEY_BYTES_MAX
        && bytes
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

/// Whether a secret name is 1 to 32 lowercase letters, digits or `_`, starting with a
/// letter. Input: the name. Output: true when valid.
pub fn valid_secret(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= SECRET_BYTES_MAX
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

/// Whether a header name is 1 to 64 letters, digits or `-`. Input: the name. Output: true
/// when valid; a reserved name is judged by the caller.
pub fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= HEADER_NAME_BYTES_MAX
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// Whether a host is an exact lowercase name of dot-separated labels, or `[::1]`. Input:
/// the host. Output: true when valid.
pub fn valid_host(host: &str) -> bool {
    if host == "[::1]" {
        return true;
    }
    !host.is_empty()
        && host.len() <= HOST_BYTES_MAX
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

fn host_names(fields: &Fields<'_>, url_host: &str) -> Result<Vec<String>, Problem> {
    let names = fields.list("hosts")?;
    if names.len() > HOSTS_MAX {
        return Err(problem("app_plugin_hosts_limit", "http.hosts"));
    }
    let mut hosts: Vec<String> = Vec::with_capacity(names.len());
    for name in names {
        if !valid_host(name) || hosts.iter().any(|known| known == name) {
            return Err(problem("app_plugin_host_invalid", "http.hosts"));
        }
        hosts.push(name.to_owned());
    }
    if !hosts.iter().any(|host| host == url_host) {
        return Err(problem("app_plugin_host_undeclared", "http.url"));
    }
    if hosts.iter().any(|host| host != url_host) {
        return Err(problem("app_plugin_host_unused", "http.hosts"));
    }
    Ok(hosts)
}

fn headers(fields: &Fields<'_>, secrets: &[String]) -> Result<Vec<Header>, Problem> {
    let empty = Table::new();
    let entries: &Table = match fields.optional("headers") {
        None => &empty,
        Some(Value::Table(entries)) => entries,
        Some(_) => return Err(problem("app_plugin_field_invalid", "http.headers")),
    };
    if entries.len() > HEADERS_MAX {
        return Err(problem("app_plugin_headers_limit", "http.headers"));
    }
    let mut headers: Vec<Header> = Vec::with_capacity(entries.len());
    let mut used = vec![false; secrets.len()];
    for (name, value) in entries {
        if !plain_key(name) {
            return Err(problem("app_plugin_header_invalid", "http.headers"));
        }
        let field = format!("http.headers.{name}");
        if !valid_header_name(name)
            || headers
                .iter()
                .any(|known| known.name.eq_ignore_ascii_case(name))
        {
            return Err(problem("app_plugin_header_invalid", &field));
        }
        if RESERVED_HEADERS
            .iter()
            .any(|reserved| reserved.eq_ignore_ascii_case(name))
        {
            return Err(problem("app_plugin_header_reserved", &field));
        }
        let text = match value {
            Value::String(text) => text.clone(),
            Value::Integer(number) => number.to_string(),
            _ => return Err(problem("app_plugin_header_invalid", &field)),
        };
        if text.is_empty()
            || text.len() > HEADER_VALUE_BYTES_MAX
            || !text.bytes().all(|byte| (0x20..0x7f).contains(&byte))
        {
            return Err(problem("app_plugin_header_invalid", &field));
        }
        let value = pieces(&text, secrets, &field)?;
        for piece in &value {
            if let Piece::Secret(index) = piece {
                used[*index] = true;
            }
        }
        headers.push(Header {
            name: name.clone(),
            value,
        });
    }
    if used.iter().any(|used| !used) {
        return Err(problem("app_plugin_secret_unused", "http.secrets"));
    }
    Ok(headers)
}

fn pieces(text: &str, secrets: &[String], field: &str) -> Result<Vec<Piece>, Problem> {
    let mut pieces = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        if open > 0 {
            pieces.push(Piece::Text(rest[..open].to_owned()));
        }
        let after = &rest[open + 2..];
        let close = after
            .find("}}")
            .ok_or_else(|| problem("app_plugin_placeholder_unknown", field))?;
        let inner = &after[..close];
        let Some(name) = inner.strip_prefix("secret.") else {
            return Err(problem("app_plugin_placeholder_unknown", field));
        };
        let index = secrets
            .iter()
            .position(|secret| secret == name)
            .ok_or_else(|| problem("app_plugin_secret_undeclared", field))?;
        pieces.push(Piece::Secret(index));
        rest = &after[close + 2..];
    }
    if rest.contains("}}") {
        return Err(problem("app_plugin_placeholder_unknown", field));
    }
    if !rest.is_empty() {
        pieces.push(Piece::Text(rest.to_owned()));
    }
    Ok(pieces)
}

fn body(value: &Value) -> Result<serde_json::Value, Problem> {
    if !value.is_table() {
        return Err(problem("app_plugin_field_invalid", "http.body"));
    }
    let mut inputs = 0usize;
    let json = check_body(value, "http.body", false, &mut inputs)?;
    match inputs {
        0 => Err(problem("app_plugin_inputs_missing", "http.body")),
        1 => Ok(json),
        _ => Err(problem("app_plugin_inputs_repeated", "http.body")),
    }
}

fn check_body(
    value: &Value,
    path: &str,
    in_list: bool,
    inputs: &mut usize,
) -> Result<serde_json::Value, Problem> {
    match value {
        Value::String(text) if text == INPUTS && !in_list => {
            *inputs += 1;
            Ok(serde_json::Value::String(text.clone()))
        }
        Value::String(text) if text == PURPOSE => Ok(serde_json::Value::String(text.clone())),
        Value::String(text) if text.contains("{{secret.") => {
            Err(problem("app_plugin_secret_outside_headers", path))
        }
        Value::String(text) if text.contains("{{") || text.contains("}}") => {
            Err(problem("app_plugin_placeholder_unknown", path))
        }
        Value::String(text) => Ok(serde_json::Value::String(text.clone())),
        Value::Integer(number) => Ok(serde_json::Value::from(*number)),
        Value::Float(number) => serde_json::Number::from_f64(*number)
            .map(serde_json::Value::Number)
            .ok_or_else(|| problem("app_plugin_field_invalid", path)),
        Value::Boolean(flag) => Ok(serde_json::Value::Bool(*flag)),
        Value::Datetime(_) => Err(problem("app_plugin_field_invalid", path)),
        Value::Array(items) => items
            .iter()
            .map(|item| check_body(item, path, true, inputs))
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
        Value::Table(entries) => entries
            .iter()
            .map(|(key, item)| {
                if !plain_key(key) {
                    return Err(problem("app_plugin_field_invalid", path));
                }
                check_body(item, &format!("{path}.{key}"), in_list, inputs)
                    .map(|json| (key.clone(), json))
            })
            .collect::<Result<serde_json::Map<_, _>, _>>()
            .map(serde_json::Value::Object),
    }
}

fn vector_steps(path: &str) -> Result<Vec<Step>, Problem> {
    let invalid = || problem("app_plugin_vectors_invalid", "http.vectors");
    let steps: Vec<Step> = path
        .split('.')
        .map(|step| match step {
            "*" => Ok(Step::Each),
            key if !key.is_empty()
                && key.len() <= 64
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')) =>
            {
                Ok(Step::Key(key.to_owned()))
            }
            _ => Err(invalid()),
        })
        .collect::<Result<_, _>>()?;
    let each = steps.iter().filter(|step| **step == Step::Each).count();
    if each != 1 || steps.len() > VECTOR_STEPS_MAX {
        return Err(invalid());
    }
    Ok(steps)
}
