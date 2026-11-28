use super::VERSION_MIN;

fn release(version: &str) -> Option<(u32, u32)> {
    let version = version.trim();
    let version = version.strip_prefix("next-").unwrap_or(version);
    let (major, rest) = version.split_once('.')?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Some((major.parse::<u32>().ok()?, digits.parse::<u32>().ok()?))
}

/// Whether agent terminals support a tmux version, the second word of `tmux -V`: 3.7 or
/// later ([`VERSION_MIN`]). Input: the version. Output: the answer, or none when it is not
/// a release number.
pub fn supported(version: &str) -> Option<bool> {
    release(version).map(|found| found >= VERSION_MIN)
}

/// The oldest tmux version agent terminals support, as setup names it. Output: the
/// version, such as "3.7".
pub fn version_needed() -> String {
    format!("{}.{}", VERSION_MIN.0, VERSION_MIN.1)
}
