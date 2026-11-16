use super::{claims, owned};
use crate::harnesses::home;
use crate::harnesses::launch::GitIdentity;
use crate::harnesses::leftovers::LINEAGE_VARIABLE;
use crate::harnesses::manifest::{VALUE, parse};
use crate::harnesses::registry::Registry;
use crate::sandbox::model_proxy;
use crate::sandbox::toolchains::AUTO_INSTALL_ENV;
use crate::sandbox::{AGENT_GIT_ENV, AGENT_HOME_ENV, AGENT_TLS_ENV, TOOL_CACHE_ENV};

const SMALL: &str = "schema: 1\nkind: harness\nid: \"sample\"\nname: \"Sample\"\nbinaries:\n  - \"sample\"\nheadless: \"sample --json\"\nmodel_selection: true\naccount_kinds:\n  - \"api_key\"\n";

const LISTED: &str = concat!(
    "schema: 1\nkind: harness\nid: \"rowharness\"\nname: \"Row Harness\"\n",
    "binaries:\n  - \"rowharness\"\nheadless: \"rowharness for these rows\"\n",
    "model_selection: true\naccount_kinds:\n  - \"api_key\"\n",
    "  - \"open_ai_compatible_url\"\n  - \"local\"\n",
    "roles:\n  - \"agent\"\nsub_agents: \"app\"\npiped:\n  driver: \"jsonl-turn\"\n",
    "account_protocols:\n  - \"openai-chat\"\n",
    "accounts:\n  openai-chat:\n    env:\n",
);

const COVERS: &str =
    "this prefix covers names CrazyAgent sets itself or that decide which code runs.";

const ENGINE_FAMILY_NAMES: &[&str] = &[
    "NPM_CONFIG_NODE_OPTIONS",
    "NPM_CONFIG_SCRIPT_SHELL",
    "PNPM_HOME",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_HOME",
    "RUSTC_WRAPPER",
    "RUSTFLAGS",
    "RUSTUP_TOOLCHAIN",
    "SSL_CERT_DIR",
    "CODEX_HOME",
    "CLAUDE_CONFIG_DIR",
    "UV_INDEX_URL",
    "COREPACK_HOME",
    "HOMEBREW_PREFIX",
    "PYTHONSTARTUP",
    "PYTHONPATH",
    "PERL5OPT",
    "PERLLIB",
    "RUBYOPT",
    "NODE_OPTIONS",
    "NODE_EXTRA_CA_CERTS",
    "BASH_ENV",
    "BASH_FUNC_X",
    "LD_PRELOAD",
    "DYLD_INSERT_LIBRARIES",
    "GIT_SSH_COMMAND",
    "GIT_CONFIG_GLOBAL",
    "CRAZYAGENT_HOME",
    "LC_ALL",
    "XDG_CONFIG_DIRS",
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
    "PIP_INDEX_URL",
    "PIP_EXTRA_INDEX_URL",
    "PIP_TRUSTED_HOST",
    "PIP_CONFIG_FILE",
    "YARN_NPM_REGISTRY_SERVER",
    "YARN_RC_FILENAME",
    "BUN_CONFIG_REGISTRY",
    "OPENSSL_CONF",
    "OPENSSL_MODULES",
    "GEM_HOME",
    "GEM_PATH",
    "GOPROXY",
    "GOSUMDB",
    "GONOSUMDB",
    "GOINSECURE",
    "GOPRIVATE",
    "GONOPROXY",
    "GOTOOLCHAIN",
];

const ENGINE_FAMILY_PREFIXES: &[&str] = &[
    "NPM_",
    "PNPM_",
    "CARGO_",
    "RUSTC_",
    "RUST_",
    "RUSTUP_",
    "SSL_",
    "CODEX_",
    "CLAUDE_",
    "UV_",
    "COREPACK_",
    "HOMEBREW_",
    "PYTHON_",
    "PY_",
    "PERL5_",
    "RUBY_",
    "NODE_",
    "BASH_",
    "LD_",
    "DYLD_",
    "GIT_",
    "CRAZYAGENT_",
    "LC_",
    "XDG_",
    "JAVA_",
    "HTTP_",
    "HTTPS_",
    "ALL_",
    "NO_",
    "TERM_",
    "PATH_",
    "TMP_",
    "G_",
    "GO_",
    "LANG_",
    "TZ_",
    "SHELL_",
    "USER_",
    "CURL_",
    "REQUESTS_",
    "ELECTRON_",
    "PIP_",
    "PIP_CONFIG_",
    "YARN_",
    "BUN_",
    "OPENSSL_",
    "GEM_",
];

const HARNESS_NAMES: &[&str] = &[
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
    "OPENAI_MODEL",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_BASE_URL",
    "GEMINI_API_KEY",
    "GEMINI_MODEL",
    "GOOGLE_GEMINI_BASE_URL",
    "NO_COLOR",
    "QWEN_HOME",
    "KIMI_SHARE_DIR",
    "GEMINI_CLI_TRUST_WORKSPACE",
    "GOOSE_PATH_ROOT",
    "GOOSE_MODE",
    "FX_HOME",
    "COPILOT_HOME",
    "OPENCODE_CONFIG",
    "STANDIN_ACP_CONFIG",
    "SAMPLE_DIR",
    "OTHER_TOOL_MODE",
];

const HARNESS_PREFIXES: &[&str] = &[
    "QWEN_",
    "KIMI_",
    "GEMINI_",
    "GOOSE_",
    "FX_",
    "COPILOT_",
    "OPENCODE_",
    "PI_",
    "CRUSH_",
    "CLINE_",
    "DROID_",
    "AIDER_",
    "CONTINUE_",
    "OPENHANDS_",
    "OPENCLAW_",
    "OPENAI_",
    "ANTHROPIC_",
    "GOOGLE_",
    "STANDIN_ACP_",
    "SAMPLE_",
];

fn account_refusal(env: &str) -> Option<(&'static str, String)> {
    Registry::build(&[("rowharness", &format!("{LISTED}{env}"))])
        .err()
        .map(|refused| (refused.code, refused.message))
}

fn home_refusal(block: &str) -> Option<(&'static str, String)> {
    let manifest = parse(format!("{SMALL}home:\n{block}").as_bytes()).unwrap();
    home::read(&manifest)
        .err()
        .map(|refused| (refused.code, refused.message))
}

/// Env-name rule (2.4.2 backlog, rv-code T18 r2): every variable the engine sets on a
/// launch is a name no manifest may set, read from the engine's own lists, so neither an
/// account entry nor a home block can name one and the per-launch hand-over list of Task 20
/// never holds a pinned name.
#[test]
fn every_variable_the_engine_sets_on_a_launch_is_owned() {
    let identity = GitIdentity::new("Row".into(), "row@example.com".into()).unwrap();
    let mut set: Vec<String> = AGENT_GIT_ENV
        .iter()
        .chain(AGENT_HOME_ENV.iter())
        .chain(AGENT_TLS_ENV.iter())
        .chain(AUTO_INSTALL_ENV.iter())
        .map(|(name, _)| (*name).to_owned())
        .collect();
    set.extend(TOOL_CACHE_ENV.iter().map(|name| (*name).to_owned()));
    set.extend(
        model_proxy::variables(1)
            .iter()
            .map(|(name, _)| (*name).to_owned()),
    );
    set.extend(
        identity
            .variables()
            .iter()
            .map(|(name, _)| (*name).to_owned()),
    );
    set.push(LINEAGE_VARIABLE.to_owned());
    set.extend(
        [
            "HOME",
            "TMPDIR",
            "TMPPREFIX",
            "PATH",
            "SHELL",
            "SSL_CERT_FILE",
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
            "CLAUDE_SECURESTORAGE_CONFIG_DIR",
            "CLAUDE_CODE_TMPDIR",
        ]
        .map(str::to_owned),
    );
    assert!(set.len() >= 37, "{set:?}");

    let open: Vec<&String> = set.iter().filter(|name| !owned(name)).collect();

    assert!(
        open.is_empty(),
        "the engine sets these and a manifest may: {open:?}"
    );
}

/// Env-name rule: each family that decides which code a shell, a linker, git, a package
/// manager or a language runtime loads, or which server is trusted, is owned by every name
/// in it, and the engine's lower-case names count as their upper-case twins.
#[test]
fn every_family_that_decides_which_code_runs_or_which_server_is_trusted_is_owned() {
    for name in ENGINE_FAMILY_NAMES {
        assert!(owned(name), "{name}");
    }
    for name in [
        "pnpm_config_store_dir",
        "https_proxy",
        "npm_config_userconfig",
    ] {
        assert!(owned(name), "{name}");
    }
}

/// Env-name rule: no prefix covering an owned name or family can be claimed, in either
/// direction: a stem that starts an owned name (TERM_, NO_, G_) or family (PY_), and a
/// prefix that starts with a family (RUSTUP_, PERL5_).
#[test]
fn no_prefix_that_covers_an_owned_name_or_family_can_be_claimed() {
    for prefix in ENGINE_FAMILY_PREFIXES {
        assert!(claims(prefix), "{prefix}");
    }
}

/// Env-name rule, Task 20 compatibility: the model key, address and model names a listed
/// harness reads stay open, as do the names and prefixes the Phase 3 harnesses read, so the
/// rule refuses no hand-over of Task 17 or Task 20 and no planned manifest.
#[test]
fn model_names_and_harness_prefixes_stay_open() {
    for name in HARNESS_NAMES {
        assert!(!owned(name), "{name}");
    }
    for prefix in HARNESS_PREFIXES {
        assert!(!claims(prefix), "{prefix}");
    }
}

/// Env-name rule at the account site: an `accounts` entry cannot set a name in an engine
/// family, with the sentence it already gives, and keeps its model names with the three
/// hand-over placeholders.
#[test]
fn an_account_entry_cannot_set_an_owned_name_and_keeps_its_model_names() {
    for name in [
        "NPM_CONFIG_USERCONFIG",
        "SSL_CERT_FILE",
        "CODEX_HOME",
        "CARGO_BUILD_RUSTC_WRAPPER",
        "PYTHONNOUSERSITE",
        "BASH_ENV",
        "HOMEBREW_PREFIX",
    ] {
        assert_eq!(
            account_refusal(&format!("      {name}: \"1\"\n")),
            Some((
                VALUE,
                format!(
                    "rowharness: accounts.openai-chat.env: {name} is not a variable an account entry may set."
                )
            )),
            "{name}"
        );
    }

    let kept = concat!(
        "      OPENAI_BASE_URL: \"{relay_url}\"\n",
        "      OPENAI_API_KEY: \"{token}\"\n",
        "      OPENAI_MODEL: \"{model}\"\n",
        "      NO_COLOR: \"1\"\n",
    );

    assert_eq!(account_refusal(kept), None);
}

/// Env-name rule at the home site: `home.env_prefixes` cannot claim an engine family and
/// `home.env` cannot set one of its names, each with the sentence it already gives, while
/// harness prefixes and the XDG folders under the private home stay open.
#[test]
fn a_home_block_cannot_claim_an_engine_family_or_set_one_of_its_names() {
    for prefix in [
        "NPM_", "CARGO_", "SSL_", "CODEX_", "CLAUDE_", "UV_", "HTTPS_", "RUSTC_",
    ] {
        assert_eq!(
            home_refusal(&format!("  env_prefixes:\n    - \"{prefix}\"\n")),
            Some((VALUE, format!("home.env_prefixes.{prefix}: {COVERS}"))),
            "{prefix}"
        );
    }
    for name in ["SSL_CERT_FILE", "NPM_CONFIG_PREFIX", "CODEX_HOME"] {
        assert_eq!(
            home_refusal(&format!(
                "  env_prefixes:\n    - \"SAMPLE_\"\n  env:\n    {name}: \"1\"\n"
            )),
            Some((
                VALUE,
                format!("home.env.{name}: CrazyAgent sets this itself, so a manifest cannot.")
            )),
            "{name}"
        );
    }
    for prefix in ["OPENCODE_", "GOOSE_", "QWEN_", "FX_"] {
        assert_eq!(
            home_refusal(&format!("  env_prefixes:\n    - \"{prefix}\"\n")),
            None,
            "{prefix}"
        );
    }

    let xdg =
        "  env_prefixes:\n    - \"SAMPLE_\"\n  env:\n    XDG_CONFIG_HOME: \"{home}/config\"\n";

    assert_eq!(home_refusal(xdg), None);
}

/// Env-name rule, supply chain (rv-sec3 envrule F3): the package index, module proxy, gem
/// path and OpenSSL families decide which server an install trusts or which code loads, so
/// an account entry cannot set them and a home block cannot claim them; `PIP_` sits inside
/// the open `PI_` prefix, which keeps its claim but cannot set a `PIP_` name under it.
#[test]
fn the_package_index_module_proxy_and_openssl_families_are_owned_at_both_sites() {
    for name in [
        "PIP_INDEX_URL",
        "GOPROXY",
        "GOSUMDB",
        "YARN_NPM_REGISTRY_SERVER",
        "BUN_CONFIG_REGISTRY",
        "OPENSSL_CONF",
        "GEM_PATH",
    ] {
        assert_eq!(
            account_refusal(&format!("      {name}: \"https://mirror.example\"\n")),
            Some((
                VALUE,
                format!(
                    "rowharness: accounts.openai-chat.env: {name} is not a variable an account entry may set."
                )
            )),
            "{name}"
        );
    }
    for prefix in ["PIP_", "YARN_", "BUN_", "OPENSSL_", "GEM_"] {
        assert_eq!(
            home_refusal(&format!("  env_prefixes:\n    - \"{prefix}\"\n")),
            Some((VALUE, format!("home.env_prefixes.{prefix}: {COVERS}"))),
            "{prefix}"
        );
    }

    let inside_pi =
        "  env_prefixes:\n    - \"PI_\"\n  env:\n    PIP_INDEX_URL: \"https://mirror.example\"\n";

    assert_eq!(
        home_refusal(inside_pi),
        Some((
            VALUE,
            "home.env.PIP_INDEX_URL: CrazyAgent sets this itself, so a manifest cannot.".to_owned()
        ))
    );
    assert_eq!(
        home_refusal("  env_prefixes:\n    - \"PI_\"\n  env:\n    PI_HOME: \"{home}\"\n"),
        None
    );
}
