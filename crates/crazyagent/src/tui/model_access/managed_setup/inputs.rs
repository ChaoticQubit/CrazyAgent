use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Provider {
    Codex,
    Claude,
    Api,
    Local,
}

impl Provider {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::Api => "API key or compatible endpoint",
            Self::Local => "Local endpoint",
        }
    }

    pub(super) fn native(self) -> bool {
        matches!(self, Self::Codex | Self::Claude)
    }

    fn choices() -> [Self; 4] {
        [Self::Codex, Self::Claude, Self::Api, Self::Local]
    }
}

pub(super) struct Input {
    pub provider: Provider,
    pub label: String,
    pub endpoint: String,
    pub model: String,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            provider: Provider::Codex,
            label: String::new(),
            endpoint: String::new(),
            model: String::new(),
        }
    }
}

impl Input {
    pub fn choose(&mut self, forward: bool) {
        let choices = Provider::choices();
        let index = choices
            .iter()
            .position(|item| item == &self.provider)
            .unwrap();
        let delta = if forward { 1 } else { choices.len() - 1 };
        self.provider = choices[(index + delta) % choices.len()];
    }

    pub fn edit(&mut self, stage: Stage, character: Option<char>) -> Result<(), String> {
        let (value, limit) = match stage {
            Stage::Label => (&mut self.label, 128),
            Stage::Endpoint => (&mut self.endpoint, 2048),
            Stage::Model => (&mut self.model, 256),
            _ => return Err("Select an account setup field".into()),
        };
        if let Some(character) = character {
            if character.is_control() || value.len() + character.len_utf8() > limit {
                return Err(format!("This field accepts at most {limit} literal bytes"));
            }
            value.push(character);
        } else {
            value.pop();
        }
        Ok(())
    }

    pub fn validate_endpoint(&self) -> Result<(), String> {
        let endpoint = url::Url::parse(&self.endpoint)
            .map_err(|_| "Enter a complete HTTP or HTTPS endpoint URL")?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || !endpoint.has_host()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || self
                .endpoint
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
        {
            return Err("Endpoint URL must exclude credentials, queries and fragments".into());
        }
        if self.provider == Provider::Local
            && (endpoint.scheme() != "http"
                || !endpoint.host().is_some_and(|host| match host {
                    url::Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
                    url::Host::Ipv4(address) => address.is_loopback(),
                    url::Host::Ipv6(address) => address.is_loopback(),
                }))
        {
            return Err("A no-key local endpoint requires an HTTP loopback address".into());
        }
        Ok(())
    }

    pub fn connection(&self, nonce: &str) -> Result<ModelConnection, String> {
        let reference = |kind| generated_reference(self.provider, &self.label, nonce, kind);
        let provider = match self.provider {
            Provider::Codex => "openai",
            Provider::Claude => "anthropic",
            _ => "openai-compatible",
        };
        let native = self.provider.native();
        let connection = ModelConnection {
            id: reference("connection")?,
            provider: CredentialRef::new(provider).map_err(|error| error.to_string())?,
            harness: CredentialRef::new(match self.provider {
                Provider::Codex => "codex",
                Provider::Claude => "claude",
                _ => "rcli",
            })
            .map_err(|error| error.to_string())?,
            protocol: CredentialRef::new(if native { "native" } else { "openai-chat" })
                .map_err(|error| error.to_string())?,
            account_ref: reference("account")?,
            endpoint: if native {
                ModelEndpoint::Native {}
            } else {
                self.validate_endpoint()?;
                ModelEndpoint::Http {
                    url: self.endpoint.clone(),
                }
            },
            model: ModelReference {
                id: if native {
                    "native-default".into()
                } else {
                    self.model.clone()
                },
                author: CredentialRef::new(provider).map_err(|error| error.to_string())?,
            },
            auth: match self.provider {
                Provider::Codex | Provider::Claude => ModelAuthentication::NativeManaged {
                    profile_ref: reference("profile")?,
                },
                Provider::Api => ModelAuthentication::ApiKey {
                    credential_ref: reference("profile")?,
                },
                Provider::Local => ModelAuthentication::None {},
            },
        };
        crate::config::ModelAccess {
            accounts: vec![connection.account_ref.clone()],
            connections: vec![connection.clone()],
            pools: Vec::new(),
        }
        .validate()
        .map_err(|_| "Enter a valid model identifier and endpoint".to_owned())?;
        Ok(connection)
    }

    pub fn lines(&self, stage: Stage) -> Vec<String> {
        match stage {
            Stage::Provider => {
                let mut lines: Vec<_> = Provider::choices()
                    .into_iter()
                    .map(|provider| {
                        format!(
                            "{} {}",
                            if provider == self.provider { ">" } else { " " },
                            provider.label()
                        )
                    })
                    .collect();
                lines.extend([
                    "Up/Down selects | Enter continues | Esc cancels".into(),
                    "Managed sign-in availability is checked by the owner".into(),
                    "Advanced: Esc, 2 Connections, n Create".into(),
                ]);
                lines
            }
            Stage::Label => vec![
                format!("{} | Account label (optional)", self.provider.label()),
                self.label.clone(),
                "Enter continues | Esc cancels without changes".into(),
            ],
            Stage::Endpoint => vec![
                "Endpoint URL".into(),
                self.endpoint.clone(),
                "Enter continues | Esc cancels without changes".into(),
            ],
            Stage::Model => vec![
                "Model".into(),
                self.model.clone(),
                "Enter saves account setup | Esc cancels without changes".into(),
            ],
            _ => vec![
                format!("Setting up your {} account", self.provider.label()),
                self.label.clone(),
                "Saving your account setup and account pool".into(),
                if self.provider.native() {
                    "Next: choose an official sign-in method".into()
                } else {
                    "Metadata does not verify the endpoint, key or model".into()
                },
                "Ctrl-R checks pending setup | Ctrl-P refreshes before retry".into(),
            ],
        }
    }

    pub fn assert_consistent(&self) {
        for (value, limit) in [
            (&self.label, 128),
            (&self.endpoint, 2048),
            (&self.model, 256),
        ] {
            assert!(value.len() <= limit);
            assert!(!value.chars().any(char::is_control));
        }
    }
}

pub(super) fn generated_reference(
    provider: Provider,
    label: &str,
    nonce: &str,
    kind: &str,
) -> Result<CredentialRef, String> {
    let stem: String = label
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(24)
        .flat_map(char::to_lowercase)
        .collect();
    let prefix = match provider {
        Provider::Codex => "codex",
        Provider::Claude => "claude",
        Provider::Api => "api",
        Provider::Local => "local",
    };
    CredentialRef::new(format!("{prefix}-{kind}-{stem}-{nonce}")).map_err(|error| error.to_string())
}
