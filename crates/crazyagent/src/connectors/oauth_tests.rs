use super::*;
use crate::app_plugins::connector::{OAuthSpec, Registration};
use crate::connectors::mcp_client::tests::{Seen, Vendor, empty, json_reply, no_lookup, send};
use crate::connectors::tests::{Case, add, case};
use crate::storage::accounts::PLUGIN_KEYS;
use base64::Engine;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const CLIENT: &str = "board-own-client-1";
const CODE: &str = "code-from-the-browser-1";
const ACCESS: &str = "at-board-0001";
const REFRESH: &str = "rt-board-0001";
const REDIRECT: &str = "http://127.0.0.1:53682/callback";
const SHA: &str = "sha256-of-the-trusted-board-file";
const REFUSED: &str = "connector_address_refused";

const BOARD_OAUTH: &str = "schema: 1
kind: connector
name: board
title: Board
version: 1
mcp:
  url: https://mcp.board.example/mcp
  hosts: [mcp.board.example]
oauth:
  hosts: [auth.board.example]
  registration: [own, cimd, dcr]
  scopes_read: [read]
  scopes_write: [write]
";

/// How the stand-in vendor answers: the `resource_metadata` its 401 names (none for a bare
/// challenge), the authorization servers its resource metadata lists, the two endpoints
/// its server metadata gives, and whether it promises `iss` in every answer. Each `{base}`
/// becomes the stand-in's own address.
#[derive(Clone)]
struct Shape {
    hint: Option<&'static str>,
    servers: Vec<&'static str>,
    authorize: &'static str,
    token: &'static str,
    iss: bool,
}

fn shape() -> Shape {
    Shape {
        hint: Some("{base}/.well-known/oauth-protected-resource/mcp"),
        servers: vec!["{base}"],
        authorize: "{base}/authorize",
        token: "{base}/token",
        iss: true,
    }
}

/// A loopback vendor that is both the MCP server and its authorization server, answering
/// as the shape says. Input: the shape. Output: the running stand-in.
fn vendor(shape: Shape) -> Vendor {
    Vendor::start(move |seen, _, stream| {
        let base = format!("http://{}", seen.header("host").unwrap_or_default());
        let fill = |text: &str| text.replace("{base}", &base);
        let reply = match (seen.verb.as_str(), seen.path.as_str()) {
            ("POST", "/mcp") => {
                let challenge = match shape.hint {
                    Some(hint) => {
                        format!(
                            "Bearer resource_metadata=\"{}\", scope=\"read\"",
                            fill(hint)
                        )
                    }
                    None => "Bearer scope=\"read\"".to_owned(),
                };
                empty(
                    "401 Unauthorized",
                    &[("WWW-Authenticate", challenge.as_str())],
                )
            }
            ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                let servers: Vec<String> = shape.servers.iter().copied().map(fill).collect();
                json_reply(
                    "200 OK",
                    &[],
                    &json!({"resource": fill("{base}/mcp"), "authorization_servers": servers}),
                )
            }
            ("GET", "/.well-known/oauth-authorization-server") => json_reply(
                "200 OK",
                &[],
                &json!({
                    "issuer": base,
                    "authorization_endpoint": fill(shape.authorize),
                    "token_endpoint": fill(shape.token),
                    "response_types_supported": ["code"],
                    "code_challenge_methods_supported": ["S256"],
                    "authorization_response_iss_parameter_supported": shape.iss,
                }),
            ),
            ("POST", "/token") => json_reply(
                "200 OK",
                &[("Cache-Control", "no-store")],
                &json!({
                    "access_token": ACCESS,
                    "token_type": "Bearer",
                    "expires_in": 3600,
                    "refresh_token": REFRESH,
                }),
            ),
            _ => empty("404 Not Found", &[]),
        };
        send(stream, &reply);
    })
}

struct Sign {
    case: Case,
    vendor: Vendor,
    spec: ConnectorSpec,
}

impl Sign {
    fn issuer(&self) -> String {
        format!("http://127.0.0.1:{}", self.vendor.port)
    }

    fn resource(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.vendor.port)
    }

    fn paths(&self) -> Vec<String> {
        self.vendor
            .seen()
            .iter()
            .map(|seen| format!("{} {}", seen.verb, seen.path))
            .collect()
    }

    fn token_requests(&self) -> Vec<Seen> {
        self.vendor
            .seen()
            .into_iter()
            .filter(|seen| seen.path == "/token")
            .collect()
    }
}

/// A real engine root with the trusted oauth-only `board` connector and the owner's own
/// client saved for it, signing in at a loopback stand-in. Inputs: the stand-in's shape and
/// the OAuth hosts declared. Output: the case, the stand-in and the connector as declared.
fn signing(shape: Shape, oauth_hosts: &[&str]) -> Sign {
    let case = case();
    add(&case, BOARD_OAUTH, false);
    assert_eq!(
        set_own_client(&case.db, &case.root, "board", CLIENT),
        Ok(())
    );
    let vendor = vendor(shape);
    let spec = ConnectorSpec {
        mcp: vendor.endpoint(),
        key: None,
        oauth: Some(OAuthSpec {
            hosts: oauth_hosts.iter().map(|host| (*host).to_owned()).collect(),
            registration: vec![Registration::Own],
            scopes_read: vec!["read".to_owned()],
            scopes_write: vec!["write".to_owned()],
        }),
        read_tools: Vec::new(),
    };
    Sign { case, vendor, spec }
}

fn redirect() -> url::Url {
    url::Url::parse(REDIRECT).unwrap()
}

fn start(sign: &Sign, writes: bool) -> Result<Pending, OAuthError> {
    begin_with(
        &sign.case.root,
        "board",
        SHA,
        &sign.spec,
        &redirect(),
        writes,
        &no_lookup(),
    )
}

fn begun(sign: &Sign, writes: bool) -> Pending {
    let begun = start(sign, writes);
    assert!(begun.is_ok(), "a declared sign-in begins: {begun:?}");
    begun.unwrap()
}

fn finished(sign: &Sign, state: &str, iss: Option<&str>) -> Result<(), OAuthError> {
    finish_with(&sign.case.root, state, CODE, iss, &no_lookup())
}

/// The values of the keys saved under one name prefix in `plugin-keys`, by name.
fn saved(case: &Case, prefix: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(case.root.join(PLUGIN_KEYS)) else {
        return Vec::new();
    };
    let mut found: Vec<(String, String)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            name.starts_with(prefix)
                .then(|| (name, std::fs::read_to_string(entry.path()).unwrap()))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, value)| value).collect()
}

fn query(url: &url::Url) -> BTreeMap<String, String> {
    url.query_pairs().into_owned().collect()
}

fn form(seen: &Seen) -> BTreeMap<String, String> {
    url::form_urlencoded::parse(seen.raw.as_bytes())
        .into_owned()
        .collect()
}

/// The smallest real check: an answer that names another issuer, or none where the server
/// promised one, never reaches the token endpoint and ends its sign-in.
#[test]
fn a_code_with_a_mismatched_iss_is_never_redeemed() {
    let sign = signing(shape(), &["127.0.0.1"]);
    let issuer = sign.issuer();
    let good = begun(&sign, false);
    assert_eq!(finished(&sign, &good.state, Some(&issuer)), Ok(()));
    assert_eq!(sign.token_requests().len(), 1);
    assert_eq!(
        saved(&sign.case, "connector-token-board-"),
        vec![ACCESS.to_owned()]
    );
    assert_eq!(
        finished(&sign, &good.state, Some(&issuer)),
        Err(OAuthError("connector_oauth_state_unknown")),
        "a state is answered once"
    );
    let slash = format!("{issuer}/");
    for iss in [
        Some("http://127.0.0.1:1"),
        Some(slash.as_str()),
        Some(""),
        None,
    ] {
        let pending = begun(&sign, false);
        assert_eq!(
            finished(&sign, &pending.state, iss),
            Err(OAuthError("connector_oauth_issuer_mismatch")),
            "{iss:?}"
        );
        assert_eq!(
            finished(&sign, &pending.state, Some(&issuer)),
            Err(OAuthError("connector_oauth_state_unknown")),
            "a refused answer ends its sign-in: {iss:?}"
        );
    }
    assert_eq!(
        sign.token_requests().len(),
        1,
        "no code met the token endpoint after a wrong or missing iss"
    );

    let quiet = signing(
        Shape {
            iss: false,
            ..shape()
        },
        &["127.0.0.1"],
    );
    let pending = begun(&quiet, false);
    assert_eq!(
        finished(&quiet, &pending.state, Some("http://127.0.0.1:1")),
        Err(OAuthError("connector_oauth_issuer_mismatch")),
        "an iss given is checked where the server promised none"
    );
    let pending = begun(&quiet, false);
    assert_eq!(finished(&quiet, &pending.state, None), Ok(()));
    assert_eq!(quiet.token_requests().len(), 1);
}

/// The authorization request carries the S256 challenge of the verifier the token request
/// then sends, and both name the MCP server as the resource, the same redirect and client.
#[test]
fn pkce_s256_and_resource_are_in_both_requests() {
    let sign = signing(shape(), &["127.0.0.1"]);
    let pending = begun(&sign, true);
    let asked = query(&pending.authorize_url);
    assert_eq!(pending.authorize_url.host_str(), Some("127.0.0.1"));
    assert_eq!(pending.authorize_url.port(), Some(sign.vendor.port));
    assert_eq!(pending.authorize_url.path(), "/authorize");
    assert_eq!(asked["response_type"], "code");
    assert_eq!(asked["client_id"], CLIENT);
    assert_eq!(asked["redirect_uri"], REDIRECT);
    assert_eq!(asked["state"], pending.state);
    assert_eq!(asked["code_challenge_method"], "S256");
    assert_eq!(asked["resource"], sign.resource());
    assert_eq!(asked["scope"], "read write");
    assert!(pending.state.len() >= 43, "a state of 256 random bits");
    let probe = &sign.vendor.seen()[0];
    assert_eq!((probe.verb.as_str(), probe.path.as_str()), ("POST", "/mcp"));
    assert_eq!(probe.header("authorization"), None);

    assert_eq!(
        finished(&sign, &pending.state, Some(&sign.issuer())),
        Ok(())
    );
    let tokens = sign.token_requests();
    assert_eq!(tokens.len(), 1);
    let sent = form(&tokens[0]);
    assert_eq!(sent["grant_type"], "authorization_code");
    assert_eq!(sent["code"], CODE);
    assert_eq!(sent["redirect_uri"], REDIRECT);
    assert_eq!(sent["client_id"], CLIENT);
    assert_eq!(sent["resource"], sign.resource());
    let verifier = &sent["code_verifier"];
    let unreserved = |byte: u8| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte);
    assert!((43..=128).contains(&verifier.len()));
    assert!(verifier.bytes().all(unreserved));
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    assert_eq!(asked["code_challenge"], challenge);
    assert_eq!(
        tokens[0].header("content-type"),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(tokens[0].header("authorization"), None);
    assert_eq!(
        saved(&sign.case, "connector-refresh-board-"),
        vec![REFRESH.to_owned()]
    );

    let again = query(&begun(&sign, false).authorize_url);
    assert_ne!(again["state"], asked["state"]);
    assert_ne!(again["code_challenge"], asked["code_challenge"]);
    assert_eq!(
        again["scope"], "read",
        "writes off asks for the read scopes only"
    );
}

/// Every address of a sign-in is checked before it is used: resource metadata on a host
/// the file declares, and the authorization server and both its endpoints on a declared
/// OAuth host. A refused one ends the sign-in before anything is sent there.
#[test]
fn an_authorization_url_on_an_undeclared_host_is_refused() {
    let sign = signing(shape(), &["127.0.0.1"]);
    assert_eq!(
        begun(&sign, false).authorize_url.host_str(),
        Some("127.0.0.1")
    );
    let bare = signing(
        Shape {
            hint: None,
            ..shape()
        },
        &["127.0.0.1"],
    );
    assert_eq!(
        begun(&bare, false).authorize_url.host_str(),
        Some("127.0.0.1")
    );
    assert_eq!(
        bare.paths(),
        vec![
            "POST /mcp",
            "GET /.well-known/oauth-protected-resource/mcp",
            "GET /.well-known/oauth-authorization-server",
        ],
        "with no hint the resource metadata is read at the MCP host's well-known address"
    );

    let evil = "https://evil.example";
    for (bent, hosts, reached) in [
        (
            Shape {
                authorize: "https://evil.example/authorize",
                ..shape()
            },
            &["127.0.0.1"][..],
            3,
        ),
        (
            Shape {
                token: "https://evil.example/token",
                ..shape()
            },
            &["127.0.0.1"][..],
            3,
        ),
        (
            Shape {
                servers: vec![evil],
                ..shape()
            },
            &["127.0.0.1"][..],
            2,
        ),
        (
            Shape {
                hint: Some("https://evil.example/.well-known/oauth-protected-resource"),
                ..shape()
            },
            &["127.0.0.1"][..],
            1,
        ),
        (shape(), &["auth.board.example"][..], 2),
    ] {
        let sign = signing(bent, hosts);
        let refused = start(&sign, false);
        assert_eq!(refused, Err(OAuthError(REFUSED)), "{:?}", sign.paths());
        assert_eq!(sign.vendor.seen().len(), reached, "{:?}", sign.paths());
    }
}
