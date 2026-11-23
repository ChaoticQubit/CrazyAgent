use super::capture::{self, CaptureRules};
use super::{Query, target};
use crate::error::StoreError;
use crate::secret::Piece::{Escaped, Form, Plain};
use crate::secret::{SecretString, sized, wipe};
use crate::secrets::Secrets;
use std::net::IpAddr;

/// The user agent every kernel request names.
pub const USER_AGENT: &str = "CrazyAgent";
/// The most reply headers one request may ask for.
pub const REPLY_HEADERS_MAX: usize = 16;
/// The most bytes of the reply body a plugin gets, equal to the SDK's bytes bound.
pub const REPLY_BYTES_MAX: usize = 4_194_304;
/// The most bytes of one reply header name or value, equal to the SDK's text bound.
pub const REPLY_TEXT_BYTES_MAX: usize = 8_192;
/// The most reply headers and captured fields a plugin gets, equal to the SDK's list bound.
pub const REPLY_LIST_MAX: usize = 64;

/// The HTTP method of one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    /// The method as curl's `request` line names it. Output: the name.
    fn name(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

/// One plain header or form field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub value: String,
}

/// One form field whose value is the secret behind a handle id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretField {
    pub name: String,
    pub secret: u64,
}

/// One reply field a request asks the kernel to capture, kept under a name when
/// `keep_as` is set, else held for this engine run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    pub field: String,
    pub keep_as: Option<String>,
}

/// One outgoing request as a plugin wrote it, with handle ids in place of secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<Field>,
    pub bearer: Option<u64>,
    pub form: Vec<Field>,
    pub secret_form: Vec<SecretField>,
    pub json: Option<Vec<u8>>,
    pub reply_headers: Vec<String>,
    pub capture: Vec<Capture>,
}

/// One captured reply field: its name and the handle id of its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    pub field: String,
    pub secret: u64,
}

/// What a plugin gets back: the status, the body without any captured field, the reply
/// headers it named and the handles of what was captured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    pub headers: Vec<Field>,
    pub captured: Vec<Captured>,
}

/// One outgoing request with its secrets filled in. No Debug, no Clone, wiped on drop.
/// Only egress reads it; plugins cannot name this crate.
pub struct Sealed {
    config: Vec<u8>,
    host: String,
    reply_headers: Vec<String>,
}

impl Sealed {
    /// The host the config's URL names, as egress allowed it. Output: the host.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Lends the filled curl config for one send. Output: the bytes; the caller never
    /// keeps them.
    pub fn config(&self) -> &[u8] {
        &self.config
    }

    /// The reply header names the request asked for, lowercase token characters only.
    /// Output: the names.
    pub fn reply_headers(&self) -> &[String] {
        &self.reply_headers
    }
}

impl Drop for Sealed {
    fn drop(&mut self) {
        wipe(&mut self.config);
    }
}

/// What egress got back: the status, the named reply headers and the body.
pub struct RawReply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// The egress seam: policy and the send. Implemented outside this crate.
pub trait Egress {
    /// Whether the calling plugin may reach one origin. Inputs: the plugin, scheme, host
    /// and port. Output: nothing, or the refusal.
    fn allows(
        &self,
        caller: &'static str,
        scheme: &str,
        host: &str,
        port: u16,
    ) -> Result<(), StoreError>;

    /// Resolves a host name once for one request. Inputs: the host and port. Output: every
    /// address it resolved to; the kernel checks each with [`super::address_allowed`] and
    /// pins the config's `resolve` line to the first, so `send` must connect only to the
    /// address that line names and never look the name up again.
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, StoreError>;

    /// Sends one sealed request. Input: the request. Output: the reply, or the failure.
    fn send(&self, sealed: Sealed) -> Result<RawReply, StoreError>;
}

/// The kernel's `net.request`: checks, fills secrets by handle, sends through egress and
/// captures reply fields into handles.
pub struct Requests<'r> {
    secrets: &'r Secrets<'r>,
    egress: &'r dyn Egress,
    rules: &'r CaptureRules,
}

impl<'r> Requests<'r> {
    /// Builds the request path of one owner request. Inputs: the secrets module, egress and
    /// the capture rules. Output: the request path.
    pub fn new(secrets: &'r Secrets<'r>, egress: &'r dyn Egress, rules: &'r CaptureRules) -> Self {
        Self {
            secrets,
            egress,
            rules,
        }
    }

    /// Sends one request of the calling plugin. Inputs: the plugin and the request. Output:
    /// the reply with captured fields replaced by handles, or a refusal. The URL and egress
    /// policy are checked before any secret is read; every refusal of the request's own
    /// shape comes before the send.
    pub fn request(&self, caller: &'static str, request: Request) -> Result<Reply, StoreError> {
        let parsed = target(&request.url, Query::Allowed)
            .map_err(|_| StoreError::code("net_url_invalid"))?
            .url;
        let host = parsed
            .host_str()
            .expect("a target always has a host")
            .to_owned();
        if matches!(parsed.host(), Some(url::Host::Ipv4(_)))
            && written_host(&request.url) != Some(&host)
        {
            return Err(StoreError::code("net_url_invalid"));
        }
        let port = parsed
            .port_or_known_default()
            .expect("https and http always have a port");
        self.egress.allows(caller, parsed.scheme(), &host, port)?;
        let pinned = pinned(self.egress, &parsed, port)?;
        check_shape(&request)?;
        let host_key = capture::host_key(&parsed);
        let fields = capture::fields(self.rules, caller, &host_key, &request.capture)?;
        for wanted in &fields {
            if let Some(name) = &wanted.keep_as {
                self.secrets.check_name(caller, name)?;
            }
        }
        if fields.len() > self.secrets.free_handles(caller) {
            return Err(StoreError::code("secret_handles_full"));
        }
        let bearer = match request.bearer {
            Some(id) => Some(self.secrets.value(caller, id, &host_key)?),
            None => None,
        };
        let mut secret_form = Vec::with_capacity(request.secret_form.len());
        for field in &request.secret_form {
            secret_form.push((
                field.name.as_str(),
                self.secrets.value(caller, field.secret, &host_key)?,
            ));
        }
        let config = config(
            &request,
            (&parsed, pinned.as_deref()),
            bearer.as_ref(),
            &secret_form,
        )?;
        drop(bearer);
        drop(secret_form);
        let sealed = Sealed {
            config,
            host,
            reply_headers: request
                .reply_headers
                .iter()
                .map(|name| name.to_ascii_lowercase())
                .collect(),
        };
        let raw = self.egress.send(sealed)?;
        let headers = named_headers(&request.reply_headers, raw.headers);
        let fits = headers.len() <= REPLY_LIST_MAX
            && headers.iter().all(|field| {
                field.name.len() <= REPLY_TEXT_BYTES_MAX
                    && field.value.len() <= REPLY_TEXT_BYTES_MAX
            });
        if !fits {
            return Err(StoreError::code("cap_output_too_large"));
        }
        let (body, captured) = capture::take(
            self.secrets,
            caller,
            (self.rules, &host_key),
            &fields,
            raw.body,
        )?;
        Ok(Reply {
            status: raw.status,
            body,
            headers,
            captured,
        })
    }
}

/// The host as the plugin wrote it in a URL: after `://`, before the path, query or
/// fragment, without user info or port. A URL parser reads hex, octal, short and integer
/// IPv4 forms as an address; comparing this with the parsed host refuses all of them, so
/// only the dotted decimal form is a target. Input: the raw URL. Output: the host text, or
/// none when the URL has no `://`.
fn written_host(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#', '\\']).next().unwrap_or(rest);
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    Some(match host.rsplit_once(':') {
        Some((name, port)) if port.bytes().all(|byte| byte.is_ascii_digit()) => name,
        _ => host,
    })
}

/// Checks the address a request may connect to: an IP literal host directly, a host name
/// through egress's single lookup, every resolved address checked. Inputs: egress, the
/// parsed URL and its port. Output: the `resolve` line value for a host name, none for a
/// literal, or `net_url_invalid` for a refused, missing or unresolved address.
fn pinned(egress: &dyn Egress, parsed: &url::Url, port: u16) -> Result<Option<String>, StoreError> {
    let invalid = || StoreError::code("net_url_invalid");
    let name = match parsed.host() {
        Some(url::Host::Ipv4(address)) => {
            return super::address_allowed(IpAddr::V4(address))
                .then_some(None)
                .ok_or_else(invalid);
        }
        Some(url::Host::Ipv6(address)) => {
            return super::address_allowed(IpAddr::V6(address))
                .then_some(None)
                .ok_or_else(invalid);
        }
        Some(url::Host::Domain(name)) => name,
        None => return Err(invalid()),
    };
    let resolved = egress.resolve(name, port)?;
    let first = *resolved.first().ok_or_else(invalid)?;
    if !resolved
        .iter()
        .all(|address| super::address_allowed(*address))
    {
        return Err(invalid());
    }
    let address = match first {
        IpAddr::V4(address) => address.to_string(),
        IpAddr::V6(address) => format!("[{address}]"),
    };
    Ok(Some(format!("{name}:{port}:{address}")))
}

/// Whether a name is an HTTP field name a curl config and write-out can carry: RFC 9110
/// token characters only, without `%`, which curl's write-out reads as a variable.
/// Input: the name. Output: the answer.
fn token(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$&'*+-.^_`|~".contains(&byte))
}

/// Checks the parts of a request that need no secret and no network: header and
/// reply-header names, the body kinds and the bounds. Input: the request. Output:
/// nothing, or the refusal.
fn check_shape(request: &Request) -> Result<(), StoreError> {
    for field in &request.headers {
        if !token(&field.name) || field.name.eq_ignore_ascii_case("host") {
            return Err(StoreError::code("net_header_name_invalid"));
        }
    }
    if request.reply_headers.len() > REPLY_HEADERS_MAX
        || !request.reply_headers.iter().all(|name| token(name))
    {
        return Err(StoreError::code("net_header_name_invalid"));
    }
    let form = !request.form.is_empty() || !request.secret_form.is_empty();
    if (form && request.json.is_some())
        || (request.method == Method::Get && (form || request.json.is_some()))
    {
        return Err(StoreError::code("net_request_invalid"));
    }
    if let Some(json) = &request.json
        && std::str::from_utf8(json).is_err()
    {
        return Err(StoreError::code("net_request_invalid"));
    }
    Ok(())
}

/// Builds curl's config for one request: the URL from the parsed target, the pinned
/// address for a host name, the user agent, the bearer, the headers, then the form or the
/// method with its JSON body. Inputs: the request, the parsed URL with its `resolve` line
/// value, the bearer value and the secret form values. Output: the config bytes, or
/// `net_request_invalid` for a control byte in any plugin text.
fn config(
    request: &Request,
    (parsed, pinned): (&url::Url, Option<&str>),
    bearer: Option<&SecretString>,
    secret_form: &[(&str, SecretString)],
) -> Result<Vec<u8>, StoreError> {
    let mut pieces = vec![Plain("url = \""), Escaped(parsed.as_str()), Plain("\"\n")];
    if let Some(pinned) = pinned {
        pieces.extend([Plain("resolve = \""), Escaped(pinned), Plain("\"\n")]);
    }
    pieces.extend([Plain("user-agent = \""), Plain(USER_AGENT), Plain("\"\n")]);
    if let Some(token) = bearer {
        pieces.extend([
            Plain("header = \"Authorization: Bearer "),
            Escaped(token.expose()),
            Plain("\"\n"),
        ]);
    }
    for field in &request.headers {
        pieces.extend([
            Plain("header = \""),
            Escaped(&field.name),
            Plain(": "),
            Escaped(&field.value),
            Plain("\"\n"),
        ]);
    }
    let form = !request.form.is_empty() || !secret_form.is_empty();
    if form {
        pieces.push(Plain("data = \""));
        let plain = request
            .form
            .iter()
            .map(|field| (field.name.as_str(), field.value.as_str()));
        let secret = secret_form
            .iter()
            .map(|(name, value)| (*name, value.expose()));
        for (index, (name, value)) in plain.chain(secret).enumerate() {
            if index > 0 {
                pieces.push(Plain("&"));
            }
            pieces.extend([Form(name), Plain("="), Form(value)]);
        }
        pieces.push(Plain("\"\n"));
    }
    let json = request
        .json
        .as_deref()
        .map(|bytes| std::str::from_utf8(bytes).expect("the shape check proved UTF-8"));
    let writes_method = match request.method {
        Method::Get => false,
        Method::Post => !form,
        Method::Put | Method::Patch | Method::Delete => true,
    };
    if writes_method {
        pieces.extend([
            Plain("request = \""),
            Plain(request.method.name()),
            Plain("\"\n"),
        ]);
    }
    match json {
        Some(json) => pieces.extend([
            Plain("header = \"Content-Type: application/json\"\ndata-raw = \""),
            Escaped(json),
            Plain("\"\n"),
        ]),
        None if writes_method && request.method != Method::Delete => {
            pieces.push(Plain("data-raw = \"\"\n"))
        }
        None => {}
    }
    sized(&pieces).ok_or_else(|| StoreError::code("net_request_invalid"))
}

/// Keeps only the reply headers a request named, in its order and with its spelling.
/// Inputs: the names and egress's headers. Output: the fields.
fn named_headers(names: &[String], headers: Vec<(String, String)>) -> Vec<Field> {
    let mut kept = Vec::new();
    for name in names {
        for (found, value) in &headers {
            if found.eq_ignore_ascii_case(name) {
                kept.push(Field {
                    name: name.clone(),
                    value: value.clone(),
                });
            }
        }
    }
    kept
}

#[cfg(test)]
mod budget_tests;
#[cfg(test)]
mod tests;
