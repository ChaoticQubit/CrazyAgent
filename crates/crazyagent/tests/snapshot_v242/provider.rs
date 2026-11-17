use super::Scenario;
use crazyagent::accounts::{AccountDraft, Provider, SignInMethod};
use serde_json::{Value, json};
use std::path::Path;

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
    ]
}

/// Captures the provider catalog every client renders. Input: the scratch folder, unused.
/// Output: the catalog as JSON.
fn catalog(_scratch: &Path) -> Result<Value, String> {
    let entries = crazyagent::accounts::catalog();
    assert_eq!(entries.len(), PROVIDERS.len());
    serde_json::to_value(entries).map_err(|error| error.to_string())
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
/// with and without `/v1` and a trailing `/`. Input: the scratch folder, unused. Output: one
/// entry per harness name and URL.
fn endpoint_base_url(_scratch: &Path) -> Result<Value, String> {
    let urls = [
        "https://api.anthropic.com/v1",
        "https://api.anthropic.com/v1/",
        "https://api.openai.com/v1",
        "https://openrouter.ai/api/v1",
        "http://127.0.0.1:11434/v1",
        "http://127.0.0.1:11434",
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
