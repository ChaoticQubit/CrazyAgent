use serde::{Deserialize, Serialize};

pub const CLIENT_ID: &str = "Ov23liHICPRkMyS5sNMV";
pub const SCOPES: &str = "repo read:user";
pub const REPOS_PER_PAGE: u32 = 100;
pub const REPO_PAGE_MAX: u32 = 100;

/// How the engine holds the owner's GitHub token: device sign-in, a pasted token, a token
/// reused from a signed-in `gh` CLI, or none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubMethod {
    Device,
    Token,
    Cli,
    None,
}

/// What the GitHub settings page shows: signed in or not, the login name, the method,
/// whether a device sign-in is waiting for the owner, and a plain problem sentence such as
/// "GitHub sign-in is not set up yet: client id not configured."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubStatus {
    pub signed_in: bool,
    pub login: Option<String>,
    pub method: GitHubMethod,
    pub waiting: bool,
    pub problem: Option<String>,
}

/// The code the owner types at GitHub's device page, that page's address and how many
/// seconds the code stays valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubDeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    pub expires_s: u64,
}

/// One repository the signed-in owner can clone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubRepo {
    pub full_name: String,
    pub private: bool,
    pub default_branch: String,
    pub size_kb: u64,
}

/// One page of repositories, newest activity first, and the next page number if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubRepoPage {
    pub repos: Vec<GitHubRepo>,
    pub next: Option<u32>,
}

/// A finished clone: which repository and the project folder it landed in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloneReceipt {
    pub full_name: String,
    pub path: String,
}
