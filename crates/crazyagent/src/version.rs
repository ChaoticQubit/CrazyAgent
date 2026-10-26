pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const COMMIT_CHARS: usize = 7;
const ENGINE_VERSION_BYTES_MAX: usize = 64;

/// Names the release channel a version belongs to. Input: a semver string.
/// Output: `dev` for the rolling prerelease builds (`<next>-dev.<stamp>`), else `stable`.
pub fn channel_of(version: &str) -> &'static str {
    if version.contains("-dev.") {
        "dev"
    } else {
        "stable"
    }
}

/// Shortens a build commit to the first seven hex digits. Input: the commit the release
/// build recorded, if any. Output: the short commit, or `unknown` for a missing or
/// malformed one.
pub fn short_commit(commit: Option<&str>) -> &str {
    match commit {
        Some(sha)
            if sha.len() >= COMMIT_CHARS && sha.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            &sha[..COMMIT_CHARS]
        }
        _ => "unknown",
    }
}

/// The commit this binary was built from: `CRAZYAGENT_COMMIT` set by the release build,
/// else the `GITHUB_SHA` of a CI build. Output: seven hex digits or `unknown`.
pub fn commit() -> &'static str {
    short_commit(option_env!("CRAZYAGENT_COMMIT").or(option_env!("GITHUB_SHA")))
}

/// The line `crazyagent --version` prints. Output:
/// `crazyagent <semver> (<channel>, <git short sha>)`.
pub fn line() -> String {
    let line = format!(
        "crazyagent {VERSION} ({}, {})",
        channel_of(VERSION),
        commit()
    );
    assert!(line.starts_with("crazyagent "));
    line
}

/// Checks the version an engine reported against this app's own. Input: the engine's
/// version, empty when an older engine did not report one. Output: the plain message to
/// show when they differ, or none when they match.
pub fn mismatch(engine: &str) -> Option<String> {
    if engine == VERSION {
        return None;
    }
    let shown = if engine.is_empty()
        || engine.len() > ENGINE_VERSION_BYTES_MAX
        || engine.chars().any(char::is_control)
    {
        "unknown"
    } else {
        engine
    };
    Some(format!(
        "Engine is version {shown}, this app is {VERSION}; restart the engine"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_prereleases_are_the_dev_channel_and_everything_else_is_stable() {
        assert_eq!(channel_of("0.2.0-dev.202610251600"), "dev");
        assert_eq!(channel_of("0.2.0"), "stable");
        assert_eq!(channel_of("0.3.1"), "stable");
    }

    #[test]
    fn short_commit_keeps_seven_hex_digits_and_names_anything_else_unknown() {
        assert_eq!(
            short_commit(Some("672b1db0e26662d6e2462aa0011223344556677")),
            "672b1db"
        );
        assert_eq!(short_commit(Some("672b1db")), "672b1db");
        assert_eq!(short_commit(Some("672b1")), "unknown");
        assert_eq!(short_commit(Some("not-a-sha-at-all")), "unknown");
        assert_eq!(short_commit(None), "unknown");
    }

    #[test]
    fn version_line_names_semver_channel_and_commit() {
        let line = line();
        let expected = format!("crazyagent {VERSION} (");
        assert!(line.starts_with(&expected), "{line}");
        let inside = line[expected.len()..].strip_suffix(')').unwrap();
        let (channel, commit) = inside.split_once(", ").unwrap();
        assert_eq!(channel, channel_of(VERSION));
        assert!(
            commit == "unknown"
                || (commit.len() == 7 && commit.bytes().all(|b| b.is_ascii_hexdigit())),
            "{line}"
        );
    }

    #[test]
    fn mismatch_names_both_versions_and_is_silent_when_equal() {
        assert_eq!(mismatch(VERSION), None);
        assert_eq!(
            mismatch("0.0.1").unwrap(),
            format!("Engine is version 0.0.1, this app is {VERSION}; restart the engine")
        );
        assert_eq!(
            mismatch("").unwrap(),
            format!("Engine is version unknown, this app is {VERSION}; restart the engine")
        );
        assert_eq!(
            mismatch("0.1.0\n\u{1b}[2J").unwrap(),
            format!("Engine is version unknown, this app is {VERSION}; restart the engine")
        );
    }
}
