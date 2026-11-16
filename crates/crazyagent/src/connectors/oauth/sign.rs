use super::{
    OAuthError, OWN_ID_BYTES_MAX, Pending, key_store, mark_sign_in, not_offered, now_ms,
    own_issuer_ref, own_ref, own_secret_ref, signed_in, visible,
};
use crate::app_plugins::connector::{
    ConnectorSpec, McpEndpoint, OAuthSpec, ReadOnly, Registration,
};
use crate::app_plugins::http::{self, Config, Ended};
use crate::app_plugins::store;
use crate::connectors::mcp_client::{Credential, McpError, guard, wire};
use crate::credentials::SecretString;
use crate::protocol::CredentialRef;
use crate::provider_reach::ProviderReach;
use crate::storage::accounts::{self, PLUGIN_KEYS};
use base64::Engine;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The most sign-ins one engine root waits on at once; a new one drops the oldest.
const FLOWS_MAX: usize = 8;
/// How long a sign-in waits for the browser's answer.
const FLOW_LIFE: Duration = Duration::from_secs(10 * 60);
/// The random bytes behind one state or one PKCE verifier: 43 characters once encoded.
const RANDOM_BYTES: usize = 32;
const CONFIG_BYTES: usize = 32 * 1024;
const FORM_BYTES: usize = 8 * 1024;
const HEAD_BYTES_MAX: usize = 16 * 1024;
const REPLY_BYTES_MAX: usize = 64 * 1024;
const CHALLENGES_MAX: usize = 8;
const CODE_BYTES_MAX: usize = 1024;
/// The longest token kept, the key store's own bound on one key.
const TOKEN_BYTES_MAX: usize = 8192;
/// The request that asks an MCP server which authorization server it trusts: any request
/// without a token is answered 401 with the resource metadata address.
const PROBE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
/// How long before its end an access token is renewed, so a call never starts on one that
/// ends on the way.
const RENEW_EARLY_MS: i64 = 60_000;
/// The longest access token lifetime taken from a server, ten years.
const LIFETIME_MAX_S: i64 = 10 * 365 * 24 * 60 * 60;
/// The most step-up sign-ins in a row before the engine stops asking for more scope.
const STEP_UPS_MAX: u8 = 2;
/// The most scopes one challenge may name.
const SCOPES_MAX: usize = 32;

fn failed() -> OAuthError {
    OAuthError("connector_call_failed")
}

fn reply_invalid() -> OAuthError {
    OAuthError("connector_reply_invalid")
}

fn metadata_invalid() -> OAuthError {
    OAuthError("connector_oauth_metadata_invalid")
}

fn token_invalid() -> OAuthError {
    OAuthError("connector_oauth_token_invalid")
}

fn sign_in_needed() -> OAuthError {
    OAuthError("connector_sign_in_needed")
}

fn from_mcp(error: McpError) -> OAuthError {
    match error {
        McpError::Refused(code) => OAuthError(code),
        McpError::Timeout => OAuthError("connector_timeout"),
        McpError::TooLarge => OAuthError("connector_reply_too_large"),
        McpError::Unauthorized { .. }
        | McpError::InsufficientScope { .. }
        | McpError::Vendor(_) => failed(),
    }
}

/// One sign-in waiting for the browser, held in engine memory only: where it ends, what
/// the answer must match, and the PKCE verifier, which never leaves the engine before the
/// token request.
struct Flow {
    root: PathBuf,
    state: String,
    connector: String,
    sha256: String,
    issuer: String,
    iss_promised: bool,
    token: url::Url,
    hosts: Vec<String>,
    client: Client,
    redirect: String,
    resource: String,
    scopes: Vec<String>,
    tries: u8,
    verifier: SecretString,
    started: Instant,
}

/// The client a sign-in uses: its id, where its secret is saved when it has one, whether
/// the token endpoint takes that secret in the body rather than a Basic header, and
/// whether it is the owner's own app.
struct Client {
    id: String,
    secret: Option<CredentialRef>,
    post: bool,
    own: bool,
}

/// What a renewal needs of a connector's last sign-in, saved after its tokens in the plugin
/// key store: the issuer, its token endpoint, the client and how it signs in, the resource
/// the tokens are for, the scopes it asked, how many step-ups in a row led to it, and when
/// the access token ends (Unix ms; `i64::MAX` for no end).
#[derive(Serialize, Deserialize)]
struct Record {
    issuer: String,
    token: String,
    client: String,
    secret: Option<String>,
    post: bool,
    resource: String,
    scopes: Vec<String>,
    tries: u8,
    expires_ms: i64,
}

static FLOWS: Mutex<Vec<Flow>> = Mutex::new(Vec::new());
/// Held while an access token is renewed or a code redeemed, so two calls never spend one
/// refresh token twice and a sign-in's saves never interleave with a renewal's.
static RENEWING: Mutex<()> = Mutex::new(());

fn flows() -> MutexGuard<'static, Vec<Flow>> {
    FLOWS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Keeps a sign-in until its answer: expired ones go first, then the oldest of the same
/// engine root once it already waits on `FLOWS_MAX`.
fn keep(flow: Flow) {
    let mut flows = flows();
    flows.retain(|kept| kept.started.elapsed() < FLOW_LIFE);
    let waiting = flows.iter().filter(|kept| kept.root == flow.root).count();
    if waiting >= FLOWS_MAX
        && let Some(oldest) = flows.iter().position(|kept| kept.root == flow.root)
    {
        flows.remove(oldest);
    }
    let root = flow.root.clone();
    flows.push(flow);
    assert!(flows.iter().filter(|kept| kept.root == root).count() <= FLOWS_MAX);
}

/// Takes the sign-in a state names out of memory, so each state is answered once whatever
/// the answer. Inputs: the engine root and the state. Output: the sign-in, or
/// `connector_oauth_state_unknown` for a state unknown, answered or expired.
fn take(root: &Path, state: &str) -> Result<Flow, OAuthError> {
    let mut flows = flows();
    flows.retain(|kept| kept.started.elapsed() < FLOW_LIFE);
    let at = flows
        .iter()
        .position(|kept| kept.root == root && kept.state == state)
        .ok_or(OAuthError("connector_oauth_state_unknown"))?;
    Ok(flows.remove(at))
}

fn random_text() -> Result<String, OAuthError> {
    let mut bytes = [0_u8; RANDOM_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| failed())?;
    let text = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    bytes.fill(0);
    assert_eq!(text.len(), 43);
    Ok(text)
}

/// The loopback address the browser comes back to (RFC 8252): plain http to `127.0.0.1`
/// at a port, path `/callback`, nothing else.
fn loopback_redirect(redirect: &url::Url) -> bool {
    redirect.scheme() == "http"
        && redirect.host() == Some(url::Host::Ipv4(Ipv4Addr::LOCALHOST))
        && redirect.port().is_some_and(|port| port != 0)
        && redirect.path() == "/callback"
        && redirect.query().is_none()
        && redirect.fragment().is_none()
        && redirect.username().is_empty()
        && redirect.password().is_none()
}

#[derive(Clone, Copy)]
enum Verb<'a> {
    Get,
    Json(&'a str),
    Form(&'a str),
}

/// One reply: its status, its `WWW-Authenticate` values and its body, zeroed when dropped
/// since a token reply holds tokens.
struct Reply {
    status: u16,
    challenges: Vec<String>,
    body: Vec<u8>,
}

impl Drop for Reply {
    fn drop(&mut self) {
        self.body.fill(0);
    }
}

impl Reply {
    fn object(&self) -> Option<Map<String, Value>> {
        match serde_json::from_slice(&self.body).ok()? {
            Value::Object(object) => Some(object),
            _ => None,
        }
    }
}

/// Sends one request through curl after the address check: the URL must be https to a
/// declared host (the rows' stand-in is plain http to `127.0.0.1`), and curl is pinned to
/// the public addresses looked up for it. A body goes only into the wiped config on
/// curl's stdin, never to disk, and so does a client's Basic sign-in. Blocking. Inputs:
/// the URL, its allowed hosts, the verb, the client id and secret for a Basic header, and
/// the lookup. Output: the reply; a redirect, an oversized reply and a timeout are errors.
fn request(
    url: &url::Url,
    hosts: &[String],
    verb: Verb<'_>,
    basic: Option<(&str, &str)>,
    reach: &ProviderReach,
) -> Result<Reply, OAuthError> {
    let loopback = guard::checked(url, hosts).map_err(from_mcp)?;
    let refused = || OAuthError("connector_address_refused");
    let host = url.host_str().ok_or_else(refused)?;
    let port = url.port_or_known_default().ok_or_else(refused)?;
    let pins = if loopback {
        None
    } else {
        Some(guard::pins(host, port, reach).map_err(from_mcp)?)
    };
    let mut config = Config::with_capacity(CONFIG_BYTES);
    http::push_line(&mut config, "url", url.as_str()).map_err(|_| failed())?;
    config.push(match verb {
        Verb::Get => "request = \"GET\"\nheader = \"Accept: application/json\"\n",
        Verb::Json(_) => concat!(
            "request = \"POST\"\nheader = \"Content-Type: application/json\"\n",
            "header = \"Accept: application/json, text/event-stream\"\n"
        ),
        Verb::Form(_) => concat!(
            "request = \"POST\"\nheader = \"Content-Type: application/x-www-form-urlencoded\"\n",
            "header = \"Accept: application/json\"\n"
        ),
    });
    config.push("header = \"Expect:\"\n");
    config.push(concat!(
        "header = \"User-Agent: CrazyAgent/",
        env!("CARGO_PKG_VERSION"),
        "\"\n"
    ));
    if let Some(pins) = &pins {
        http::push_line(&mut config, "resolve", pins).map_err(|_| failed())?;
    }
    if let Some((id, secret)) = basic {
        let mut user = String::with_capacity(3 * id.len());
        user.extend(url::form_urlencoded::byte_serialize(id.as_bytes()));
        let mut key = String::with_capacity(3 * secret.len());
        key.extend(url::form_urlencoded::byte_serialize(secret.as_bytes()));
        config.push("header = \"Authorization: Basic ");
        let pushed = http::push_basic(&mut config, &user, &key);
        let mut key = key.into_bytes();
        key.fill(0);
        pushed.map_err(|_| failed())?;
        config.push("\"\n");
    }
    if let Verb::Json(body) | Verb::Form(body) = verb {
        http::push_line(&mut config, "data-raw", body).map_err(|_| failed())?;
    }
    let scheme = if loopback { "=http" } else { "=https" };
    let mut raw: Vec<u8> = Vec::with_capacity(HEAD_BYTES_MAX + REPLY_BYTES_MAX);
    let mut over = false;
    let ended = http::stream(&mut wire::command(scheme), &config, &mut |chunk| {
        if raw.len() + chunk.len() > HEAD_BYTES_MAX + REPLY_BYTES_MAX {
            over = true;
            return false;
        }
        raw.extend_from_slice(chunk);
        true
    });
    drop(config);
    let reply = match ended {
        _ if over => Err(OAuthError("connector_reply_too_large")),
        Ok(Ended::Exit(Some(0))) => parse(&raw),
        Ok(Ended::Exit(Some(wire::CURL_TIMED_OUT))) => Err(OAuthError("connector_timeout")),
        _ => Err(failed()),
    };
    raw.fill(0);
    reply
}

/// Reads curl's `--include` output: informational heads are skipped, a redirect is
/// refused, and the final head gives the status and challenges. Input: the bytes. Output:
/// the reply, `connector_redirect_refused` or `connector_reply_invalid`.
fn parse(raw: &[u8]) -> Result<Reply, OAuthError> {
    let mut rest = raw;
    loop {
        let (end, gap) = wire::head_end(rest).ok_or_else(reply_invalid)?;
        if end > HEAD_BYTES_MAX {
            return Err(reply_invalid());
        }
        let mut lines = rest[..end]
            .split(|byte| *byte == b'\n')
            .map(|line| line.strip_suffix(b"\r").unwrap_or(line));
        let status = lines
            .next()
            .and_then(wire::status_of)
            .ok_or_else(reply_invalid)?;
        let after = &rest[end + gap..];
        if (100..200).contains(&status) {
            rest = after;
            continue;
        }
        if (300..400).contains(&status) {
            return Err(OAuthError("connector_redirect_refused"));
        }
        let mut challenges = Vec::new();
        for line in lines {
            let Ok(text) = std::str::from_utf8(line) else {
                continue;
            };
            if let Some((name, value)) = text.split_once(':')
                && name.trim().eq_ignore_ascii_case("www-authenticate")
                && challenges.len() < CHALLENGES_MAX
            {
                challenges.push(value.trim().to_owned());
            }
        }
        if after.len() > REPLY_BYTES_MAX {
            return Err(OAuthError("connector_reply_too_large"));
        }
        return Ok(Reply {
            status,
            challenges,
            body: after.to_vec(),
        });
    }
}

fn at(base: &url::Url, path: &str) -> url::Url {
    let mut place = base.clone();
    place.set_path(path);
    place.set_query(None);
    place.set_fragment(None);
    place
}

/// Whether a resource identifier names the MCP server: same scheme, host and port, no
/// user, query or fragment, and a path the server's path equals or sits under.
/// Whether a token for `resource` may go to every address of an MCP endpoint: its URL and,
/// when it has one, its read-only URL. Inputs: the resource and the endpoint. Output: true
/// when the resource covers each.
fn reaches(resource: &str, endpoint: &McpEndpoint) -> bool {
    let read_only = match &endpoint.read_only {
        ReadOnly::Url(url) => covers(resource, url),
        ReadOnly::None | ReadOnly::Header { .. } => true,
    };
    covers(resource, &endpoint.url) && read_only
}

fn covers(resource: &str, mcp: &url::Url) -> bool {
    let Ok(resource) = url::Url::parse(resource) else {
        return false;
    };
    let path = resource.path().trim_end_matches('/');
    resource.scheme() == mcp.scheme()
        && resource.host_str() == mcp.host_str()
        && resource.port_or_known_default() == mcp.port_or_known_default()
        && resource.username().is_empty()
        && resource.password().is_none()
        && resource.query().is_none()
        && resource.fragment().is_none()
        && (mcp.path() == path || mcp.path().starts_with(&format!("{path}/")))
}

/// Finds the MCP server's resource identifier and authorization server (RFC 9728): from
/// the `resource_metadata` its 401 names, else the well-known address under its path, then
/// at its root. Resource metadata is read only on a host the file declares. Inputs: the
/// connector, its OAuth block and the lookup. Output: the resource and the issuer.
fn protected_resource(
    spec: &ConnectorSpec,
    oauth: &OAuthSpec,
    reach: &ProviderReach,
) -> Result<(String, String), OAuthError> {
    let mcp = &spec.mcp;
    let mut declared = mcp.hosts.clone();
    for host in &oauth.hosts {
        if !declared.contains(host) {
            declared.push(host.clone());
        }
    }
    let probe = request(&mcp.url, &mcp.hosts, Verb::Json(PROBE), None, reach)?;
    let hint = (probe.status == 401)
        .then(|| {
            probe
                .challenges
                .iter()
                .flat_map(|challenge| wire::bearer(challenge))
                .find(|(name, _)| name == "resource_metadata")
                .map(|(_, value)| value)
        })
        .flatten();
    drop(probe);
    let places = match hint {
        Some(hint) => {
            vec![url::Url::parse(&hint).map_err(|_| OAuthError("connector_address_refused"))?]
        }
        None => {
            let path = mcp.url.path().trim_end_matches('/');
            let mut places = Vec::with_capacity(2);
            if !path.is_empty() {
                places.push(at(
                    &mcp.url,
                    &format!("/.well-known/oauth-protected-resource{path}"),
                ));
            }
            places.push(at(&mcp.url, "/.well-known/oauth-protected-resource"));
            places
        }
    };
    let mut found = None;
    for place in &places {
        let reply = request(place, &declared, Verb::Get, None, reach)?;
        if reply.status == 200
            && let Some(object) = reply.object()
        {
            found = Some(object);
            break;
        }
    }
    let metadata = found.ok_or_else(metadata_invalid)?;
    let resource = metadata
        .get("resource")
        .and_then(Value::as_str)
        .filter(|resource| reaches(resource, mcp))
        .ok_or_else(metadata_invalid)?;
    let issuer = metadata
        .get("authorization_servers")
        .and_then(Value::as_array)
        .and_then(|servers| servers.first())
        .and_then(Value::as_str)
        .ok_or_else(metadata_invalid)?;
    Ok((resource.to_owned(), issuer.to_owned()))
}

/// What a sign-in uses of the authorization server's metadata: its issuer, whether answers
/// carry `iss`, both endpoints, whether it takes client metadata documents, its
/// registration endpoint, and whether its token endpoint takes a secret only in the body.
struct Server {
    issuer: String,
    iss_promised: bool,
    authorize: url::Url,
    token: url::Url,
    cimd: bool,
    registration: Option<url::Url>,
    post_only: bool,
}

/// Reads the authorization server's metadata (RFC 8414, then OpenID) on a declared OAuth
/// host. Its `issuer` must be the one it was read for, both endpoints must be on declared
/// OAuth hosts and it must offer PKCE S256. Inputs: the issuer, the OAuth block and the
/// lookup. Output: the server, or why it cannot be used.
fn server(issuer: &str, oauth: &OAuthSpec, reach: &ProviderReach) -> Result<Server, OAuthError> {
    let base = url::Url::parse(issuer).map_err(|_| metadata_invalid())?;
    guard::checked(&base, &oauth.hosts).map_err(from_mcp)?;
    let path = base.path().trim_end_matches('/');
    let mut places = vec![
        at(
            &base,
            &format!("/.well-known/oauth-authorization-server{path}"),
        ),
        at(&base, &format!("/.well-known/openid-configuration{path}")),
    ];
    if !path.is_empty() {
        places.push(at(
            &base,
            &format!("{path}/.well-known/openid-configuration"),
        ));
    }
    let mut found = None;
    for place in &places {
        let reply = request(place, &oauth.hosts, Verb::Get, None, reach)?;
        if reply.status == 200
            && let Some(object) = reply.object()
        {
            found = Some(object);
            break;
        }
    }
    let metadata = found.ok_or_else(metadata_invalid)?;
    if metadata.get("issuer").and_then(Value::as_str) != Some(issuer) {
        return Err(metadata_invalid());
    }
    let endpoint = |key: &str| -> Result<url::Url, OAuthError> {
        let text = metadata
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(metadata_invalid)?;
        let url = url::Url::parse(text).map_err(|_| metadata_invalid())?;
        guard::checked(&url, &oauth.hosts).map_err(from_mcp)?;
        Ok(url)
    };
    let authorize = endpoint("authorization_endpoint")?;
    let token = endpoint("token_endpoint")?;
    let s256 = metadata
        .get("code_challenge_methods_supported")
        .and_then(Value::as_array)
        .is_some_and(|methods| methods.iter().any(|method| method == "S256"));
    if !s256 {
        return Err(OAuthError("connector_oauth_pkce_unsupported"));
    }
    let registration = match metadata.get("registration_endpoint") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let text = value.as_str().ok_or_else(metadata_invalid)?;
            Some(url::Url::parse(text).map_err(|_| metadata_invalid())?)
        }
    };
    let methods: Vec<&str> = metadata
        .get("token_endpoint_auth_methods_supported")
        .and_then(Value::as_array)
        .map(|methods| methods.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let yes = |key: &str| metadata.get(key) == Some(&Value::Bool(true));
    Ok(Server {
        issuer: issuer.to_owned(),
        iss_promised: yes("authorization_response_iss_parameter_supported"),
        authorize,
        token,
        cimd: yes("client_id_metadata_document_supported"),
        registration,
        post_only: methods.contains(&"client_secret_post")
            && !methods.contains(&"client_secret_basic"),
    })
}

fn saved(root: &Path, reference: &CredentialRef) -> Result<Option<SecretString>, OAuthError> {
    accounts::read_key_in(root, PLUGIN_KEYS, reference).map_err(|_| key_store())
}

/// The client a sign-in uses, in this order where the file allows each: the owner's own
/// OAuth app when saved, the app's client metadata document where the server takes one and
/// the build has its address, then a client registered at this issuer (RFC 7591).
/// Inputs: root, connector, OAuth block, server, document address, redirect and lookup.
/// Output: the client, or `connector_oauth_needs_own_app`.
fn client(
    root: &Path,
    name: &str,
    oauth: &OAuthSpec,
    server: &Server,
    document: Option<&url::Url>,
    redirect: &url::Url,
    reach: &ProviderReach,
) -> Result<Client, OAuthError> {
    let allows = |way: Registration| oauth.registration.contains(&way);
    if allows(Registration::Own)
        && let Some(id) = saved(root, &own_ref(name)?)?
    {
        let bound = accounts::read_bearer_in(root, PLUGIN_KEYS, &own_issuer_ref(name)?)
            .map_err(|_| key_store())?;
        if bound.is_some_and(|issuer| issuer.expose() != server.issuer) {
            return Err(OAuthError("connector_oauth_issuer_mismatch"));
        }
        let secret = own_secret_ref(name)?;
        let has_secret = saved(root, &secret)?.is_some();
        return Ok(Client {
            id: id.expose().to_owned(),
            secret: has_secret.then_some(secret),
            post: server.post_only,
            own: true,
        });
    }
    if allows(Registration::Cimd)
        && server.cimd
        && let Some(document) = document
    {
        return Ok(Client {
            id: document.as_str().to_owned(),
            secret: None,
            post: false,
            own: false,
        });
    }
    if allows(Registration::Dcr)
        && let Some(endpoint) = &server.registration
    {
        return registered(root, name, oauth, server, endpoint, redirect, reach);
    }
    Err(OAuthError("connector_oauth_needs_own_app"))
}

/// The client registered for this connector at this issuer: the one saved, or a new
/// public client registered once (RFC 7591) and saved, its secret too when the server
/// gives one. Inputs: root, connector, OAuth block, server, registration endpoint,
/// redirect and lookup. Output: the client, or why it could not be registered.
fn registered(
    root: &Path,
    name: &str,
    oauth: &OAuthSpec,
    server: &Server,
    endpoint: &url::Url,
    redirect: &url::Url,
    reach: &ProviderReach,
) -> Result<Client, OAuthError> {
    let id_ref = token_ref("client", name, &server.issuer)?;
    let secret_ref = token_ref("secret", name, &server.issuer)?;
    if let Some(id) = saved(root, &id_ref)? {
        let has_secret = saved(root, &secret_ref)?.is_some();
        return Ok(Client {
            id: id.expose().to_owned(),
            secret: has_secret.then_some(secret_ref),
            post: server.post_only,
            own: false,
        });
    }
    let body = json!({
        "client_name": "CrazyAgent",
        "redirect_uris": [redirect.as_str()],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    })
    .to_string();
    let reply = request(endpoint, &oauth.hosts, Verb::Json(&body), None, reach)?;
    if !matches!(reply.status, 200 | 201) {
        return Err(OAuthError("connector_oauth_refused"));
    }
    let made = reply.object().ok_or_else(metadata_invalid)?;
    let id = made
        .get("client_id")
        .and_then(Value::as_str)
        .filter(|id| visible(id, OWN_ID_BYTES_MAX))
        .ok_or_else(metadata_invalid)?;
    let secret = match made.get("client_secret") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_str()
                .filter(|secret| visible(secret, crate::credentials::SECRET_BYTES_MAX))
                .ok_or_else(metadata_invalid)?,
        ),
    };
    match secret {
        Some(secret) => {
            accounts::replace_key_in(root, PLUGIN_KEYS, &secret_ref, secret.as_bytes().to_vec())
        }
        None => accounts::remove_key_in(root, PLUGIN_KEYS, secret_ref.as_str()),
    }
    .map_err(|_| key_store())?;
    accounts::replace_key_in(root, PLUGIN_KEYS, &id_ref, id.as_bytes().to_vec())
        .map_err(|_| key_store())?;
    Ok(Client {
        id: id.to_owned(),
        secret: secret.is_some().then_some(secret_ref),
        post: server.post_only,
        own: false,
    })
}

/// Starts a sign-in with the connector's file given, as [`super::begin`] does for a trusted
/// one: the resource and server are found, the client picked, and the state and PKCE
/// verifier made and kept. Inputs: engine root, connector, its file's sha256 and spec, the
/// loopback redirect, whether writes are allowed and the lookup. Output: the pending sign-in.
#[allow(clippy::too_many_arguments)]
pub(crate) fn begin_with(
    root: &Path,
    name: &str,
    sha256: &str,
    spec: &ConnectorSpec,
    redirect: &url::Url,
    writes: bool,
    cimd: Option<&url::Url>,
    reach: &ProviderReach,
) -> Result<Pending, OAuthError> {
    let oauth = spec.oauth.as_ref().ok_or_else(not_offered)?;
    let scopes = declared(oauth, writes)
        .into_iter()
        .map(str::to_owned)
        .collect();
    begin_scoped(root, name, sha256, spec, redirect, scopes, 0, cimd, reach)
}

/// The scopes a sign-in may ask for: the file's read scopes, then its write scopes while
/// writes are allowed, each once, in the file's order.
fn declared(oauth: &OAuthSpec, writes: bool) -> Vec<&str> {
    let mut scopes: Vec<&str> =
        Vec::with_capacity(oauth.scopes_read.len() + oauth.scopes_write.len());
    let write = oauth.scopes_write.iter().filter(|_| writes);
    for scope in oauth.scopes_read.iter().chain(write) {
        if !scopes.contains(&scope.as_str()) {
            scopes.push(scope);
        }
    }
    scopes
}

/// Starts a sign-in for the scopes given: finds the authorization server, picks the
/// client and keeps the flow. Inputs: root, connector, the trusted file's sha256, its
/// declaration, the loopback address, the scopes, the step-ups in a row it is, the client
/// document address and the lookup. Output: the address to open and its state.
#[allow(clippy::too_many_arguments)]
fn begin_scoped(
    root: &Path,
    name: &str,
    sha256: &str,
    spec: &ConnectorSpec,
    redirect: &url::Url,
    scopes: Vec<String>,
    tries: u8,
    cimd: Option<&url::Url>,
    reach: &ProviderReach,
) -> Result<Pending, OAuthError> {
    assert!(
        tries <= STEP_UPS_MAX,
        "a step-up past the limit is refused before"
    );
    let oauth = spec.oauth.as_ref().ok_or_else(not_offered)?;
    if !loopback_redirect(redirect) {
        return Err(OAuthError("connector_oauth_redirect_invalid"));
    }
    let (resource, issuer) = protected_resource(spec, oauth, reach)?;
    let server = server(&issuer, oauth, reach)?;
    let client = client(root, name, oauth, &server, cimd, redirect, reach)?;
    let state = random_text()?;
    let verifier = random_text()?;
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let mut authorize_url = server.authorize.clone();
    {
        let mut query = authorize_url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", &client.id)
            .append_pair("redirect_uri", redirect.as_str());
        if !scopes.is_empty() {
            query.append_pair("scope", &scopes.join(" "));
        }
        query
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("resource", &resource);
    }
    let verifier = SecretString::new(verifier.into_bytes()).map_err(|_| failed())?;
    keep(Flow {
        root: root.to_path_buf(),
        state: state.clone(),
        connector: name.to_owned(),
        sha256: sha256.to_owned(),
        issuer: server.issuer,
        iss_promised: server.iss_promised,
        token: server.token,
        hosts: oauth.hosts.clone(),
        client,
        redirect: redirect.as_str().to_owned(),
        resource,
        scopes,
        tries,
        verifier,
        started: Instant::now(),
    });
    Ok(Pending {
        authorize_url,
        state,
    })
}

/// [`super::finish`]: the sign-in is taken, the connector's file must still be the one
/// trusted when it began, then the answer is redeemed. Inputs: database, engine root, the
/// answer's state, code and issuer, and the lookup. Output: Ok once the tokens are saved.
pub(super) fn finish_trusted(
    db: &Connection,
    root: &Path,
    state: &str,
    code: &str,
    iss: Option<&str>,
    reach: &ProviderReach,
) -> Result<(), OAuthError> {
    let flow = take(root, state)?;
    let trusted = store::trusted_connector(db, root, &flow.connector)
        .is_ok_and(|manifest| manifest.sha256 == flow.sha256);
    if !trusted {
        return Err(OAuthError("connector_oauth_trust_changed"));
    }
    let connector = flow.connector.clone();
    redeem(root, flow, code, iss, reach)?;
    signed_in(db, &connector)
}

/// [`finish_trusted`] without the trust read, for a sign-in begun with a file given.
#[cfg(test)]
pub(crate) fn finish_with(
    root: &Path,
    state: &str,
    code: &str,
    iss: Option<&str>,
    reach: &ProviderReach,
) -> Result<(), OAuthError> {
    redeem(root, take(root, state)?, code, iss, reach)
}

/// Where one token of a connector signed in at one issuer is saved: the kind, the
/// connector and the first 16 hex digits of the issuer's sha256.
fn token_ref(kind: &str, connector: &str, issuer: &str) -> Result<CredentialRef, OAuthError> {
    let digest = Sha256::digest(issuer.as_bytes());
    let mut short = String::with_capacity(16);
    for byte in &digest[..8] {
        short.push_str(&format!("{byte:02x}"));
    }
    assert_eq!(short.len(), 16);
    CredentialRef::new(format!("connector-{kind}-{connector}-{short}")).map_err(|_| key_store())
}

fn token_of<'a>(tokens: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, OAuthError> {
    match tokens.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .filter(|token| visible(token, TOKEN_BYTES_MAX))
            .map(Some)
            .ok_or_else(token_invalid),
    }
}

/// Adds a client's sign-in to a token request: its id in the body unless its secret goes in
/// a Basic header, and that secret in the body where the server takes only that. Inputs:
/// the form, the client and its secret. Output: the Basic id and secret, if any.
fn signed<'a>(
    form: &mut url::form_urlencoded::Serializer<'_, String>,
    client: &'a Client,
    secret: Option<&'a SecretString>,
) -> Option<(&'a str, &'a str)> {
    let basic = secret.filter(|_| !client.post);
    if basic.is_none() {
        form.append_pair("client_id", &client.id);
    }
    if client.post
        && let Some(secret) = secret
    {
        form.append_pair("client_secret", secret.expose());
    }
    basic.map(|secret| (client.id.as_str(), secret.expose()))
}

/// A client's secret read from where it is saved. Output: none for a public client, or
/// `connector_oauth_client_invalid` when a saved secret is gone.
fn secret_of(root: &Path, client: &Client) -> Result<Option<SecretString>, OAuthError> {
    match &client.secret {
        Some(reference) => {
            Some(saved(root, reference)?.ok_or(OAuthError("connector_oauth_client_invalid")))
                .transpose()
        }
        None => Ok(None),
    }
}

/// A token endpoint's answer (RFC 6749 5.1) as the engine keeps it: the Bearer access token,
/// the refresh token when one came, and when the access token ends.
struct Grant<'a> {
    access: &'a str,
    refresh: Option<&'a str>,
    ends_ms: i64,
}

/// Reads a token answer. Inputs: its object and the clock. Output: the grant, or
/// `connector_oauth_token_invalid` for a token of the wrong shape or kind or a lifetime
/// that is not one second to ten years.
fn grant(tokens: &Map<String, Value>, now_ms: i64) -> Result<Grant<'_>, OAuthError> {
    let access = token_of(tokens, "access_token")?.ok_or_else(token_invalid)?;
    let bearer = tokens
        .get("token_type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("bearer"));
    if !bearer {
        return Err(token_invalid());
    }
    let ends_ms = match tokens.get("expires_in") {
        None | Some(Value::Null) => i64::MAX,
        Some(value) => {
            let seconds = value
                .as_i64()
                .filter(|seconds| (1..=LIFETIME_MAX_S).contains(seconds))
                .ok_or_else(token_invalid)?;
            now_ms.saturating_add(seconds * 1000)
        }
    };
    Ok(Grant {
        access,
        refresh: token_of(tokens, "refresh_token")?,
        ends_ms,
    })
}

/// Saves a grant under the connector and issuer. A grant with no refresh token removes the
/// old one after a code, and keeps it after a renewal (RFC 6749 6). Inputs: root,
/// connector, issuer, grant and whether it renews. Output: Ok, or the key store's refusal.
fn keep_grant(
    root: &Path,
    name: &str,
    issuer: &str,
    grant: &Grant<'_>,
    renewing: bool,
) -> Result<(), OAuthError> {
    let save = |kind: &str, token: &str| -> Result<(), OAuthError> {
        accounts::replace_key_in(
            root,
            PLUGIN_KEYS,
            &token_ref(kind, name, issuer)?,
            token.as_bytes().to_vec(),
        )
        .map_err(|_| key_store())
    };
    save("token", grant.access)?;
    match (grant.refresh, renewing) {
        (Some(refresh), _) => save("refresh", refresh),
        (None, true) => Ok(()),
        (None, false) => accounts::remove_key_in(
            root,
            PLUGIN_KEYS,
            token_ref("refresh", name, issuer)?.as_str(),
        )
        .map_err(|_| key_store()),
    }
}

/// Ends a connector's sign-in after its renewal was refused: the refresh token, the
/// access token and the record are removed, so later calls ask the server nothing.
/// Inputs: root, connector and issuer. Output: Ok, or the key store's refusal.
fn ended(root: &Path, name: &str, issuer: &str) -> Result<(), OAuthError> {
    for reference in [
        token_ref("refresh", name, issuer)?,
        token_ref("token", name, issuer)?,
        record_ref(name)?,
    ] {
        accounts::remove_key_in(root, PLUGIN_KEYS, reference.as_str()).map_err(|_| key_store())?;
    }
    Ok(())
}

fn record_ref(name: &str) -> Result<CredentialRef, OAuthError> {
    CredentialRef::new(format!("connector-signin-{name}")).map_err(|_| key_store())
}

/// A connector's last sign-in. Output: the record, none when there is none or it cannot be
/// read as one, or the key store's refusal.
fn record_of(root: &Path, name: &str) -> Result<Option<Record>, OAuthError> {
    let text =
        accounts::read_bearer_in(root, PLUGIN_KEYS, &record_ref(name)?).map_err(|_| key_store())?;
    Ok(text.and_then(|text| serde_json::from_str(text.expose()).ok()))
}

/// A record as the key store keeps it: visible ASCII with no space, the shape every saved
/// key must have. Output: the text, or `connector_oauth_metadata_invalid`.
fn record_text(record: &Record) -> Result<String, OAuthError> {
    let text = serde_json::to_string(record).map_err(|_| metadata_invalid())?;
    if !visible(&text, TOKEN_BYTES_MAX) {
        return Err(metadata_invalid());
    }
    Ok(text)
}

fn save_record(root: &Path, name: &str, text: String) -> Result<(), OAuthError> {
    accounts::replace_key_in(root, PLUGIN_KEYS, &record_ref(name)?, text.into_bytes())
        .map_err(|_| key_store())
}

/// Redeems an answer's code (RFC 6749 4.1.3 with PKCE and the resource): the issuer is
/// checked first, so a code from another server is never sent anywhere. The request body
/// lives in a buffer that never grows and is zeroed. Under the renewal lock, the owner's
/// own app must still be bound to this issuer or to none; then the tokens are saved, the
/// tokens of another issuer the connector was signed in at before are removed, the owner's
/// own app is bound to this issuer, and the sign-in record is saved last. Inputs: engine root, the
/// sign-in, the code, the answer's issuer and the lookup. Output: Ok once all is saved.
fn redeem(
    root: &Path,
    flow: Flow,
    code: &str,
    iss: Option<&str>,
    reach: &ProviderReach,
) -> Result<(), OAuthError> {
    let mismatch = OAuthError("connector_oauth_issuer_mismatch");
    match iss {
        Some(iss) if iss != flow.issuer => return Err(mismatch),
        None if flow.iss_promised => return Err(mismatch),
        _ => {}
    }
    if !visible(code, CODE_BYTES_MAX) {
        return Err(OAuthError("connector_oauth_code_invalid"));
    }
    let _one = RENEWING.lock().unwrap_or_else(PoisonError::into_inner);
    if flow.client.own {
        let bound = accounts::read_bearer_in(root, PLUGIN_KEYS, &own_issuer_ref(&flow.connector)?)
            .map_err(|_| key_store())?;
        if bound.is_some_and(|issuer| issuer.expose() != flow.issuer) {
            return Err(mismatch);
        }
    }
    let secret = secret_of(root, &flow.client)?;
    let mut form = url::form_urlencoded::Serializer::new(String::with_capacity(FORM_BYTES));
    form.append_pair("grant_type", "authorization_code")
        .append_pair("code", code)
        .append_pair("redirect_uri", &flow.redirect)
        .append_pair("code_verifier", flow.verifier.expose());
    let basic = signed(&mut form, &flow.client, secret.as_ref());
    let body = form.append_pair("resource", &flow.resource).finish();
    let sent = if body.len() <= FORM_BYTES {
        request(&flow.token, &flow.hosts, Verb::Form(&body), basic, reach)
    } else {
        Err(OAuthError("connector_oauth_code_invalid"))
    };
    let mut body = body.into_bytes();
    body.fill(0);
    let reply = sent?;
    if reply.status != 200 {
        return Err(OAuthError("connector_oauth_refused"));
    }
    let tokens = reply.object().ok_or_else(token_invalid)?;
    let granted = grant(&tokens, now_ms())?;
    let text = record_text(&Record {
        issuer: flow.issuer.clone(),
        token: flow.token.as_str().to_owned(),
        client: flow.client.id.clone(),
        secret: flow
            .client
            .secret
            .as_ref()
            .map(|reference| reference.as_str().to_owned()),
        post: flow.client.post,
        resource: flow.resource.clone(),
        scopes: flow.scopes.clone(),
        tries: flow.tries,
        expires_ms: granted.ends_ms,
    })?;
    if let Some(before) = record_of(root, &flow.connector)?
        && before.issuer != flow.issuer
    {
        for kind in ["token", "refresh"] {
            let old = token_ref(kind, &flow.connector, &before.issuer)?;
            accounts::remove_key_in(root, PLUGIN_KEYS, old.as_str()).map_err(|_| key_store())?;
        }
    }
    keep_grant(root, &flow.connector, &flow.issuer, &granted, false)?;
    if flow.client.own {
        accounts::replace_key_in(
            root,
            PLUGIN_KEYS,
            &own_issuer_ref(&flow.connector)?,
            flow.issuer.as_bytes().to_vec(),
        )
        .map_err(|_| key_store())?;
    }
    save_record(root, &flow.connector, text)
}

/// Renews a connector's access token at the token endpoint of its sign-in (RFC 6749 6),
/// never one found again, so a refresh token only goes back where it came from, and only
/// while that endpoint is on a host the file declares. The body is sized for its values
/// so it never grows, and is zeroed. Inputs: root, connector, OAuth block, the record,
/// clock and lookup. Output: when the new token ends, or `connector_sign_in_needed`.
fn renew(
    root: &Path,
    name: &str,
    oauth: &OAuthSpec,
    record: &Record,
    now_ms: i64,
    reach: &ProviderReach,
) -> Result<i64, OAuthError> {
    let refresh = accounts::read_bearer_in(
        root,
        PLUGIN_KEYS,
        &token_ref("refresh", name, &record.issuer)?,
    )
    .map_err(|_| key_store())?
    .ok_or_else(sign_in_needed)?;
    let token = url::Url::parse(&record.token).map_err(|_| sign_in_needed())?;
    let client = Client {
        id: record.client.clone(),
        secret: record
            .secret
            .as_deref()
            .map(CredentialRef::new)
            .transpose()
            .map_err(|_| key_store())?,
        post: record.post,
        own: false,
    };
    let secret = secret_of(root, &client)?;
    let values = [
        refresh.expose(),
        client.id.as_str(),
        secret.as_ref().map_or("", SecretString::expose),
        record.resource.as_str(),
    ];
    let room = values.iter().map(|value| 3 * value.len()).sum::<usize>() + 128;
    let mut form = url::form_urlencoded::Serializer::new(String::with_capacity(room));
    form.append_pair("grant_type", "refresh_token")
        .append_pair("refresh_token", refresh.expose());
    let basic = signed(&mut form, &client, secret.as_ref());
    let body = form.append_pair("resource", &record.resource).finish();
    assert!(body.len() <= room, "the renewal body never grows");
    let sent = request(&token, &oauth.hosts, Verb::Form(&body), basic, reach);
    let mut body = body.into_bytes();
    body.fill(0);
    let reply = sent?;
    match reply.status {
        200 => {}
        400 | 401 => return Err(sign_in_needed()),
        _ => return Err(OAuthError("connector_oauth_refused")),
    }
    let tokens = reply.object().ok_or_else(token_invalid)?;
    let granted = grant(&tokens, now_ms)?;
    keep_grant(root, name, &record.issuer, &granted, true)?;
    Ok(granted.ends_ms)
}

/// The credential a call sends now for one connector as declared, its access token
/// renewed first at the token endpoint of its sign-in when it ends within a minute. A
/// sign-in for another resource than the one declared is not used. Blocking when it
/// renews. Inputs: database, root, connector, its title, its declaration, the clock (Unix
/// ms) and the lookup. Output: the credential, or `connector_sign_in_needed` and a
/// renewal's refusals.
pub(crate) fn fresh_with(
    db: &Connection,
    root: &Path,
    name: &str,
    title: &str,
    spec: &ConnectorSpec,
    now_ms: i64,
    reach: &ProviderReach,
) -> Result<Credential, OAuthError> {
    current(db, root, name, title, spec, now_ms, false, reach)
}

/// [`fresh_with`] with the access token renewed whatever its end, for a server that
/// refused it; a renewal or sign-in another call made since the record was read is used
/// instead of a second one. Inputs and outputs as there.
pub(crate) fn renew_with(
    db: &Connection,
    root: &Path,
    name: &str,
    title: &str,
    spec: &ConnectorSpec,
    now_ms: i64,
    reach: &ProviderReach,
) -> Result<Credential, OAuthError> {
    current(db, root, name, title, spec, now_ms, true, reach)
}

/// Ends a sign-in whose server refused the token just renewed for it, and raises its
/// Connector sign-in item, so later calls ask the vendor nothing until a new sign-in. A
/// sign-in another call renewed or began since, or a key, is left as it is. Inputs: the
/// database, root, connector, its title, the refused credential and the time. Output:
/// nothing, or the key store's or database's refusal.
pub(crate) fn refused_with(
    db: &Connection,
    root: &Path,
    name: &str,
    title: &str,
    refused: &Credential,
    now_ms: i64,
) -> Result<(), OAuthError> {
    let Credential::OAuth {
        issuer, expires_ms, ..
    } = refused
    else {
        return Ok(());
    };
    let _one = RENEWING.lock().unwrap_or_else(PoisonError::into_inner);
    let still = record_of(root, name)?
        .is_some_and(|record| record.issuer == *issuer && record.expires_ms == *expires_ms);
    if still {
        ended(root, name, issuer)?;
        mark_sign_in(db, name, title, now_ms)?;
    }
    Ok(())
}

/// The credential now, renewed when it ends within a minute or when `force` asks.
#[allow(clippy::too_many_arguments)]
fn current(
    db: &Connection,
    root: &Path,
    name: &str,
    title: &str,
    spec: &ConnectorSpec,
    now_ms: i64,
    force: bool,
    reach: &ProviderReach,
) -> Result<Credential, OAuthError> {
    let oauth = spec.oauth.as_ref().ok_or_else(not_offered)?;
    let usable = |record: Option<Record>| {
        record
            .filter(|record| reaches(&record.resource, &spec.mcp))
            .ok_or_else(sign_in_needed)
    };
    let ends_soon = |record: &Record| now_ms.saturating_add(RENEW_EARLY_MS) >= record.expires_ms;
    let mut record = usable(record_of(root, name)?)?;
    if force || ends_soon(&record) {
        let read_ends_ms = record.expires_ms;
        let _one = RENEWING.lock().unwrap_or_else(PoisonError::into_inner);
        record = usable(record_of(root, name)?)?;
        let renewed_since = record.expires_ms != read_ends_ms;
        if (force && !renewed_since) || (!force && ends_soon(&record)) {
            match renew(root, name, oauth, &record, now_ms, reach) {
                Ok(ends_ms) => {
                    record.expires_ms = ends_ms;
                    save_record(root, name, record_text(&record)?)?;
                }
                Err(refused) if refused == sign_in_needed() => {
                    ended(root, name, &record.issuer)?;
                    mark_sign_in(db, name, title, now_ms)?;
                    return Err(refused);
                }
                Err(other) => return Err(other),
            }
        }
    }
    let refresh = token_ref("refresh", name, &record.issuer)?;
    let renewable = accounts::key_saved_in(root, PLUGIN_KEYS, &refresh).map_err(|_| key_store())?;
    Ok(Credential::OAuth {
        access: token_ref("token", name, &record.issuer)?,
        refresh: renewable.then_some(refresh),
        issuer: record.issuer,
        expires_ms: record.expires_ms,
    })
}

/// [`super::step_up`] with the connector's file given. Inputs: root, connector, the
/// trusted file's sha256, its declaration, the challenge's scopes, the loopback address,
/// whether writes are allowed, the client document address and the lookup.
#[allow(clippy::too_many_arguments)]
pub(crate) fn step_up_with(
    root: &Path,
    name: &str,
    sha256: &str,
    spec: &ConnectorSpec,
    challenge: &str,
    redirect: &url::Url,
    writes: bool,
    cimd: Option<&url::Url>,
    reach: &ProviderReach,
) -> Result<Pending, OAuthError> {
    let oauth = spec.oauth.as_ref().ok_or_else(not_offered)?;
    let allowed = declared(oauth, writes);
    let asked: Vec<&str> = challenge
        .split(' ')
        .filter(|scope| !scope.is_empty())
        .collect();
    let undeclared = OAuthError("connector_oauth_scope_undeclared");
    if asked.is_empty()
        || asked.len() > SCOPES_MAX
        || !asked.iter().all(|scope| allowed.contains(scope))
    {
        return Err(undeclared);
    }
    let record = record_of(root, name)?;
    let tries = record.as_ref().map_or(0, |record| record.tries);
    if tries >= STEP_UPS_MAX {
        return Err(OAuthError("connector_oauth_step_up_limit"));
    }
    let before: Vec<&str> = match &record {
        Some(record) => record.scopes.iter().map(String::as_str).collect(),
        None => allowed.clone(),
    };
    let mut union: Vec<String> = Vec::with_capacity(allowed.len());
    for scope in before.iter().chain(&asked) {
        if allowed.contains(scope) && !union.iter().any(|kept| kept == scope) {
            union.push((*scope).to_owned());
        }
    }
    assert!(!union.is_empty() && union.len() <= allowed.len());
    begin_scoped(
        root,
        name,
        sha256,
        spec,
        redirect,
        union,
        tries + 1,
        cimd,
        reach,
    )
}
