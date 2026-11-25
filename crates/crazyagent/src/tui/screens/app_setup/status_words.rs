use super::*;

pub(super) fn kind_word(kind: PackKind) -> &'static str {
    match kind {
        PackKind::Skill => "Skill",
        PackKind::McpServer => "MCP server",
    }
}

pub(super) fn pack_word(state: &PackState) -> &'static str {
    match state {
        PackState::Available => "Not installed",
        PackState::Installing => "Installing",
        PackState::On => "Installed",
        PackState::Off => "Turned off",
        PackState::Failed { .. } => "Failed",
    }
}

pub(super) fn tool_word(state: &DependencyState) -> String {
    match state {
        DependencyState::Checking => "Checking".into(),
        DependencyState::Missing { .. } => "Not installed".into(),
        DependencyState::Refused { .. } => "Found, not used".into(),
        DependencyState::Found { version } => format!("Found {}", version.as_str()),
        DependencyState::Bundled { version } => {
            format!("Bundled with CrazyAgent {}", version.as_str())
        }
        DependencyState::System { version, .. } => format!("Found {}", version.as_str()),
        DependencyState::Installing => "Installing".into(),
        DependencyState::Installed { version } => format!("Installed {}", version.as_str()),
        DependencyState::Failed { .. } => "Failed".into(),
        DependencyState::Unsupported => "Not supported".into(),
    }
}

pub(super) fn method_words(method: GitHubMethod) -> &'static str {
    match method {
        GitHubMethod::Device => "with GitHub sign-in",
        GitHubMethod::Token => "with a pasted token",
        GitHubMethod::Cli => "through the gh command",
        GitHubMethod::None => "",
    }
}
