/// Names a manifest may never set in `accounts.<format>.env` or `home.env`, and no
/// `home.env_prefixes` entry may cover: the engine sets them on a launch, or they decide which
/// shell setup runs, which code a runtime or its plugins load, which command a tool starts,
/// where TLS keys are written or which server an install trusts (rv-sa241 N1).
const NAMES: [&str; 54] = [
    "PATH",
    "HOME",
    "SHELL",
    "TMPDIR",
    "TMPPREFIX",
    "SDKROOT",
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
    "GOPROXY",
    "GOSUMDB",
    "GONOSUMDB",
    "GOINSECURE",
    "GOPRIVATE",
    "GONOPROXY",
    "GOTOOLCHAIN",
    "CURL_CA_BUNDLE",
    "REQUESTS_CA_BUNDLE",
    "GCONV_PATH",
    "PHPRC",
    "PHP_INI_SCAN_DIR",
    "DOTNET_STARTUP_HOOKS",
    "GTK_MODULES",
    "QT_PLUGIN_PATH",
    "GIO_EXTRA_MODULES",
    "TCLLIBPATH",
    "R_PROFILE",
    "R_PROFILE_USER",
    "JULIA_LOAD_PATH",
    "FPATH",
    "PAGER",
    "MANPAGER",
    "LESSOPEN",
    "LESSCLOSE",
    "EDITOR",
    "VISUAL",
    "BROWSER",
    "SSLKEYLOGFILE",
];

/// Starts of names in the families owned the same way as [`NAMES`]: the engine's own, the
/// dynamic linkers, git, TLS and OpenSSL, the package managers and toolchains the engine pins
/// or that pick an install's server, the native harness homes, the locale and XDG folders,
/// and the language runtimes and shells that load code from a variable. `home.env` may still
/// set an XDG_ folder to the private home or a folder inside it (`home::variables`).
const STARTS: [&str; 29] = [
    "CRAZYAGENT_",
    "DYLD_",
    "LD_",
    "GIT_",
    "SSL_",
    "OPENSSL_",
    "NPM_",
    "PNPM_",
    "YARN_",
    "BUN_",
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
    "GEM_",
    "NODE_",
    "BASH_",
    "CORECLR_",
    "LUA_INIT",
    "GST_PLUGIN_",
    "ERL_",
];

/// Starts owned like [`STARTS`] that lie inside a prefix a harness keeps open (`PIP_` inside
/// `PI_`): a prefix that begins with one is claimed, while the open prefix stays claimable
/// and still cannot set a name of the family.
const INNER_STARTS: [&str; 1] = ["PIP_"];

/// Names that pick the owner's own compilers, linker and build tools, their flags, header
/// and library paths and the toolchain: the owner may give an account one as a toolchain
/// choice, and the launch guard lets it through, but no manifest may set one in
/// `accounts.<format>.env` or `home.env`, nor claim one with a `home.env_prefixes` entry
/// (leader 01:34, widened 02:00 with the same-role names, rv-code4 F1, rv-sa241 LOW-1).
const OWNER_TOOLCHAIN: [&str; 24] = [
    "DEVELOPER_DIR",
    "CC",
    "CXX",
    "CFLAGS",
    "LDFLAGS",
    "CPATH",
    "LIBRARY_PATH",
    "PKG_CONFIG_PATH",
    "CXXFLAGS",
    "CPPFLAGS",
    "C_INCLUDE_PATH",
    "TOOLCHAINS",
    "PKG_CONFIG_LIBDIR",
    "PKG_CONFIG_SYSROOT_DIR",
    "AR",
    "LD",
    "AS",
    "CPP",
    "RANLIB",
    "CONFIG_SHELL",
    "LDLIBS",
    "MAKEFLAGS",
    "MACOSX_DEPLOYMENT_TARGET",
    "GOROOT",
];

/// Starts of names in the families owned the same way as [`OWNER_TOOLCHAIN`]: the C++ and
/// Objective-C header paths, the host and target tools and flags build scripts read, and the
/// compiler cache's, cgo's and CMake's settings.
const OWNER_TOOLCHAIN_STARTS: [&str; 7] = [
    "CPLUS_", "OBJC_", "HOST_", "TARGET_", "CCACHE_", "CGO_", "CMAKE_",
];

/// The words a manifest variable naming one of [`OWNER_TOOLCHAIN`] is refused with, after
/// its setting and name: an account entry's and a home block's refusal end the same way, so
/// the owner reads one reason.
pub(super) const PICKS: &str =
    "picks the owner's compiler or build tools, so only the owner sets it.";

/// The words a `home.env_prefixes` entry covering one of [`OWNER_TOOLCHAIN`] is refused
/// with, after its setting: they name the owner's toolchain, not the engine, as the reason
/// the prefix cannot be claimed.
pub(super) const COVERS_TOOLCHAIN: &str =
    "this prefix covers names that pick the owner's compiler or build tools.";

/// Whether a manifest may not set a variable of this name in `accounts.<format>.env`, or in
/// `home.env` unless it is an XDG_ folder there: the engine sets it on a launch, or it decides
/// which shell setup runs, which code a runtime loads or which server an install trusts.
/// Compared upper-cased, so the engine's lower-case names count too. Input: the name.
/// Output: the answer.
pub(super) fn owned(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    NAMES.contains(&upper.as_str())
        || STARTS
            .iter()
            .chain(INNER_STARTS.iter())
            .any(|start| upper.starts_with(start))
}

/// Whether a `home.env_prefixes` entry would cover a name the engine owns, in either
/// direction: an owned name starts with its stem, or an owned family starts with its stem
/// or begins the prefix; an inner family counts only when it begins the prefix. Input: a
/// prefix of capital letters, digits and `_` that ends with `_`. Output: the answer.
pub(super) fn claims(prefix: &str) -> bool {
    assert!(prefix.len() > 1 && prefix.ends_with('_'));
    let stem = &prefix[..prefix.len() - 1];
    NAMES.iter().any(|name| name.starts_with(stem))
        || STARTS
            .iter()
            .any(|start| start.starts_with(stem) || prefix.starts_with(start))
        || INNER_STARTS.iter().any(|start| prefix.starts_with(start))
}

/// Whether a name is one of the owner's toolchain names ([`OWNER_TOOLCHAIN`]) or in one of
/// their families ([`OWNER_TOOLCHAIN_STARTS`]), which a manifest may not set. Compared
/// upper-cased, as [`owned`] is. Input: the name. Output: the answer.
pub(super) fn owner_toolchain(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    OWNER_TOOLCHAIN.contains(&upper.as_str())
        || OWNER_TOOLCHAIN_STARTS
            .iter()
            .any(|start| upper.starts_with(start))
}

/// Whether a `home.env_prefixes` entry would cover one of the owner's toolchain names, in
/// either direction as [`claims`] reads the owned ones: a name or family starts with the
/// prefix's stem, or the prefix starts with a family. Input: a prefix of capital letters,
/// digits and `_` that ends with `_`. Output: the answer.
pub(super) fn claims_owner_toolchain(prefix: &str) -> bool {
    assert!(prefix.len() > 1 && prefix.ends_with('_'));
    let stem = &prefix[..prefix.len() - 1];
    OWNER_TOOLCHAIN.iter().any(|name| name.starts_with(stem))
        || OWNER_TOOLCHAIN_STARTS
            .iter()
            .any(|start| start.starts_with(stem) || prefix.starts_with(start))
}

#[cfg(test)]
#[path = "env_names_tests.rs"]
mod tests;
