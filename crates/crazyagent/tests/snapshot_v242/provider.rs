use super::Scenario;
use super::e2e_support::{Engine, Owner, profile};
use crazyagent::accounts::{AccountDraft, Provider, SignInMethod};
use crazyagent::engine::owner::{AccountSecret, OwnerResult};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Every provider the account screens offer, in display order.
const PROVIDERS: [Provider; 7] = [
    Provider::Chatgpt,
    Provider::Claude,
    Provider::Openrouter,
    Provider::OpenaiApi,
    Provider::AnthropicApi,
    Provider::OpenaiCompatible,
    Provider::Local,
];

/// Every sign-in method a draft can name.
const METHODS: [SignInMethod; 3] = [
    SignInMethod::Subscription,
    SignInMethod::ApiKey,
    SignInMethod::Url,
];

/// The fixed identity nonce every draft is built with, so a capture never depends on a
/// random one: 26 hex digits, of which a build takes the first 20.
const NONCE: &str = "0123456789abcdef0123456789";

/// The model a draft that asks for one names.
const MODEL: &str = "snapshot-model";

/// An OpenAI-compatible address outside this machine, on a port no metadata service uses.
const REMOTE_URL: &str = "https://models.example.invalid/v1";

/// Lists the provider scenarios: the catalog, what each offered provider and method stores,
/// the refusals and the base URL each harness is handed. Output: the scenarios in order.
pub(super) fn scenarios() -> Vec<Scenario> {
    let scenario = |name: &'static str, capture: fn(&Path) -> Result<Value, String>| Scenario {
        kind: "provider",
        name,
        capture,
    };
    vec![
        scenario("catalog", catalog),
        scenario("account-drafts", account_drafts),
        scenario("account-draft-refusals", account_draft_refusals),
        scenario("endpoint-base-url", endpoint_base_url),
        scenario("request-local-models", request_local_models),
        scenario(
            "request-local-models-fallback",
            request_local_models_fallback,
        ),
        scenario("request-check-compatible-key", request_check_compatible_key),
    ]
}

/// Ollama's own address, the preset of a Local account; its port is masked in the catalog,
/// so the catalog capture states whether the preset is still exactly this.
const OLLAMA_DEFAULT: &str = "http://127.0.0.1:11434/v1";

/// Captures the provider catalog every client renders, and whether the Local preset is still
/// Ollama's own address, since the runner masks loopback ports. Input: the scratch folder,
/// unused. Output: the catalog and that fact.
fn catalog(_scratch: &Path) -> Result<Value, String> {
    let entries = crazyagent::accounts::catalog();
    assert_eq!(entries.len(), PROVIDERS.len());
    let local_preset_is_ollama_default = entries
        .iter()
        .filter(|entry| entry.provider == Provider::Local)
        .flat_map(|entry| entry.methods.iter())
        .all(|method| method.url_preset.as_deref() == Some(OLLAMA_DEFAULT));
    Ok(json!({
        "entries": serde_json::to_value(entries).map_err(|error| error.to_string())?,
        "local_preset_is_ollama_default": local_preset_is_ollama_default,
    }))
}

/// The serde name of a provider or method, as the wire carries it. Input: the value.
/// Output: the name.
fn wire_name(value: impl serde::Serialize) -> Result<String, String> {
    match serde_json::to_value(value).map_err(|error| error.to_string())? {
        Value::String(name) => Ok(name),
        other => Err(format!("a wire name is not text: {other}")),
    }
}

/// A draft with only a provider and method set. Inputs: both. Output: the draft.
fn bare(provider: Provider, method: SignInMethod) -> AccountDraft {
    AccountDraft {
        provider,
        method,
        name: String::new(),
        url: None,
        model: None,
    }
}

/// Builds one draft and captures what it stores, or the code it is refused with.
/// Inputs: the draft and whether a key comes with it. Output: the connection and details,
/// or `{"refused": code}`.
fn built(draft: &AccountDraft, key: bool) -> Result<Value, String> {
    Ok(match draft.build(key, NONCE) {
        Ok((connection, details)) => json!({
            "connection": serde_json::to_value(connection).map_err(|error| error.to_string())?,
            "details": serde_json::to_value(details).map_err(|error| error.to_string())?,
        }),
        Err(error) => json!({"refused": error.0}),
    })
}

/// The draft the account screen sends for one offered provider and method, with the fields
/// that method asks for filled. Inputs: the provider and method. Output: the draft and
/// whether a key comes with it.
fn offered(provider: Provider, method: SignInMethod) -> (AccountDraft, bool) {
    let mut draft = bare(provider, method);
    let keyed = method == SignInMethod::ApiKey;
    if method != SignInMethod::Subscription {
        draft.model = Some(MODEL.to_owned());
    }
    if provider == Provider::OpenaiCompatible {
        draft.url = Some(REMOTE_URL.to_owned());
    }
    (draft, keyed)
}

/// Captures what each provider and method the catalog offers stores: the model connection
/// (harness, protocol, endpoint, authentication) and the account details. Input: the scratch
/// folder, unused. Output: one entry per offered pair, in catalog order.
fn account_drafts(_scratch: &Path) -> Result<Value, String> {
    let mut entries = Vec::new();
    for entry in crazyagent::accounts::catalog() {
        for method in entry.methods {
            let (draft, key) = offered(entry.provider, method.method);
            entries.push(json!({
                "provider": wire_name(entry.provider)?,
                "method": wire_name(method.method)?,
                "stored": built(&draft, key)?,
            }));
        }
    }
    assert!(!entries.is_empty());
    Ok(Value::Array(entries))
}

/// Captures how every provider and method pair answers a bare draft, offered or not, and
/// the code each field rule refuses with. Input: the scratch folder, unused. Output: the
/// pairs as a list and the field cases by name.
fn account_draft_refusals(_scratch: &Path) -> Result<Value, String> {
    let mut pairs = Vec::new();
    for provider in PROVIDERS {
        for method in METHODS {
            pairs.push(json!({
                "provider": wire_name(provider)?,
                "method": wire_name(method)?,
                "bare": built(&bare(provider, method), false)?,
            }));
        }
    }
    let with = |provider, method, change: fn(&mut AccountDraft)| {
        let mut draft = offered(provider, method).0;
        change(&mut draft);
        draft
    };
    let mut fields = serde_json::Map::new();
    let mut note = |case: &str, value: Value| {
        fields.insert(case.to_owned(), value);
    };
    let url_on_subscription = with(Provider::Chatgpt, SignInMethod::Subscription, |draft| {
        draft.url = Some(REMOTE_URL.to_owned())
    });
    note("url-on-subscription", built(&url_on_subscription, false)?);
    let model_on_subscription = with(Provider::Claude, SignInMethod::Subscription, |draft| {
        draft.model = Some(MODEL.to_owned())
    });
    note(
        "model-on-subscription",
        built(&model_on_subscription, false)?,
    );
    let subscription = offered(Provider::Chatgpt, SignInMethod::Subscription).0;
    note("key-on-subscription", built(&subscription, true)?);
    let api_key = offered(Provider::OpenaiApi, SignInMethod::ApiKey).0;
    note("api-key-without-key", built(&api_key, false)?);
    let no_model = with(Provider::Openrouter, SignInMethod::ApiKey, |draft| {
        draft.model = None
    });
    note("api-key-without-model", built(&no_model, true)?);
    let bad_model = with(Provider::AnthropicApi, SignInMethod::ApiKey, |draft| {
        draft.model = Some("model with spaces".to_owned())
    });
    note("model-invalid", built(&bad_model, true)?);
    let local_remote = with(Provider::Local, SignInMethod::Url, |draft| {
        draft.url = Some(REMOTE_URL.to_owned())
    });
    note("local-url-not-loopback", built(&local_remote, false)?);
    let plain_http = with(Provider::OpenaiCompatible, SignInMethod::Url, |draft| {
        draft.url = Some("http://models.example.invalid/v1".to_owned())
    });
    note("compatible-plain-http-remote", built(&plain_http, false)?);
    let metadata = with(Provider::OpenaiCompatible, SignInMethod::Url, |draft| {
        draft.url = Some("https://models.example.invalid:988/v1".to_owned())
    });
    note("compatible-metadata-port", built(&metadata, false)?);
    let control_name = with(Provider::Claude, SignInMethod::ApiKey, |draft| {
        draft.name = "Team\u{202e}key".to_owned()
    });
    note("name-with-direction-control", built(&control_name, true)?);
    let (offered_draft, _) = offered(Provider::Chatgpt, SignInMethod::Subscription);
    note(
        "nonce-too-short",
        match offered_draft.build(false, "0123") {
            Ok(_) => json!("built"),
            Err(error) => json!({"refused": error.0}),
        },
    );
    Ok(json!({"pairs": pairs, "fields": fields}))
}

/// Captures the base URL each harness is handed for an endpoint account, over stored URLs
/// with and without `/v1` and a trailing `/`, none on loopback, whose port the runner masks.
/// Input: the scratch folder, unused. Output: one entry per harness name and URL.
fn endpoint_base_url(_scratch: &Path) -> Result<Value, String> {
    let urls = [
        "https://api.anthropic.com/v1",
        "https://api.anthropic.com/v1/",
        "https://api.openai.com/v1",
        "https://openrouter.ai/api/v1",
        "https://models.example.invalid:8443/v1",
        "https://models.example.invalid:8443",
    ];
    let names = crazyagent::harnesses::specs()
        .iter()
        .map(|spec| spec.id)
        .chain(["claude"]);
    let mut entries = Vec::new();
    for name in names {
        for url in urls {
            entries.push(json!({
                "harness": name,
                "stored": url,
                "handed": crazyagent::harnesses::endpoint_base_url(name, url),
            }));
        }
    }
    Ok(Value::Array(entries))
}

/// A key that is no real provider's and starts with no token prefix the golden writer refuses.
const DUMMY_KEY: &str = "snapshot-dummy-provider-key-0001";

/// The most requests one loopback recorder answers.
const HEARD_MAX: usize = 8;

/// The longest request head a loopback recorder reads.
const HEAD_BYTES_MAX: usize = 16 * 1024;

/// How long a loopback recorder waits for one request's bytes.
const HEAD_WAIT: Duration = Duration::from_secs(10);

/// A loopback model server standing in for a provider: it answers `/api/tags` (when told to)
/// and `*/models` with one model and records each request's line, its header names in the
/// order sent and, for an Authorization header, its scheme word and whether its token is the
/// dummy key, never the token. Dropping it leaves its thread blocked on a port no one dials.
struct Recorder {
    port: u16,
    heard: Arc<Mutex<Vec<Value>>>,
}

/// What one request head says, as a recorder keeps it. Input: the head text and whether
/// Ollama's `/api/tags` answers. Output: the record and the reply to send.
fn heard(head: &str, tags: bool) -> (Value, String) {
    let mut lines = head.split("\r\n");
    let line = lines.next().unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    let mut authorization = Value::Null;
    for field in lines.take_while(|field| !field.is_empty()) {
        let (name, value) = field.split_once(':').unwrap_or((field, ""));
        let name = name.trim().to_ascii_lowercase();
        if name == "authorization" {
            let (scheme, token) = value.trim().split_once(' ').unwrap_or((value.trim(), ""));
            authorization = json!({"scheme": scheme, "token_is_the_key": token == DUMMY_KEY});
        }
        headers.push(Value::String(name));
    }
    let path = line.split(' ').nth(1).unwrap_or_default();
    let body = if tags && path == "/api/tags" {
        Some(json!({"models": [{"name": MODEL}]}))
    } else if path.ends_with("/models") {
        Some(json!({"object": "list", "data": [{"id": MODEL}]}))
    } else {
        None
    };
    let reply = match body {
        Some(body) => {
            let text = body.to_string();
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            )
        }
        None => {
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
        }
    };
    let record = json!({"line": line, "headers": headers, "authorization": authorization});
    (record, reply)
}

/// Reads one request head from a connection, at most [`HEAD_BYTES_MAX`] bytes.
/// Input: the connection. Output: the head text, lossily decoded.
fn read_head(stream: &mut std::net::TcpStream) -> String {
    let mut head = Vec::new();
    let mut chunk = [0_u8; 1024];
    while head.len() < HEAD_BYTES_MAX && !head.windows(4).any(|end| end == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => head.extend_from_slice(&chunk[..read]),
        }
    }
    head.truncate(HEAD_BYTES_MAX);
    String::from_utf8_lossy(&head).into_owned()
}

impl Recorder {
    /// Starts the recorder on a free loopback port. Input: whether `/api/tags` answers.
    /// Output: the recorder, or why its port could not be opened.
    fn start(tags: bool) -> Result<Self, String> {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
        let port = listener
            .local_addr()
            .map_err(|error| error.to_string())?
            .port();
        let heard_list = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&heard_list);
        std::thread::spawn(move || {
            for stream in listener.incoming().take(HEARD_MAX) {
                let Ok(mut stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(HEAD_WAIT));
                let (record, reply) = heard(&read_head(&mut stream), tags);
                kept.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(record);
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        assert!(port > 0);
        Ok(Self {
            port,
            heard: heard_list,
        })
    }

    /// The requests heard so far, in order. Output: their records.
    fn requests(&self) -> Vec<Value> {
        self.heard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// Starts a real engine whose HOME is a private folder below the scratch folder and saves
/// the owner profile. Input: the scratch folder. Output: the running engine, or why its HOME
/// could not be made.
fn engine(scratch: &Path) -> Result<Engine, String> {
    let home = scratch
        .canonicalize()
        .map_err(|error| error.to_string())?
        .join("home");
    std::fs::create_dir(&home).map_err(|error| error.to_string())?;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    let mut engine = Engine::new("ca-snap-p2-");
    engine.home = Some(home);
    engine.launch();
    Ok(engine)
}

/// Runs owner steps against the engine as the TUI client on a runtime of its own, then
/// detaches. Inputs: the engine and the steps, which hand the owner back with their result.
/// Output: the steps' result.
fn with_owner<T, F, Fut>(engine: &Engine, steps: F) -> Result<T, String>
where
    F: FnOnce(Owner) -> Fut,
    Fut: std::future::Future<Output = (Owner, Result<T, String>)>,
{
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let mut owner = Owner::attach(engine).await;
        assert_eq!(owner.save_profile(profile()).await, OwnerResult::Applied);
        let (mut owner, result) = steps(owner).await;
        owner.controller.detach();
        result
    })
}

/// Captures the requests the engine sends to list a local model server's models and the
/// models it reads back. Inputs: the scratch folder and whether `/api/tags` answers, so the
/// second request shape shows. Output: the answer and the requests heard.
fn local_models(scratch: &Path, tags: bool) -> Result<Value, String> {
    let recorder = Recorder::start(tags)?;
    let engine = engine(scratch)?;
    let url = format!("http://127.0.0.1:{}", recorder.port);
    let answer = with_owner(&engine, |owner| async move {
        let answer = match owner.controller.local_models(url).await {
            Ok(models) => json!({"models": models}),
            Err(error) => json!({"refused": error.0}),
        };
        (owner, Ok(answer))
    })?;
    Ok(json!({"answer": answer, "requests": recorder.requests()}))
}

fn request_local_models(scratch: &Path) -> Result<Value, String> {
    local_models(scratch, true)
}

fn request_local_models_fallback(scratch: &Path) -> Result<Value, String> {
    local_models(scratch, false)
}

/// Captures what adding an OpenAI-compatible account with a key at a loopback address
/// stores and the request its check sends, with the key's place and scheme but never the
/// key. Input: the scratch folder. Output: the added account, the check and the requests.
fn request_check_compatible_key(scratch: &Path) -> Result<Value, String> {
    let recorder = Recorder::start(false)?;
    let engine = engine(scratch)?;
    let draft = AccountDraft {
        provider: Provider::OpenaiCompatible,
        method: SignInMethod::Url,
        name: "Snapshot compatible".to_owned(),
        url: Some(format!("http://127.0.0.1:{}/v1", recorder.port)),
        model: Some(MODEL.to_owned()),
    };
    let key = AccountSecret::new(DUMMY_KEY.as_bytes().to_vec()).map_err(|error| error.0)?;
    let (added, check) = with_owner(&engine, |owner| async move {
        let added = match owner.controller.add_account(draft, Some(key)).await {
            Ok(view) => view,
            Err(error) => return (owner, Err(format!("add_account: {}", error.0))),
        };
        let check = match owner.controller.check_account(added.id.clone()).await {
            Ok(check) => serde_json::to_value(check).map_err(|error| error.to_string()),
            Err(error) => Ok(json!({"refused": error.0})),
        };
        let added = serde_json::to_value(added).map_err(|error| error.to_string());
        let both = added.and_then(|added| Ok((added, check?)));
        (owner, both)
    })?;
    Ok(json!({"added": added, "check": check, "requests": recorder.requests()}))
}
