use crate::config::{
    ModelAccess, ModelAuthentication, ModelConnection, ModelEndpoint, ProviderQuotaPolicy,
};
use crate::protocol::CredentialRef;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberKind {
    Chatgpt,
    Claude,
    ApiKey,
    Url,
    Local,
    Other,
}

impl MemberKind {
    /// Names the kind of account a pool member is, in owner terms.
    /// Input: the member's connection. Output: ChatGPT or Claude for managed
    /// sign-ins, API key, local model for a loopback URL, URL, or Other.
    pub fn of(connection: &ModelConnection) -> Self {
        match (&connection.auth, &connection.endpoint) {
            (ModelAuthentication::NativeManaged { .. }, _) => match connection.harness.as_str() {
                "codex" => Self::Chatgpt,
                "claude" | "claude-code" => Self::Claude,
                _ => Self::Other,
            },
            (ModelAuthentication::ApiKey { .. }, _) => Self::ApiKey,
            (ModelAuthentication::None {}, ModelEndpoint::Http { url }) => {
                let local = url::Url::parse(url).is_ok_and(|url| {
                    matches!(
                        url.host_str(),
                        Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
                    )
                });
                if local { Self::Local } else { Self::Url }
            }
            _ => Self::Other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Chatgpt => "ChatGPT",
            Self::Claude => "Claude",
            Self::ApiKey => "API key",
            Self::Url => "URL",
            Self::Local => "Local model",
            Self::Other => "Other",
        }
    }

    fn subscription(self) -> bool {
        matches!(self, Self::Chatgpt | Self::Claude)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberStatus {
    SignedIn,
    NotSignedIn,
    SetUp,
}

impl MemberStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::SignedIn => "Signed in",
            Self::NotSignedIn => "Not signed in",
            Self::SetUp => "Set up",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolMember {
    pub id: CredentialRef,
    pub kind: MemberKind,
    pub provider: String,
    pub status: MemberStatus,
    pub status_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolSummary {
    pub id: CredentialRef,
    pub name: String,
    pub summary: String,
    pub members: Vec<PoolMember>,
    pub project_count: usize,
    #[serde(default)]
    pub provider_quota_policy: ProviderQuotaPolicy,
}

/// Builds the plain pool list shown to the owner.
/// Input: the model-access catalog and a check for whether a managed connection's
/// latest sign-in was observed. Output: one summary per pool, in catalog order,
/// such as "2 ChatGPT, 1 Claude, 1 API key" with each member's status.
pub fn summarize(
    catalog: &ModelAccess,
    signed_in: impl Fn(&ModelConnection) -> bool,
) -> Vec<PoolSummary> {
    let summaries: Vec<PoolSummary> = catalog
        .pools
        .iter()
        .map(|pool| {
            let members: Vec<PoolMember> = pool
                .members
                .iter()
                .filter_map(|id| catalog.connections.iter().find(|c| &c.id == id))
                .map(|connection| {
                    let kind = MemberKind::of(connection);
                    let status = if !kind.subscription() {
                        MemberStatus::SetUp
                    } else if signed_in(connection) {
                        MemberStatus::SignedIn
                    } else {
                        MemberStatus::NotSignedIn
                    };
                    PoolMember {
                        id: connection.id.clone(),
                        kind,
                        provider: kind.label().into(),
                        status,
                        status_label: status.label().into(),
                    }
                })
                .collect();
            let mut kinds: Vec<MemberKind> = members.iter().map(|m| m.kind).collect();
            kinds.sort();
            let mut parts: Vec<String> = Vec::new();
            for kind in kinds.iter().copied() {
                let count = kinds.iter().filter(|k| **k == kind).count();
                let part = format!("{count} {}", kind.label());
                if !parts.contains(&part) {
                    parts.push(part);
                }
            }
            PoolSummary {
                id: pool.id.clone(),
                name: pool.name.clone().unwrap_or_else(|| pool.id.as_str().into()),
                summary: if parts.is_empty() {
                    "No accounts".into()
                } else {
                    parts.join(", ")
                },
                members,
                project_count: pool.projects.len(),
                provider_quota_policy: pool.provider_quota_policy,
            }
        })
        .collect();
    assert_eq!(summaries.len(), catalog.pools.len());
    summaries
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIXED: &str = r#"accounts = ["codex-1", "codex-2", "claude-1", "api-1", "local-1", "url-1"]
connections = [
  { id = "codex-a", provider = "openai", harness = "codex", protocol = "native", account_ref = "codex-1", endpoint = { kind = "native" }, model = { id = "native-default", author = "openai" }, auth = { method = "native_managed", profile_ref = "codex-profile-a" } },
  { id = "codex-b", provider = "openai", harness = "codex", protocol = "native", account_ref = "codex-2", endpoint = { kind = "native" }, model = { id = "native-default", author = "openai" }, auth = { method = "native_managed", profile_ref = "codex-profile-b" } },
  { id = "claude-a", provider = "anthropic", harness = "claude", protocol = "native", account_ref = "claude-1", endpoint = { kind = "native" }, model = { id = "native-default", author = "anthropic" }, auth = { method = "native_managed", profile_ref = "claude-profile-a" } },
  { id = "api-a", provider = "openrouter", harness = "rcli", protocol = "openai-chat", account_ref = "api-1", endpoint = { kind = "http", url = "https://openrouter.ai/api/v1" }, model = { id = "some/model", author = "openrouter" }, auth = { method = "api_key", credential_ref = "api-key-ref" } },
  { id = "local-a", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-1", endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }, model = { id = "qwen2.5:0.5b", author = "openai-compatible" }, auth = { method = "none" } },
  { id = "url-a", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "url-1", endpoint = { kind = "http", url = "https://models.example.invalid/v1" }, model = { id = "m", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [
  { id = "pool-mixed", name = "Everything", members = ["claude-a", "api-a", "codex-a", "codex-b", "local-a", "url-a"], projects = ["demo"], roles = ["project_leader"], fallback_allowed = false },
  { id = "pool-plain", members = ["codex-a"], projects = [], roles = [], fallback_allowed = false },
]"#;

    #[test]
    fn a_mixed_pool_reads_as_plain_counts_with_member_status() {
        let catalog: ModelAccess = toml::from_str(MIXED).unwrap();
        catalog.validate().unwrap();
        let summaries = summarize(&catalog, |connection| connection.id.as_str() == "codex-a");
        assert_eq!(summaries.len(), 2);
        let mixed = &summaries[0];
        assert_eq!(mixed.name, "Everything");
        assert_eq!(
            mixed.summary,
            "2 ChatGPT, 1 Claude, 1 API key, 1 URL, 1 Local model"
        );
        assert_eq!(mixed.project_count, 1);
        let status: Vec<(&str, &str, &str)> = mixed
            .members
            .iter()
            .map(|m| (m.id.as_str(), m.provider.as_str(), m.status_label.as_str()))
            .collect();
        assert_eq!(
            status,
            vec![
                ("claude-a", "Claude", "Not signed in"),
                ("api-a", "API key", "Set up"),
                ("codex-a", "ChatGPT", "Signed in"),
                ("codex-b", "ChatGPT", "Not signed in"),
                ("local-a", "Local model", "Set up"),
                ("url-a", "URL", "Set up"),
            ]
        );
        assert_eq!(summaries[1].name, "pool-plain");
        assert_eq!(summaries[1].summary, "1 ChatGPT");
        assert_eq!(summaries[1].project_count, 0);
    }
}
