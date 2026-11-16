/// Names a manifest may never set in `accounts.<format>.env` or `home.env`, and no
/// `home.env_prefixes` entry may cover: the engine sets them on a launch, or they decide which
/// shell setup runs, which code a runtime loads or which server is trusted.
const NAMES: [&str; 26] = [
    "PATH",
    "HOME",
    "SHELL",
    "TMPDIR",
    "TMPPREFIX",
    "USER",
    "LOGNAME",
    "TERM",
    "LANG",
    "TZ",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "ENV",
    "ZDOTDIR",
    "PROMPT_COMMAND",
    "SHELLOPTS",
    "BASHOPTS",
    "PS4",
    "ELECTRON_RUN_AS_NODE",
    "JAVA_TOOL_OPTIONS",
    "JDK_JAVA_OPTIONS",
    "GOFLAGS",
    "CURL_CA_BUNDLE",
    "REQUESTS_CA_BUNDLE",
];

/// Starts of names in the families owned the same way as [`NAMES`]: the engine's own, the
/// dynamic linkers, git, TLS, the package managers and toolchains the engine pins or keeps
/// from installing, the native harness homes, the locale and XDG folders, and the language
/// runtimes and shells that load code from a variable.
const STARTS: [&str; 21] = [
    "CRAZYAGENT_",
    "DYLD_",
    "LD_",
    "GIT_",
    "SSL_",
    "NPM_",
    "PNPM_",
    "CARGO_",
    "RUST",
    "CODEX_",
    "CLAUDE_",
    "UV_",
    "COREPACK_",
    "HOMEBREW_",
    "LC_",
    "XDG_",
    "PYTHON",
    "PERL",
    "RUBY",
    "NODE_",
    "BASH_",
];

/// Whether a manifest may not set a variable of this name in `accounts.<format>.env` or
/// `home.env`: the engine sets it on a launch, or it decides which shell setup runs, which
/// code a runtime loads or which server is trusted. Compared upper-cased, so the engine's
/// lower-case names count too. Input: the name. Output: the answer.
pub(super) fn owned(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    NAMES.contains(&upper.as_str()) || STARTS.iter().any(|start| upper.starts_with(start))
}

/// Whether a `home.env_prefixes` entry would cover a name the engine owns, in either
/// direction: an owned name starts with its stem, or an owned family starts with its stem
/// or begins the prefix. Input: a prefix of capital letters, digits and `_` that ends with
/// `_`. Output: the answer.
pub(super) fn claims(prefix: &str) -> bool {
    assert!(prefix.len() > 1 && prefix.ends_with('_'));
    let stem = &prefix[..prefix.len() - 1];
    NAMES.iter().any(|name| name.starts_with(stem))
        || STARTS
            .iter()
            .any(|start| start.starts_with(stem) || prefix.starts_with(start))
}

#[cfg(test)]
#[path = "env_names_tests.rs"]
mod tests;
