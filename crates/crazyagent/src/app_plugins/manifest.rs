use super::yaml::{self, Value};
use super::{PluginKind, Problem, RunsView, TrustView};

/// The batch size of an HTTP plugin that does not set one.
pub const BATCH_DEFAULT: u16 = 32;
/// The most inputs one HTTP plugin request carries.
pub const BATCH_MAX: u16 = 128;
/// The fewest and most numbers in one embedding vector.
pub const DIMENSIONS_MIN: u16 = 64;
pub const DIMENSIONS_MAX: u16 = 1024;
const HOSTS_MAX: usize = 4;
const SECRETS_MAX: usize = 4;
const HEADERS_MAX: usize = 16;
const NAME_BYTES_MAX: usize = 40;
const TITLE_BYTES_MAX: usize = 64;
const VERSION_BYTES_MAX: usize = 32;
const DESCRIPTION_BYTES_MAX: usize = 400;
const URL_BYTES_MAX: usize = 2048;
const HOST_BYTES_MAX: usize = 253;
const SECRET_BYTES_MAX: usize = 32;
const HEADER_NAME_BYTES_MAX: usize = 64;
const HEADER_VALUE_BYTES_MAX: usize = 1024;
const VECTOR_STEPS_MAX: usize = 8;
const INPUTS: &str = "{{inputs}}";
const PURPOSE: &str = "{{purpose}}";
const RESERVED_HEADERS: [&str; 12] = [
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
const LOOPBACK_HOSTS: [&str; 3] = ["127.0.0.1", "[::1]", "localhost"];

/// A local runtime the app itself runs; its download source and hashes live in engine
/// code, never in a manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runtime {
    StaticRetrievalMrlEnV1,
}

impl Runtime {
    /// The id a manifest names the runtime by.
    pub fn id(self) -> &'static str {
        match self {
            Self::StaticRetrievalMrlEnV1 => "static-retrieval-mrl-en-v1",
        }
    }

    /// The runtime with this id. Input: the id. Output: the runtime, none when unknown.
    pub fn from_id(id: &str) -> Option<Self> {
        [Self::StaticRetrievalMrlEnV1]
            .into_iter()
            .find(|runtime| runtime.id() == id)
    }

    /// The bytes of the runtime's one-time download.
    pub fn download_bytes(self) -> u64 {
        match self {
            Self::StaticRetrievalMrlEnV1 => crate::semantic::local::DOWNLOAD_BYTES,
        }
    }
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

/// How a plugin runs: the app's local runtime, or an HTTP API.
#[derive(Debug, Clone, PartialEq)]
pub enum Runs {
    Local { runtime: Runtime },
    Http(Box<HttpSpec>),
}

/// A validated plugin manifest and the SHA-256 of its exact bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub name: String,
    pub title: String,
    pub version: String,
    pub description: String,
    pub kind: PluginKind,
    pub runs: Runs,
    pub sha256: String,
}

impl Manifest {
    /// How the plugin runs, as screens show it.
    pub fn runs_view(&self) -> RunsView {
        match &self.runs {
            Runs::Local { runtime } => RunsView::Local {
                runtime: runtime.id().to_owned(),
                download_bytes: runtime.download_bytes(),
            },
            Runs::Http(http) => RunsView::Http {
                url: http.url.as_str().to_owned(),
                hosts: http.hosts.clone(),
                local_unencrypted: http.local_unencrypted,
                dimensions: http.dimensions,
            },
        }
    }

    /// The declared secret names, none for a local plugin.
    pub fn secrets(&self) -> &[String] {
        match &self.runs {
            Runs::Local { .. } => &[],
            Runs::Http(http) => &http.secrets,
        }
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
        }
    }
}

fn problem(code: &'static str, field: &str) -> Problem {
    Problem::new(code, None, Some(field))
}

/// Reads and checks one plugin manifest: the strict YAML subset, then every field. No
/// part of it runs. Input: the manifest text. Output: the manifest, or the first problem
/// with its line (YAML) or field path.
pub fn parse(text: &str) -> Result<Manifest, Problem> {
    let value = yaml::parse(text).map_err(|error| error.problem())?;
    let fields = Fields::new(&value, "")?;
    fields.only(&[
        "schema",
        "kind",
        "name",
        "title",
        "version",
        "description",
        "local",
        "http",
    ])?;
    match fields.required("schema")? {
        Value::Int(1) => {}
        Value::Int(_) => return Err(problem("app_plugin_schema_unknown", "schema")),
        _ => return Err(problem("app_plugin_field_invalid", "schema")),
    }
    let kind = match fields.required("kind")? {
        Value::Str(kind) if kind == "embedding" => PluginKind::Embedding,
        Value::Str(_) => return Err(problem("app_plugin_kind_unknown", "kind")),
        _ => return Err(problem("app_plugin_field_invalid", "kind")),
    };
    let name = fields.text("name", TITLE_BYTES_MAX)?;
    if !valid_name(&name) {
        return Err(problem("app_plugin_name_invalid", "name"));
    }
    let title = fields.text("title", TITLE_BYTES_MAX)?;
    if title.trim().is_empty() {
        return Err(problem("app_plugin_field_invalid", "title"));
    }
    let version = match fields.required("version")? {
        Value::Int(number) if *number >= 0 => number.to_string(),
        Value::Str(version)
            if !version.is_empty()
                && version.len() <= VERSION_BYTES_MAX
                && version.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-')
                }) =>
        {
            version.clone()
        }
        _ => return Err(problem("app_plugin_field_invalid", "version")),
    };
    let description = match fields.optional("description") {
        None | Some(Value::Null) => String::new(),
        Some(_) => fields.text("description", DESCRIPTION_BYTES_MAX)?,
    };
    let runs = match (fields.optional("local"), fields.optional("http")) {
        (Some(local), None) => local_runs(local)?,
        (None, Some(http)) => Runs::Http(Box::new(http_spec(http)?)),
        _ => return Err(problem("app_plugin_runs_invalid", "local")),
    };
    Ok(Manifest {
        name,
        title,
        version,
        description,
        kind,
        runs,
        sha256: crate::semantic::sha256_hex(text.as_bytes()),
    })
}

/// Whether a plugin name is 1 to 40 lowercase letters, digits or `-`, starting with a
/// letter. Input: the name. Output: true when valid.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_BYTES_MAX
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

struct Fields<'a> {
    entries: &'a [(String, Value)],
    prefix: String,
}

impl<'a> Fields<'a> {
    fn new(value: &'a Value, path: &str) -> Result<Self, Problem> {
        match value {
            Value::Map(entries) => Ok(Self {
                entries,
                prefix: if path.is_empty() {
                    String::new()
                } else {
                    format!("{path}.")
                },
            }),
            _ => Err(problem(
                "app_plugin_field_invalid",
                if path.is_empty() { "file" } else { path },
            )),
        }
    }

    fn path(&self, key: &str) -> String {
        format!("{}{key}", self.prefix)
    }

    fn only(&self, known: &[&str]) -> Result<(), Problem> {
        match self
            .entries
            .iter()
            .find(|(key, _)| !known.contains(&key.as_str()))
        {
            Some((key, _)) => Err(problem("app_plugin_field_unknown", &self.path(key))),
            None => Ok(()),
        }
    }

    fn optional(&self, key: &str) -> Option<&'a Value> {
        self.entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    fn required(&self, key: &str) -> Result<&'a Value, Problem> {
        self.optional(key)
            .ok_or_else(|| problem("app_plugin_field_missing", &self.path(key)))
    }

    fn text(&self, key: &str, bytes_max: usize) -> Result<String, Problem> {
        match self.required(key)? {
            Value::Str(text)
                if !text.is_empty()
                    && text.len() <= bytes_max
                    && !text.chars().any(char::is_control) =>
            {
                Ok(text.clone())
            }
            _ => Err(problem("app_plugin_field_invalid", &self.path(key))),
        }
    }

    fn list(&self, key: &str) -> Result<Vec<&'a str>, Problem> {
        match self.optional(key) {
            None => Ok(Vec::new()),
            Some(Value::List(items)) => items
                .iter()
                .map(|item| match item {
                    Value::Str(text) => Ok(text.as_str()),
                    _ => Err(problem("app_plugin_field_invalid", &self.path(key))),
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
            Some(Value::Int(number)) => u16::try_from(*number)
                .ok()
                .filter(|number| range.contains(number))
                .map(Some)
                .ok_or_else(|| problem(code, &self.path(key))),
            Some(_) => Err(problem(code, &self.path(key))),
        }
    }
}

fn local_runs(value: &Value) -> Result<Runs, Problem> {
    let fields = Fields::new(value, "local")?;
    fields.only(&["runtime"])?;
    let id = fields.text("runtime", 64)?;
    let runtime = Runtime::from_id(&id)
        .ok_or_else(|| problem("app_plugin_runtime_unknown", "local.runtime"))?;
    Ok(Runs::Local { runtime })
}

fn http_spec(value: &Value) -> Result<HttpSpec, Problem> {
    let fields = Fields::new(value, "http")?;
    fields.only(&[
        "url",
        "hosts",
        "secrets",
        "headers",
        "body",
        "vectors",
        "dimensions",
        "batch",
    ])?;
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
        Value::Str(path) => vector_steps(path)?,
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
        let valid = !name.is_empty()
            && name.len() <= SECRET_BYTES_MAX
            && name.as_bytes()[0].is_ascii_lowercase()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
        if !valid {
            return Err(problem("app_plugin_secret_invalid", "http.secrets"));
        }
        if secrets.iter().any(|known| known == name) {
            return Err(problem("app_plugin_secret_duplicate", "http.secrets"));
        }
        secrets.push(name.to_owned());
    }
    Ok(secrets)
}

fn valid_host(host: &str) -> bool {
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
    let entries: &[(String, Value)] = match fields.optional("headers") {
        None => &[],
        Some(Value::Map(entries)) => entries,
        Some(_) => return Err(problem("app_plugin_field_invalid", "http.headers")),
    };
    if entries.len() > HEADERS_MAX {
        return Err(problem("app_plugin_headers_limit", "http.headers"));
    }
    let mut headers: Vec<Header> = Vec::with_capacity(entries.len());
    let mut used = vec![false; secrets.len()];
    for (name, value) in entries {
        let field = format!("http.headers.{name}");
        let valid_name = !name.is_empty()
            && name.len() <= HEADER_NAME_BYTES_MAX
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
        if !valid_name
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
            Value::Str(text) => text.clone(),
            Value::Int(number) => number.to_string(),
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
    if !matches!(value, Value::Map(_)) {
        return Err(problem("app_plugin_field_invalid", "http.body"));
    }
    let mut inputs = 0usize;
    check_body(value, "http.body", false, &mut inputs)?;
    match inputs {
        0 => Err(problem("app_plugin_inputs_missing", "http.body")),
        1 => Ok(value.to_json()),
        _ => Err(problem("app_plugin_inputs_repeated", "http.body")),
    }
}

fn check_body(value: &Value, path: &str, in_list: bool, inputs: &mut usize) -> Result<(), Problem> {
    match value {
        Value::Str(text) if text == INPUTS && !in_list => {
            *inputs += 1;
            Ok(())
        }
        Value::Str(text) if text == PURPOSE => Ok(()),
        Value::Str(text) if text.contains("{{secret.") => {
            Err(problem("app_plugin_secret_outside_headers", path))
        }
        Value::Str(text) if text.contains("{{") || text.contains("}}") => {
            Err(problem("app_plugin_placeholder_unknown", path))
        }
        Value::List(items) => items
            .iter()
            .try_for_each(|item| check_body(item, path, true, inputs)),
        Value::Map(entries) => entries.iter().try_for_each(|(key, item)| {
            check_body(item, &format!("{path}.{key}"), in_list, inputs)
        }),
        _ => Ok(()),
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

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
