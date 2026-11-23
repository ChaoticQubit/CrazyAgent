use std::ffi::OsStr;

pub const REFUSED_VARIABLES: &[&str] = &[
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "SSH_AUTH_SOCK",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "PERL5LIB",
    "PERL5OPT",
    "PERLLIB",
    "PERL5DB",
];

pub const REFUSED_PREFIXES: &[&str] = &[
    "GH_",
    "GITHUB_",
    "GIT_CONFIG_KEY_",
    "GIT_CONFIG_VALUE_",
    "AWS_",
    "AZURE_",
    "CRAZYAGENT_",
];

pub const REFUSED_SUFFIXES: &[&str] = &[
    "_API_KEY",
    "_TOKEN",
    "_SECRET",
    "_SECRET_KEY",
    "_PASSWORD",
    "_ACCESS_KEY",
    "_CREDENTIALS",
];

/// Tells whether an environment variable name may never reach an agent process: GitHub and
/// git credential settings, the owner's SSH agent, cloud and provider keys, tokens,
/// secrets, passwords, the perl variables that would make a pane's keeper, a perl outside
/// the sandbox, load other code, and the engine's own `CRAZYAGENT_` settings, including the model key
/// name `ANTHROPIC_API_KEY`, which passes only with the local placeholder (see
/// [`agent_variable_allowed`]). Input: the variable name. Output: true when refused.
pub fn agent_env_refused(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return true;
    };
    let upper = name.to_ascii_uppercase();
    REFUSED_VARIABLES.contains(&upper.as_str())
        || REFUSED_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
        || REFUSED_SUFFIXES
            .iter()
            .any(|suffix| upper.ends_with(suffix))
}
