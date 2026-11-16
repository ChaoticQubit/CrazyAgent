use super::AccountKind;
use super::manifest::{self, DriverKind, Manifest, Refused};
use super::registry::Harness;
use crate::app_plugins::yaml::Value;
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint};
use crate::engine::EngineError;
use crate::model_gateway::relay::{LOCAL_PLACEHOLDER, Relay, is_placeholder};

/// The most environment variables one `accounts` entry may set.
pub const ENV_MAX: usize = 16;
const NAME_BYTES_MAX: usize = 64;
const VALUE_BYTES_MAX: usize = 1024;
const HANDED: [&str; 3] = ["relay_url", "token", "model"];
const OWNED: [&str; 11] = [
    "PATH",
    "HOME",
    "TMPDIR",
    "SHELL",
    "TERM",
    "LANG",
    "TZ",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
];
const OWNED_PREFIXES: [&str; 5] = ["DYLD_", "LD_", "GIT_", "LC_", "CRAZYAGENT_"];

/// What a listed harness is given to reach the model of one pool account: the address it
/// sends model requests to, the placeholder it presents there, the model, and the
/// environment its manifest's `accounts` entry asks for. It never holds a saved key.
pub struct Handover {
    base_url: String,
    token: String,
    model: String,
    env: Vec<(String, String)>,
}

impl Handover {
    /// The address the harness sends model requests to. Output: the engine relay's loopback
    /// URL for a keyed account, the account's own address for a keyless one.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The placeholder the harness presents. Output: the relay token, or the local
    /// placeholder for a keyless account; never a saved key.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The model the account names. Output: the model id.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The environment the manifest's `accounts` entry asks for, values filled in.
    /// Output: name and value pairs in file order.
    pub fn env(&self) -> &[(String, String)] {
        &self.env
    }
}

impl std::fmt::Debug for Handover {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Handover")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field(
                "env",
                &self.env.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

fn failure(code: &str) -> EngineError {
    EngineError(code.into())
}

fn refuse<T>(code: &'static str, message: String) -> Result<T, Refused> {
    Err(Refused { code, message })
}

/// Whether the engine drives the harness with one of its own adapters (Codex, Claude Code,
/// RCLI), which hand accounts over themselves. Input: the manifest. Output: the answer.
fn own_adapter(manifest: &Manifest) -> bool {
    matches!(
        manifest.piped,
        Some(DriverKind::CodexAppServer | DriverKind::ClaudeStreamJson | DriverKind::Rcli)
    )
}

fn variable(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_BYTES_MAX
        && name.as_bytes()[0].is_ascii_uppercase()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

/// Each placeholder written in a text, as the manifest reader finds them: a brace, a run of
/// lower-case letters and `_`, a closing brace. Input: the text. Output: the names in order.
fn placeholders(text: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let length = after
            .bytes()
            .take_while(|byte| byte.is_ascii_lowercase() || *byte == b'_')
            .count();
        if length > 0 && after[length..].starts_with('}') {
            names.push(&after[..length]);
        }
        rest = after;
    }
    assert!(names.len() <= text.len() / 3 + 1);
    names
}

/// Fills the handed placeholders of one text in a single pass, so a filled value is never
/// read again as a placeholder. Inputs: the text and each placeholder with its value.
/// Output: the filled text.
fn fill(text: &str, values: &[(&str, &str); 3]) -> String {
    let mut filled = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        filled.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let length = after
            .bytes()
            .take_while(|byte| byte.is_ascii_lowercase() || *byte == b'_')
            .count();
        let value = values
            .iter()
            .find(|(name, _)| *name == &after[..length])
            .filter(|_| after[length..].starts_with('}'));
        match value {
            Some((_, value)) => {
                filled.push_str(value);
                rest = &after[length + 1..];
            }
            None => {
                filled.push('{');
                rest = after;
            }
        }
    }
    filled.push_str(rest);
    filled
}

fn check_env(env: &Value, path: &str) -> Result<(), Refused> {
    let Value::Map(pairs) = env else {
        return refuse(
            manifest::TYPE,
            format!("{path}: this must be a block of settings."),
        );
    };
    if pairs.len() > ENV_MAX {
        return refuse(
            manifest::VALUE,
            format!("{path}: an account entry sets at most {ENV_MAX} variables."),
        );
    }
    for (name, value) in pairs {
        if !variable(name) {
            return refuse(
                manifest::VALUE,
                format!("{path}: {name} is not a variable name of capital letters, digits and _."),
            );
        }
        if OWNED.contains(&name.as_str())
            || OWNED_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
        {
            return refuse(
                manifest::VALUE,
                format!("{path}: {name} is not a variable an account entry may set."),
            );
        }
        let Value::Str(text) = value else {
            return refuse(
                manifest::TYPE,
                format!("{path}.{name}: this must be text in quotes."),
            );
        };
        if text.len() > VALUE_BYTES_MAX {
            return refuse(
                manifest::VALUE,
                format!("{path}.{name}: this text is longer than {VALUE_BYTES_MAX} bytes."),
            );
        }
        if let Some(other) = placeholders(text)
            .into_iter()
            .find(|found| !HANDED.contains(found))
        {
            return refuse(
                manifest::PLACEHOLDER,
                format!(
                    "{path}.{name}: {{{other}}} is not a value an account hands over; use {{relay_url}}, {{token}} or {{model}}."
                ),
            );
        }
    }
    Ok(())
}

/// Checks the `accounts` block of a manifest, which says how a listed harness is given an
/// account of each wire format it lists: each entry is named after one wire format the
/// manifest lists, and its `env` sets plain variables, none of the names this check refuses
/// (search path, home, temp folder, shell, locale, git, proxy and the engine's own), to
/// text that holds no placeholder but `{relay_url}`, `{token}` and `{model}`. That list is
/// a load-time subset, not the launch guard: `sandbox::agent_env` sets the engine's pins
/// (git, home, TLS and the zsh temp prefix) after the harness's own variables, so an entry
/// never replaces one. A harness on a shared driver lists each wire format by its full name
/// and has an entry for every one; the engine's own adapters need none. Input: the
/// manifest. Output: Ok, or a refusal that names the setting.
pub fn check(manifest: &Manifest) -> Result<(), Refused> {
    let shared = !own_adapter(manifest);
    if shared
        && let Some(pattern) = manifest
            .account_protocols
            .iter()
            .find(|listed| listed.ends_with('*'))
    {
        return refuse(
            manifest::VALUE,
            format!(
                "account_protocols: {pattern} stands for many wire formats; a harness on a shared driver lists each one by its full name."
            ),
        );
    }
    let entries: &[(String, Value)] = match manifest.block("accounts") {
        None => &[],
        Some(Value::Map(entries)) => entries,
        Some(_) => {
            return refuse(
                manifest::TYPE,
                "accounts: this must be a block of settings.".to_owned(),
            );
        }
    };
    for (format, entry) in entries {
        let path = format!("accounts.{format}");
        if !manifest.account_protocols.contains(format) {
            return refuse(
                manifest::VALUE,
                format!("{path}: this wire format is not listed under account_protocols."),
            );
        }
        if !matches!(entry, Value::Map(_)) {
            return refuse(
                manifest::TYPE,
                format!("{path}: this must be a block of settings."),
            );
        }
        if let Some(env) = entry.get("env") {
            check_env(env, &format!("{path}.env"))?;
        }
    }
    if shared
        && let Some(missing) = manifest
            .account_protocols
            .iter()
            .find(|listed| !entries.iter().any(|(format, _)| format == *listed))
    {
        return refuse(
            manifest::MISSING,
            format!(
                "accounts: {missing} is listed under account_protocols and needs an entry here."
            ),
        );
    }
    Ok(())
}

/// Hands one pool account to a listed harness (DEC-141 PA-4): a keyed account through the
/// engine key relay started for it, so the harness gets the relay's loopback URL and its
/// placeholder token; a keyless one as its own address and the local placeholder. The values
/// are filled into the environment the manifest's `accounts` entry for the account's wire
/// format names. No saved key is read here. Inputs: the harness, the stored connection and
/// the relay of a keyed account. Output: the hand-over; `account_not_compatible` for an
/// account the harness cannot run, `account_key_relay_missing` for a keyed account without
/// its relay, `account_relay_unexpected` for a keyless one with a relay, or
/// `account_handover_missing` for a harness the engine's own adapters drive.
pub fn handover(
    harness: &Harness,
    connection: &ModelConnection,
    relay: Option<&Relay>,
) -> Result<Handover, EngineError> {
    if !harness.account_fits(connection) {
        return Err(failure("account_not_compatible"));
    }
    let ModelEndpoint::Http { url } = &connection.endpoint else {
        return Err(failure("account_not_compatible"));
    };
    let (base_url, token) = match (&connection.auth, relay) {
        (ModelAuthentication::ApiKey { .. }, Some(relay)) => {
            (relay.base_url(), relay.token().to_owned())
        }
        (ModelAuthentication::ApiKey { .. }, None) => {
            return Err(failure("account_key_relay_missing"));
        }
        (ModelAuthentication::None {}, None) => (
            super::endpoint_base_url(&harness.manifest().id, url).to_owned(),
            LOCAL_PLACEHOLDER.to_owned(),
        ),
        (ModelAuthentication::None {}, Some(_)) => {
            return Err(failure("account_relay_unexpected"));
        }
        _ => return Err(failure("account_not_compatible")),
    };
    let entry = harness
        .manifest()
        .block("accounts")
        .and_then(|accounts| accounts.get(connection.protocol.as_str()))
        .ok_or_else(|| failure("account_handover_missing"))?;
    let model = connection.model.id.clone();
    let values = [
        ("relay_url", base_url.as_str()),
        ("token", token.as_str()),
        ("model", model.as_str()),
    ];
    let env: Vec<(String, String)> = match entry.get("env") {
        Some(Value::Map(pairs)) => pairs
            .iter()
            .map(|(name, value)| match value {
                Value::Str(text) => (name.clone(), fill(text, &values)),
                _ => unreachable!("accounts env text is checked when the manifest loads"),
            })
            .collect(),
        _ => Vec::new(),
    };
    assert!(is_placeholder(&token));
    assert!(env.len() <= ENV_MAX);
    Ok(Handover {
        base_url,
        token,
        model,
        env,
    })
}

/// Hands a ChatGPT sign-in to a listed harness that lists `openai-responses`, through the
/// ChatGPT relay started for it (DEC-180 addendum 2, answer 2): the harness gets the
/// relay's loopback URL, its placeholder and the model chosen for the run, filled into the
/// manifest's `openai-responses` entry; the sign-in's stored `native-default` is never
/// handed out. Inputs: the harness, the stored sign-in, its ChatGPT relay and the run's
/// model. Output: the hand-over; `account_not_compatible` for another account or harness,
/// `account_chatgpt_relay_missing` for a relay that is not a ChatGPT relay,
/// `account_model_missing` for a blank model, or `account_handover_missing`.
pub fn chatgpt_handover(
    harness: &Harness,
    connection: &ModelConnection,
    relay: &Relay,
    model: &str,
) -> Result<Handover, EngineError> {
    let _ = (harness, connection, relay, model);
    Err(failure("account_not_compatible"))
}

/// Whether accounts made by one Add account choice fit a harness, by the same rule as
/// `Harness::account_fits`: a sign-in only the harness it was made with, when that harness
/// lists subscription accounts; an endpoint account any harness that speaks its wire format.
/// Inputs: the harness and the harness and wire format the choice stores. Output: the answer.
fn choice_fits(harness: &Harness, made_with: &str, protocol: &str) -> bool {
    if protocol == "native" {
        harness
            .manifest()
            .account_kinds
            .contains(&AccountKind::Subscription)
            && harness.named(made_with)
    } else {
        harness.speaks(protocol)
    }
}

/// The sentence a team start shows when no account of the project's pool can run a listed
/// harness: it names the harness by its product name and each kind of account that fits
/// it once, by provider and sign-in method in catalog order, or says that no kind of
/// account fits it yet. Input: the harness. Output: the plain sentence.
pub fn no_account_line(harness: &Harness) -> String {
    let name = &harness.manifest().name;
    let mut fitting: Vec<&'static str> = Vec::new();
    for entry in crate::accounts::catalog() {
        for method in &entry.methods {
            let fits = crate::accounts::stored_as(entry.provider, method.method)
                .is_some_and(|(made_with, protocol)| choice_fits(harness, made_with, protocol));
            let phrase = crate::accounts::account_phrase(entry.provider, method.method);
            if fits && !fitting.contains(&phrase) {
                fitting.push(phrase);
            }
        }
    }
    match fitting.as_slice() {
        [] => format!(
            "No account in this project's pool can run {name}, and no kind of account CrazyAgent can add fits it yet"
        ),
        [only] => format!(
            "No account in this project's pool can run {name}. Accounts that fit it: {only}"
        ),
        [rest @ .., last] => format!(
            "No account in this project's pool can run {name}. Accounts that fit it: {} or {last}",
            rest.join(", ")
        ),
    }
}

#[cfg(test)]
#[path = "accounts_tests.rs"]
mod tests;
