use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference};
use crate::engine::EngineError;
use crate::protocol::CredentialRef;
use serde::{Deserialize, Serialize};

pub(crate) mod probe;

pub const ACCOUNT_NAME_MAX: usize = 96;
pub const ACCOUNT_URL_MAX: usize = 2048;
pub const ACCOUNT_MODEL_MAX: usize = 256;
pub const ACCOUNTS_PAGE_MAX: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Chatgpt,
    Claude,
    Openrouter,
    OpenaiApi,
    AnthropicApi,
    OpenaiCompatible,
    Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignInMethod {
    Subscription,
    ApiKey,
    Url,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    Hidden,
    Optional,
    Required,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MethodEntry {
    pub method: SignInMethod,
    pub label: String,
    pub url: Field,
    pub url_preset: Option<String>,
    pub key: Field,
    pub model: Field,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderEntry {
    pub provider: Provider,
    pub name: String,
    pub methods: Vec<MethodEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountDetails {
    pub name: String,
    pub provider: Provider,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountDraft {
    pub provider: Provider,
    pub method: SignInMethod,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    SignedIn,
    NotSignedIn,
    KeySaved,
    Unreachable,
    SigningIn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountView {
    pub id: CredentialRef,
    pub name: String,
    pub provider: Provider,
    pub provider_name: String,
    pub method: SignInMethod,
    pub method_label: String,
    pub url: Option<String>,
    pub model: Option<String>,
    pub status: AccountStatus,
    pub status_label: String,
}

struct Spec {
    harness: &'static str,
    protocol: &'static str,
    author: &'static str,
    url: Field,
    url_preset: Option<&'static str>,
    key: Field,
    model: Field,
}

const OPENAI_URL: &str = "https://api.openai.com/v1";
const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1";
const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1";
const OLLAMA_URL: &str = "http://127.0.0.1:11434/v1";

const PROVIDERS: [Provider; 7] = [
    Provider::Chatgpt,
    Provider::Claude,
    Provider::Openrouter,
    Provider::OpenaiApi,
    Provider::AnthropicApi,
    Provider::OpenaiCompatible,
    Provider::Local,
];

fn failure(code: &str) -> EngineError {
    EngineError(code.into())
}

/// Plain display name of one provider, as the account screens show it.
/// Input: a catalog provider. Output: a short human label without internal identifiers.
pub fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Chatgpt => "ChatGPT / Codex",
        Provider::Claude => "Claude",
        Provider::Openrouter => "OpenRouter",
        Provider::OpenaiApi => "OpenAI API",
        Provider::AnthropicApi => "Anthropic API",
        Provider::OpenaiCompatible => "Any OpenAI-compatible URL",
        Provider::Local => "Local models (Ollama)",
    }
}

fn methods(provider: Provider) -> &'static [SignInMethod] {
    match provider {
        Provider::Chatgpt | Provider::Claude => &[SignInMethod::Subscription, SignInMethod::ApiKey],
        Provider::Openrouter | Provider::OpenaiApi | Provider::AnthropicApi => {
            &[SignInMethod::ApiKey]
        }
        Provider::OpenaiCompatible | Provider::Local => &[SignInMethod::Url],
    }
}

fn method_label(provider: Provider, method: SignInMethod) -> &'static str {
    match (provider, method) {
        (Provider::Chatgpt, SignInMethod::Subscription) => "Sign in with ChatGPT",
        (_, SignInMethod::Subscription) => "Sign in with your Claude subscription",
        (_, SignInMethod::ApiKey) => "Use an API key",
        (_, SignInMethod::Url) => "Connect a URL",
    }
}

fn spec(provider: Provider, method: SignInMethod) -> Option<Spec> {
    if !methods(provider).contains(&method) {
        return None;
    }
    let keyed = |protocol, author, url| Spec {
        harness: "rcli",
        protocol,
        author,
        url: Field::Hidden,
        url_preset: Some(url),
        key: Field::Required,
        model: Field::Required,
    };
    let native = |harness, author| Spec {
        harness,
        protocol: "native",
        author,
        url: Field::Hidden,
        url_preset: None,
        key: Field::Hidden,
        model: Field::Hidden,
    };
    Some(match (provider, method) {
        (Provider::Chatgpt, SignInMethod::Subscription) => native("codex", "openai"),
        (Provider::Claude, SignInMethod::Subscription) => native("claude", "anthropic"),
        (Provider::Chatgpt | Provider::OpenaiApi, _) => keyed("openai-chat", "openai", OPENAI_URL),
        (Provider::Claude | Provider::AnthropicApi, _) => {
            keyed("anthropic-messages", "anthropic", ANTHROPIC_URL)
        }
        (Provider::Openrouter, _) => keyed("openai-compatible", "openrouter", OPENROUTER_URL),
        (Provider::OpenaiCompatible, _) => Spec {
            harness: "rcli",
            protocol: "openai-compatible",
            author: "openai-compatible",
            url: Field::Required,
            url_preset: None,
            key: Field::Optional,
            model: Field::Required,
        },
        (Provider::Local, _) => Spec {
            harness: "rcli",
            protocol: "openai-chat",
            author: "openai-compatible",
            url: Field::Required,
            url_preset: Some(OLLAMA_URL),
            key: Field::Hidden,
            model: Field::Required,
        },
    })
}

/// The provider catalog every client renders: providers in display order, each with its
/// ordered sign-in methods and which of URL, key and model the method asks for.
/// Output: plain labels and presets only; harness and protocol names stay in the engine.
pub fn catalog() -> Vec<ProviderEntry> {
    let entries: Vec<ProviderEntry> = PROVIDERS
        .iter()
        .map(|&provider| ProviderEntry {
            provider,
            name: provider_name(provider).into(),
            methods: methods(provider)
                .iter()
                .map(|&method| {
                    let spec = spec(provider, method).expect("catalog method has a spec");
                    MethodEntry {
                        method,
                        label: method_label(provider, method).into(),
                        url: spec.url,
                        url_preset: spec.url_preset.map(str::to_owned),
                        key: spec.key,
                        model: spec.model,
                    }
                })
                .collect(),
        })
        .collect();
    assert_eq!(entries.len(), PROVIDERS.len());
    assert!(entries.iter().all(|entry| !entry.methods.is_empty()));
    entries
}

/// Checks one owner-entered account name: trimmed, bounded and free of control or
/// bidirectional override characters. Input: raw text. Output: the trimmed name or an error.
pub fn account_name(raw: &str) -> Result<String, EngineError> {
    let name = raw.trim();
    if name.chars().count() > ACCOUNT_NAME_MAX
        || name.chars().any(|value| {
            value.is_control()
                || matches!(
                    value,
                    '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
                )
        })
    {
        return Err(failure("account_name_invalid"));
    }
    Ok(name.to_owned())
}

fn loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

/// Parses an owner-entered endpoint as a plain HTTP(S) address without credentials, query
/// or fragment. Input: raw text. Output: the parsed URL or `account_url_invalid`.
pub fn account_url(raw: &str) -> Result<url::Url, EngineError> {
    let invalid = || failure("account_url_invalid");
    if raw.len() > ACCOUNT_URL_MAX
        || raw
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return Err(invalid());
    }
    let url = url::Url::parse(raw).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(url)
}

/// Checks that a local-model address is plain HTTP on this machine.
/// Input: raw text. Output: the parsed URL or `account_local_url_required`.
pub fn local_url(raw: &str) -> Result<url::Url, EngineError> {
    let url = account_url(raw)?;
    if url.scheme() != "http" || !loopback(&url) {
        return Err(failure("account_local_url_required"));
    }
    Ok(url)
}

impl AccountDraft {
    /// Turns one add-account choice into the routing connection the engine stores, plus the
    /// owner-facing name and provider. Inputs: whether a key accompanies the draft and a
    /// random hex nonce (at least 20 characters). Output: a validated connection or an error.
    pub fn build(
        &self,
        key: bool,
        nonce: &str,
    ) -> Result<(ModelConnection, AccountDetails), EngineError> {
        if nonce.len() < 20 || !nonce.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(failure("account_identity_unavailable"));
        }
        let spec = spec(self.provider, self.method)
            .ok_or_else(|| failure("account_method_unavailable"))?;
        let name = account_name(&self.name)?;
        let name = if name.is_empty() {
            provider_name(self.provider).to_owned()
        } else {
            name
        };
        let given = |value: &Option<String>| {
            value
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        let url = given(&self.url);
        let model = given(&self.model);
        match (spec.url, &url) {
            (Field::Hidden, Some(_)) => return Err(failure("account_url_not_allowed")),
            (Field::Required, None) if spec.url_preset.is_none() => {
                return Err(failure("account_url_required"));
            }
            _ => {}
        }
        match (spec.model, &model) {
            (Field::Hidden, Some(_)) => return Err(failure("account_model_not_allowed")),
            (Field::Required, None) => return Err(failure("account_model_required")),
            _ => {}
        }
        if model.as_ref().is_some_and(|value| {
            value.len() > ACCOUNT_MODEL_MAX
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:/@+".contains(&byte))
        }) {
            return Err(failure("account_model_invalid"));
        }
        match (spec.key, key) {
            (Field::Hidden, true) => return Err(failure("account_key_not_allowed")),
            (Field::Required, false) => return Err(failure("account_key_required")),
            _ => {}
        }
        let endpoint = if spec.protocol == "native" {
            ModelEndpoint::Native {}
        } else {
            let address = url
                .as_deref()
                .or(spec.url_preset)
                .ok_or_else(|| failure("account_url_required"))?;
            if self.provider == Provider::Local {
                local_url(address)?;
            } else {
                account_url(address)?;
            }
            ModelEndpoint::Http {
                url: address.to_owned(),
            }
        };
        let nonce = &nonce[..20];
        let prefix = match self.provider {
            Provider::Chatgpt => "chatgpt",
            Provider::Claude => "claude",
            Provider::Openrouter => "openrouter",
            Provider::OpenaiApi => "openai",
            Provider::AnthropicApi => "anthropic",
            Provider::OpenaiCompatible => "url",
            Provider::Local => "local",
        };
        let reference = |kind: &str| {
            CredentialRef::new(format!("{prefix}-{kind}-{nonce}"))
                .map_err(|_| failure("account_identity_unavailable"))
        };
        let reference_of = |value: &str| {
            CredentialRef::new(value).map_err(|_| failure("account_identity_unavailable"))
        };
        let auth = match (spec.protocol, key) {
            ("native", _) => ModelAuthentication::NativeManaged {
                profile_ref: reference("profile")?,
            },
            (_, true) => ModelAuthentication::ApiKey {
                credential_ref: reference("key")?,
            },
            (_, false) => ModelAuthentication::None {},
        };
        let connection = ModelConnection {
            id: reference("account")?,
            provider: reference_of(spec.author)?,
            harness: reference_of(spec.harness)?,
            protocol: reference_of(spec.protocol)?,
            account_ref: reference("login")?,
            endpoint,
            model: ModelReference {
                id: model.unwrap_or_else(|| "native-default".into()),
                author: reference_of(spec.author)?,
            },
            auth,
        };
        crate::config::ModelAccess {
            accounts: vec![connection.account_ref.clone()],
            connections: vec![connection.clone()],
            ..Default::default()
        }
        .validate()
        .map_err(|_| failure("account_url_invalid"))?;
        assert_eq!(method_of(&connection, self.provider), self.method);
        Ok((
            connection,
            AccountDetails {
                name,
                provider: self.provider,
            },
        ))
    }
}

/// Recovers the sign-in method a stored connection was created with.
/// Inputs: the connection and its catalog provider. Output: subscription, API key or URL.
pub fn method_of(connection: &ModelConnection, provider: Provider) -> SignInMethod {
    match (&connection.auth, provider) {
        (ModelAuthentication::NativeManaged { .. }, _) => SignInMethod::Subscription,
        (_, Provider::OpenaiCompatible | Provider::Local) => SignInMethod::Url,
        _ => SignInMethod::ApiKey,
    }
}

/// Infers the catalog provider of a connection saved without account details, such as one
/// made by an earlier setup screen. Input: the stored connection. Output: its best provider.
pub fn provider_of(connection: &ModelConnection) -> Provider {
    let http = match &connection.endpoint {
        ModelEndpoint::Http { url } => url::Url::parse(url).ok(),
        ModelEndpoint::Native {} => None,
    };
    match (
        connection.harness.as_str(),
        connection.provider.as_str(),
        connection.protocol.as_str(),
    ) {
        ("codex", ..) => Provider::Chatgpt,
        ("claude" | "claude-code", ..) => Provider::Claude,
        (_, "openrouter", _) => Provider::Openrouter,
        (_, _, "anthropic-messages") => Provider::AnthropicApi,
        (_, "openai", _) => Provider::OpenaiApi,
        _ if matches!(connection.auth, ModelAuthentication::None {})
            && http
                .as_ref()
                .is_some_and(|url| url.scheme() == "http" && loopback(url)) =>
        {
            Provider::Local
        }
        _ => Provider::OpenaiCompatible,
    }
}

impl AccountStatus {
    /// Plain status text for the account list. Output: one of the five owner-facing labels.
    pub fn label(self) -> &'static str {
        match self {
            Self::SignedIn => "Signed in",
            Self::NotSignedIn => "Not signed in",
            Self::KeySaved => "Key saved",
            Self::Unreachable => "Can't reach this URL",
            Self::SigningIn => "Signing in...",
        }
    }
}

#[cfg(test)]
mod tests;
