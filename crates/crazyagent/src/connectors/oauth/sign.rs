use super::{OAuthError, Pending, key_store, not_offered, own_ref, visible};
use crate::app_plugins::connector::{ConnectorSpec, OAuthSpec, Registration};
use crate::app_plugins::http::{self, Config, Ended};
use crate::app_plugins::store;
use crate::connectors::mcp_client::{McpError, guard, wire};
use crate::credentials::SecretString;
use crate::protocol::CredentialRef;
use crate::provider_reach::ProviderReach;
use crate::storage::accounts::{self, PLUGIN_KEYS};
use base64::Engine;
use rusqlite::Connection;
use serde_json::{Map, Value};
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

fn from_mcp(error: McpError) -> OAuthError {
    match error {
        McpError::Refused(code) => OAuthError(code),
        McpError::Timeout => OAuthError("connector_timeout"),
        McpError::TooLarge => OAuthError("connector_reply_too_large"),
        McpError::Unauthorized { .. } | McpError::Vendor(_) => failed(),
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
    client_id: String,
    redirect: String,
    resource: String,
    verifier: SecretString,
    started: Instant,
}

static FLOWS: Mutex<Vec<Flow>> = Mutex::new(Vec::new());

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
/// curl's stdin, never to disk. Blocking. Inputs: the URL, its allowed hosts, the verb and
/// the lookup. Output: the reply; a redirect, an oversized reply and a timeout are errors.
fn request(
    url: &url::Url,
    hosts: &[String],
    verb: Verb<'_>,
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
    let probe = request(&mcp.url, &mcp.hosts, Verb::Json(PROBE), reach)?;
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
        let reply = request(place, &declared, Verb::Get, reach)?;
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
        .filter(|resource| covers(resource, &mcp.url))
        .ok_or_else(metadata_invalid)?;
    let issuer = metadata
        .get("authorization_servers")
        .and_then(Value::as_array)
        .and_then(|servers| servers.first())
        .and_then(Value::as_str)
        .ok_or_else(metadata_invalid)?;
    Ok((resource.to_owned(), issuer.to_owned()))
}

/// What a sign-in uses of the authorization server's metadata.
struct Server {
    issuer: String,
    iss_promised: bool,
    authorize: url::Url,
    token: url::Url,
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
        let reply = request(place, &oauth.hosts, Verb::Get, reach)?;
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
    Ok(Server {
        issuer: issuer.to_owned(),
        iss_promised: metadata.get("authorization_response_iss_parameter_supported")
            == Some(&Value::Bool(true)),
        authorize,
        token,
    })
}

/// The client id a sign-in uses: the owner's own OAuth app where the file lets the owner
/// bring one and one is saved. Inputs: the engine root, connector and OAuth block.
/// Output: the id, or `connector_oauth_needs_own_app`.
fn client(root: &Path, name: &str, oauth: &OAuthSpec) -> Result<String, OAuthError> {
    if oauth.registration.contains(&Registration::Own)
        && let Some(id) =
            accounts::read_key_in(root, PLUGIN_KEYS, &own_ref(name)?).map_err(|_| key_store())?
    {
        return Ok(id.expose().to_owned());
    }
    Err(OAuthError("connector_oauth_needs_own_app"))
}

/// Starts a sign-in with the connector's file given, as [`super::begin`] does for a trusted
/// one: the resource and server are found, the client picked, and the state and PKCE
/// verifier made and kept. Inputs: engine root, connector, its file's sha256 and spec, the
/// loopback redirect, whether writes are allowed and the lookup. Output: the pending sign-in.
pub(crate) fn begin_with(
    root: &Path,
    name: &str,
    sha256: &str,
    spec: &ConnectorSpec,
    redirect: &url::Url,
    writes: bool,
    reach: &ProviderReach,
) -> Result<Pending, OAuthError> {
    let oauth = spec.oauth.as_ref().ok_or_else(not_offered)?;
    if !loopback_redirect(redirect) {
        return Err(OAuthError("connector_oauth_redirect_invalid"));
    }
    let (resource, issuer) = protected_resource(spec, oauth, reach)?;
    let server = server(&issuer, oauth, reach)?;
    let client_id = client(root, name, oauth)?;
    let mut scopes: Vec<&str> = oauth.scopes_read.iter().map(String::as_str).collect();
    if writes {
        for scope in &oauth.scopes_write {
            if !scopes.contains(&scope.as_str()) {
                scopes.push(scope);
            }
        }
    }
    let state = random_text()?;
    let verifier = random_text()?;
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let mut authorize_url = server.authorize.clone();
    {
        let mut query = authorize_url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", &client_id)
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
        client_id,
        redirect: redirect.as_str().to_owned(),
        resource,
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
    redeem(root, flow, code, iss, reach)
}

/// [`finish_trusted`] without the trust read, for a sign-in begun with a file given.
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

/// Redeems an answer's code (RFC 6749 4.1.3 with PKCE and the resource): the issuer is
/// checked first, so a code from another server is never sent anywhere. The request body
/// lives in a buffer that never grows and is zeroed. Inputs: engine root, the sign-in, the
/// code, the answer's issuer and the lookup. Output: Ok once the tokens are saved.
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
    let body = url::form_urlencoded::Serializer::new(String::with_capacity(FORM_BYTES))
        .append_pair("grant_type", "authorization_code")
        .append_pair("code", code)
        .append_pair("redirect_uri", &flow.redirect)
        .append_pair("code_verifier", flow.verifier.expose())
        .append_pair("client_id", &flow.client_id)
        .append_pair("resource", &flow.resource)
        .finish();
    let sent = if body.len() <= FORM_BYTES {
        request(&flow.token, &flow.hosts, Verb::Form(&body), reach)
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
    let access = token_of(&tokens, "access_token")?.ok_or_else(token_invalid)?;
    let bearer = tokens
        .get("token_type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("bearer"));
    if !bearer {
        return Err(token_invalid());
    }
    let refresh = token_of(&tokens, "refresh_token")?;
    let save = |kind: &str, token: &str| -> Result<(), OAuthError> {
        accounts::replace_key_in(
            root,
            PLUGIN_KEYS,
            &token_ref(kind, &flow.connector, &flow.issuer)?,
            token.as_bytes().to_vec(),
        )
        .map_err(|_| key_store())
    };
    save("token", access)?;
    match refresh {
        Some(refresh) => save("refresh", refresh),
        None => accounts::remove_key_in(
            root,
            PLUGIN_KEYS,
            token_ref("refresh", &flow.connector, &flow.issuer)?.as_str(),
        )
        .map_err(|_| key_store()),
    }
}
