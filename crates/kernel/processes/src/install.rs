/// The most bytes of a setup package, Homebrew, npm, Winget or program name.
pub const NAME_BYTES_MAX: usize = 214;
/// The most bytes of a version.
pub const VERSION_BYTES_MAX: usize = 64;
/// The most bytes of a hash-locked requirements file.
pub const LOCK_BYTES_MAX: usize = 262_144;
/// The most bytes of a target triple.
pub const TARGET_BYTES_MAX: usize = 128;
/// The most bytes of a Python version.
pub const PYTHON_BYTES_MAX: usize = 128;
/// The most arguments of a self-update.
pub const ARGS_MAX: usize = 5;
/// The most vendor flags of an npm install.
pub const NPM_FLAGS_MAX: usize = 1;
/// The characters of a SHA-256 written in hex.
pub const SHA256_HEX_LEN: usize = 64;

/// Every target triple a setup download names.
pub const TARGETS: [&str; 6] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
    "aarch64-pc-windows-msvc",
    "x86_64-pc-windows-msvc",
];

/// The vendor npm flags the bundled harness manifests carry, the only ones an npm install
/// passes.
pub const NPM_FLAGS_ALLOWED: [&str; 2] = ["--allow-scripts=openclaw", "--ignore-scripts"];

/// The self-update argument lists the bundled harness manifests carry, the only ones a
/// self-update runs.
pub const SELF_UPDATE_ARGS_ALLOWED: [&[&str]; 1] = [&["update"]];

const _: () = assert!(NPM_FLAGS_MAX + 3 <= ARGS_MAX);
const _: () = assert!(SHA256_HEX_LEN == 64);

/// The endings npm reads as an archive file instead of a package name or a version.
const ARCHIVES: [&str; 3] = [".tgz", ".tar", ".gz"];

/// A name refused for its shape or length.
pub const NAME_INVALID: &str = "install_name_invalid";
/// A version refused for its shape or length.
pub const VERSION_INVALID: &str = "install_version_invalid";
/// A target triple no setup download names, or a Python version refused.
pub const TARGET_INVALID: &str = "install_target_invalid";
/// A SHA-256 that is not 64 lower-case hex characters.
pub const HASH_INVALID: &str = "install_hash_invalid";
/// A requirements file over [`LOCK_BYTES_MAX`].
pub const LOCK_TOO_LARGE: &str = "install_lock_too_large";
/// Arguments no bundled recipe sends.
pub const TEMPLATE_MISMATCH: &str = "install_template_mismatch";
/// An npm flag not on [`NPM_FLAGS_ALLOWED`].
pub const FLAG_UNLISTED: &str = "install_flag_unlisted";

/// Whether an install adds a package or updates one already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Install,
    Update,
}

/// The Linux package manager a system package install runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemManager {
    AptGet,
    Dnf,
}

/// One pinned installer run, one variant per installer the engine runs today, typed fields
/// only. Inputs: the variant's fields. Output: a value validation accepts or refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallRecipe {
    UvRelease {
        version: String,
        target: String,
        sha256: String,
    },
    UvTool {
        package: String,
        version: String,
        python: String,
        lock: String,
    },
    NodeArchive {
        version: String,
        target: String,
        sha256: String,
    },
    BrewFormula {
        name: String,
        verb: Verb,
    },
    BrewCask {
        name: String,
        verb: Verb,
    },
    NpmGlobal {
        package: String,
        version: String,
        verb: Verb,
        flags: Vec<String>,
    },
    Winget {
        id: String,
        version: String,
    },
    SystemPackage {
        manager: SystemManager,
        package: String,
    },
    SelfUpdate {
        program: String,
        args: Vec<String>,
    },
}

/// What one install run gave back: its exit code, the last bytes of its standard output and
/// the last bytes of its standard error, each bounded by the runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallResult {
    pub code: u32,
    pub output: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Whether a text is a plain harness name: it starts with a lower-case letter or a digit
/// and holds only lower-case letters, digits and `@ . _ -`. Input: the text. Output: the
/// answer.
pub fn plain(text: &str) -> bool {
    text.as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && text.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"@._-".contains(&byte)
        })
}

/// Whether a text is one part of an npm package name, or a version or tag after it, that
/// npm can only read as a registry name: plain characters without `@`, and no archive ending.
/// Input: the text. Output: true when npm cannot read it as a folder, a file or an address.
pub fn part(text: &str) -> bool {
    plain(text) && !text.contains('@') && !ARCHIVES.iter().any(|end| text.ends_with(end))
}

/// Whether a text is an npm scope such as `@vendor`. Input: the text. Output: the answer.
pub fn scope_name(text: &str) -> bool {
    text.strip_prefix('@').is_some_and(part)
}

/// Whether a text is a long option of lower-case letters, digits, `-` and `=`. Input: the
/// text. Output: the answer.
pub fn option(text: &str) -> bool {
    text.starts_with("--")
        && text.len() > 2
        && text.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'='
        })
}

/// Whether a text is a word of lower-case letters, digits and dashes that does not start
/// with a dash. Input: the text. Output: the answer.
pub fn word(text: &str) -> bool {
    !text.starts_with('-')
        && text
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Whether a text is a setup package name: at most [`NAME_BYTES_MAX`] bytes, a lower-case
/// letter or digit first, then lower-case letters, digits and `. _ -`. Input: the text.
/// Output: the answer.
fn setup_name(text: &str) -> bool {
    text.len() <= NAME_BYTES_MAX
        && text
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && text.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
}

/// Whether a system package name names one package to its manager: dnf reads a last `-`
/// segment that starts with a digit as a version or release, and apt-get reads a `.` as a
/// pattern. Real names of these shapes, such as `xorg-x11-fonts-75dpi` for dnf, are refused
/// too. Inputs: the manager and the name. Output: the answer.
fn one_package(manager: SystemManager, package: &str) -> bool {
    match manager {
        SystemManager::Dnf => package
            .rsplit_once('-')
            .is_none_or(|(_, last)| !last.starts_with(|first: char| first.is_ascii_digit())),
        SystemManager::AptGet => !package.contains('.'),
    }
}

/// Whether a text is a Winget id: at most [`NAME_BYTES_MAX`] bytes, a letter or digit
/// first, then letters, digits, `.` and `-`. Input: the text. Output: the answer.
fn winget_id(text: &str) -> bool {
    text.len() <= NAME_BYTES_MAX
        && text
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
}

/// Whether an npm package is a plain name or `@scope/name`, at most [`NAME_BYTES_MAX`]
/// bytes. Input: the text. Output: the answer.
fn npm_package(text: &str) -> bool {
    text.len() <= NAME_BYTES_MAX
        && match text.split_once('/') {
            Some((scope, name)) => scope_name(scope) && part(name),
            None => part(text),
        }
}

/// Whether a text is a version: 1 to [`VERSION_BYTES_MAX`] bytes of letters, digits and
/// `. + _ -`, not starting with `-`. Input: the text. Output: the answer.
fn version(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= VERSION_BYTES_MAX
        && !text.starts_with('-')
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".+_-".contains(&byte))
}

/// Whether a text is a SHA-256 of 64 lower-case hex characters. Input: the text. Output:
/// the answer.
fn sha256(text: &str) -> bool {
    text.len() == SHA256_HEX_LEN
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Whether a text is a target triple a setup download names, at most [`TARGET_BYTES_MAX`]
/// bytes. Input: the text. Output: the answer.
fn target(text: &str) -> bool {
    text.len() <= TARGET_BYTES_MAX && TARGETS.contains(&text)
}

/// Whether a text is a Python version of 1 to [`PYTHON_BYTES_MAX`] bytes of digits and
/// dots. Input: the text. Output: the answer.
fn python(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= PYTHON_BYTES_MAX
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
}

/// The download fields of a uv release or a Node archive. Inputs: the fields. Output:
/// nothing, or the refusal code.
fn download(version_text: &str, target_text: &str, hash: &str) -> Result<(), &'static str> {
    if !version(version_text) {
        return Err(VERSION_INVALID);
    }
    if !target(target_text) {
        return Err(TARGET_INVALID);
    }
    if !sha256(hash) {
        return Err(HASH_INVALID);
    }
    Ok(())
}

/// Checks every field of a recipe against its variant's bounds and the bundled lists, so no
/// field can add an argument, start an option or name a file. An npm recipe with an empty
/// version is a bare spec. Input: the recipe. Output: nothing, or the refusal code.
pub fn validate(recipe: &InstallRecipe) -> Result<(), &'static str> {
    match recipe {
        InstallRecipe::UvRelease {
            version: pinned,
            target,
            sha256,
        }
        | InstallRecipe::NodeArchive {
            version: pinned,
            target,
            sha256,
        } => download(pinned, target, sha256),
        InstallRecipe::UvTool {
            package,
            version: pinned,
            python: wanted,
            lock,
        } => {
            if !setup_name(package) {
                return Err(NAME_INVALID);
            }
            if !version(pinned) {
                return Err(VERSION_INVALID);
            }
            if !python(wanted) {
                return Err(TARGET_INVALID);
            }
            if lock.len() > LOCK_BYTES_MAX {
                return Err(LOCK_TOO_LARGE);
            }
            Ok(())
        }
        InstallRecipe::BrewFormula { name, .. } | InstallRecipe::BrewCask { name, .. } => {
            (name.len() <= NAME_BYTES_MAX && plain(name))
                .then_some(())
                .ok_or(NAME_INVALID)
        }
        InstallRecipe::NpmGlobal {
            package,
            version: pinned,
            flags,
            ..
        } => {
            if !npm_package(package) {
                return Err(NAME_INVALID);
            }
            if !(pinned.is_empty() || (version(pinned) && part(pinned))) {
                return Err(VERSION_INVALID);
            }
            if flags.len() > NPM_FLAGS_MAX || !flags.iter().all(|flag| option(flag)) {
                return Err(TEMPLATE_MISMATCH);
            }
            if !flags
                .iter()
                .all(|flag| NPM_FLAGS_ALLOWED.contains(&flag.as_str()))
            {
                return Err(FLAG_UNLISTED);
            }
            Ok(())
        }
        InstallRecipe::Winget {
            id,
            version: pinned,
        } => {
            if !winget_id(id) {
                return Err(NAME_INVALID);
            }
            version(pinned).then_some(()).ok_or(VERSION_INVALID)
        }
        InstallRecipe::SystemPackage { manager, package } => {
            let plain_end = package
                .bytes()
                .last()
                .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
            (setup_name(package)
                && plain_end
                && !package.ends_with(".rpm")
                && one_package(*manager, package))
            .then_some(())
            .ok_or(NAME_INVALID)
        }
        InstallRecipe::SelfUpdate { program, args } => {
            if !(program.len() <= NAME_BYTES_MAX && plain(program)) {
                return Err(NAME_INVALID);
            }
            let listed = args.len() <= ARGS_MAX
                && args.iter().all(|arg| word(arg))
                && SELF_UPDATE_ARGS_ALLOWED
                    .iter()
                    .any(|allowed| allowed.iter().copied().eq(args.iter().map(String::as_str)));
            listed.then_some(()).ok_or(TEMPLATE_MISMATCH)
        }
    }
}

mod steps;
#[cfg(unix)]
pub use steps::run;
pub use steps::{
    ARCHIVE_BYTES_MAX, CURL, DOWNLOAD_SECS, InstallPaths, MEMBER_BYTES_MAX, SOURCE_BYTES_MAX, Step,
    TAR, steps,
};

#[cfg(test)]
mod tests;
