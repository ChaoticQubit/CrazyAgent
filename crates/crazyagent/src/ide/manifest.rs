use crate::engine::EngineError;
use serde::Deserialize;
use std::collections::BTreeMap;

const BUILDS: &str = include_str!("builds.json");
const RELEASES: &str = "https://github.com/VSCodium/vscodium/releases/download/";
const VERSION_BYTES_MAX: usize = 32;
const PLATFORMS: [(&str, &str); 5] = [
    ("aarch64-apple-darwin", "darwin-arm64"),
    ("x86_64-apple-darwin", "darwin-x64"),
    ("x86_64-unknown-linux-gnu", "linux-x64"),
    ("aarch64-unknown-linux-gnu", "linux-arm64"),
    ("x86_64-pc-windows-msvc", "win32-x64"),
];

/// The largest IDE build archive the engine downloads.
pub const BYTES_MAX: u64 = 150 * 1024 * 1024;

/// One pinned IDE build for one platform: its version, official release URL, sha256 and
/// exact archive size, which is also the download cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub size_max: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: String,
    builds: BTreeMap<String, Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    url: String,
    sha256: String,
    bytes: u64,
}

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

/// Tells whether a version is dot-separated decimal numbers, safe as one folder name.
/// Input: the version. Output: the answer.
pub(crate) fn version_valid(version: &str) -> bool {
    version.len() <= VERSION_BYTES_MAX
        && version
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn sha256_valid(sha256: &str) -> bool {
    sha256.len() == 64
        && sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Reads the build for one platform from a build list. The URL must be exactly the
/// official VSCodium release download for that version and platform.
/// Inputs: the list as JSON and the platform's Rust target. Output: the build,
/// `ide_build_manifest_invalid`, `ide_build_unsupported_platform` or
/// `ide_build_url_refused`.
pub fn parse(text: &str, target: &str) -> Result<Build, EngineError> {
    let manifest: Manifest =
        serde_json::from_str(text).map_err(|_| failure("ide_build_manifest_invalid"))?;
    if !version_valid(&manifest.version) {
        return Err(failure("ide_build_manifest_invalid"));
    }
    let (Some(entry), Some((_, platform))) = (
        manifest.builds.get(target),
        PLATFORMS.iter().find(|(known, _)| *known == target),
    ) else {
        return Err(failure("ide_build_unsupported_platform"));
    };
    if !sha256_valid(&entry.sha256) || entry.bytes == 0 || entry.bytes > BYTES_MAX {
        return Err(failure("ide_build_manifest_invalid"));
    }
    let version = &manifest.version;
    let official = format!("{RELEASES}{version}/vscodium-reh-web-{platform}-{version}.tar.gz");
    if entry.url != official {
        return Err(failure("ide_build_url_refused"));
    }
    Ok(Build {
        version: manifest.version.clone(),
        url: official,
        sha256: entry.sha256.clone(),
        size_max: entry.bytes,
    })
}

/// The pinned build for one platform from the build list shipped in the engine.
/// Input: the platform's Rust target. Output: the build or
/// `ide_build_unsupported_platform`.
pub fn current(target: &str) -> Result<Build, EngineError> {
    parse(BUILDS, target)
}
