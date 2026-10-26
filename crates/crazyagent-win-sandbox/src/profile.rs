use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// The first line of every boundary profile, naming its format and version.
pub const HEADER: &str = "crazyagent-win-boundary 2";
/// The largest profile file the helper reads.
pub const PROFILE_BYTES_MAX: usize = 64 * 1024;
/// The longest path a profile may name, in bytes.
pub const PATH_BYTES_MAX: usize = 1024;
/// The longest AppContainer name Windows accepts.
pub const CONTAINER_CHARS_MAX: usize = 64;
/// The most read-only roots one profile grants.
pub const READ_ROOTS_MAX: usize = 32;
/// The most writable roots one profile grants.
pub const WRITE_ROOTS_MAX: usize = 16;
/// The most protected paths one profile names.
pub const GUARDED_MAX: usize = 64;
/// The most processes one agent's job may run at once.
pub const ACTIVE_PROCESSES_MAX: u32 = 4096;

/// A capability the agent's container starts with. `internetClient` is the only one: it opens
/// outbound internet access, and an agent without it has no network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    InternetClient,
}

impl Capability {
    /// The capability's name in a profile line. Input: the capability. Output: its name.
    pub fn name(self) -> &'static str {
        match self {
            Self::InternetClient => "internetClient",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        (text == "internetClient").then_some(Self::InternetClient)
    }
}

/// How a protected path is kept from the agent: read-only (no writes, renames or deletes) or
/// hidden (no access at all).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guard {
    ReadOnly,
    Hidden,
}

impl Guard {
    fn keyword(self) -> &'static str {
        match self {
            Self::ReadOnly => "readonly",
            Self::Hidden => "hidden",
        }
    }
}

/// One protected path and how it is guarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guarded {
    pub path: String,
    pub guard: Guard,
}

/// Everything the boundary helper needs to confine one agent: its AppContainer name, the
/// capabilities it starts with, the folders it may read and write, the paths kept read-only
/// or hidden inside them, its job's process limit and the engine folder where it records the
/// access entries it grants each container. Paths are plain drive paths as text, so the
/// format reads and writes the same on every system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundaryProfile {
    pub container: String,
    pub capabilities: Vec<Capability>,
    pub read_roots: Vec<String>,
    pub write_roots: Vec<String>,
    pub guarded: Vec<Guarded>,
    pub active_processes: u32,
    pub grants: String,
}

/// Why a boundary profile was refused: the line it failed on (0 for the whole profile) and a
/// plain reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileError {
    pub line: usize,
    pub reason: &'static str,
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(f, "boundary profile refused: {}", self.reason)
        } else {
            write!(
                f,
                "boundary profile refused at line {}: {}",
                self.line, self.reason
            )
        }
    }
}

impl std::error::Error for ProfileError {}

fn refused(line: usize, reason: &'static str) -> ProfileError {
    ProfileError { line, reason }
}

/// Checks one path a profile names: a drive letter, a colon and a backslash, then parts split
/// by backslashes that are not empty, `.` or `..`, do not end in a space or dot, and hold no
/// control character or `< > : " / | ? *`. A bare drive root is refused. Input: the path text.
/// Output: Ok, or the plain reason.
pub fn check_path(text: &str) -> Result<(), &'static str> {
    if text.len() > PATH_BYTES_MAX {
        return Err("a path is longer than 1024 bytes");
    }
    let bytes = text.as_bytes();
    if bytes.len() < 4 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\' {
        return Err("a path is not a drive path such as C:\\folder");
    }
    for part in text[3..].split('\\') {
        if part.is_empty() {
            return Err("a path has an empty part or ends in a backslash");
        }
        if part == "." || part == ".." {
            return Err("a path holds . or ..");
        }
        if part.ends_with(' ') || part.ends_with('.') {
            return Err("a path part ends in a space or a dot");
        }
        if part
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '|' | '?' | '*'))
        {
            return Err("a path holds a character Windows does not allow in names");
        }
    }
    Ok(())
}

/// Checks an AppContainer name: 1 to 64 ASCII letters, digits, dots or dashes, starting with a
/// letter or digit. Input: the name. Output: Ok, or the plain reason.
pub fn check_container(name: &str) -> Result<(), &'static str> {
    let valid = (1..=CONTAINER_CHARS_MAX).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if valid {
        Ok(())
    } else {
        Err("the container name must be 1 to 64 letters, digits, dots or dashes")
    }
}

impl BoundaryProfile {
    /// Checks every bound and value: a valid container name, no repeated capability, 0 to 32
    /// read roots, 1 to 16 writable roots, at most 64 protected paths, every path and the
    /// grants folder a plain drive path, and 1 to 4096 active processes. Output: Ok, or the
    /// refusal.
    pub fn validate(&self) -> Result<(), ProfileError> {
        check_container(&self.container).map_err(|reason| refused(0, reason))?;
        for (at, capability) in self.capabilities.iter().enumerate() {
            if self.capabilities[..at].contains(capability) {
                return Err(refused(0, "a capability is listed twice"));
            }
        }
        if self.read_roots.len() > READ_ROOTS_MAX {
            return Err(refused(0, "too many read roots"));
        }
        if self.write_roots.is_empty() || self.write_roots.len() > WRITE_ROOTS_MAX {
            return Err(refused(0, "writable roots must be 1 to 16 folders"));
        }
        if self.guarded.len() > GUARDED_MAX {
            return Err(refused(0, "too many protected paths"));
        }
        for path in self
            .read_roots
            .iter()
            .chain(&self.write_roots)
            .chain(self.guarded.iter().map(|item| &item.path))
            .chain(std::iter::once(&self.grants))
        {
            check_path(path).map_err(|reason| refused(0, reason))?;
        }
        if !(1..=ACTIVE_PROCESSES_MAX).contains(&self.active_processes) {
            return Err(refused(0, "the process limit must be 1 to 4096"));
        }
        Ok(())
    }

    /// Writes the profile as text: the header, then one `keyword value` line per entry, each
    /// ending in a newline. Output: the text, or the refusal when the profile is invalid or
    /// larger than 64 KiB.
    pub fn encode(&self) -> Result<String, ProfileError> {
        self.validate()?;
        let mut text = format!(
            "{HEADER}\ncontainer {}\ngrants {}\n",
            self.container, self.grants
        );
        for capability in &self.capabilities {
            text.push_str(&format!("capability {}\n", capability.name()));
        }
        for root in &self.read_roots {
            text.push_str(&format!("read {root}\n"));
        }
        for root in &self.write_roots {
            text.push_str(&format!("write {root}\n"));
        }
        for item in &self.guarded {
            text.push_str(&format!("{} {}\n", item.guard.keyword(), item.path));
        }
        text.push_str(&format!("processes {}\n", self.active_processes));
        if text.len() > PROFILE_BYTES_MAX {
            return Err(refused(0, "the profile is larger than 64 KiB"));
        }
        assert!(text.starts_with(HEADER));
        Ok(text)
    }

    /// Reads a profile from text written by [`Self::encode`], treating it as untrusted: the
    /// header first, known keywords only, exactly one container, grants folder and process
    /// limit, no empty or carriage-return lines, then [`Self::validate`]. Input: the text. Output: the
    /// profile, or the refusal with its line.
    pub fn decode(text: &str) -> Result<Self, ProfileError> {
        if text.len() > PROFILE_BYTES_MAX {
            return Err(refused(0, "the profile is larger than 64 KiB"));
        }
        let body = text.strip_suffix('\n').unwrap_or(text);
        let mut lines = body
            .split('\n')
            .enumerate()
            .map(|(at, line)| (at + 1, line));
        if lines.next().map(|(_, line)| line) != Some(HEADER) {
            return Err(refused(
                1,
                "the first line is not the boundary profile header",
            ));
        }
        let mut container = None;
        let mut grants = None;
        let mut active_processes = None;
        let mut profile = Self {
            container: String::new(),
            capabilities: Vec::new(),
            read_roots: Vec::new(),
            write_roots: Vec::new(),
            guarded: Vec::new(),
            active_processes: 0,
            grants: String::new(),
        };
        for (number, line) in lines {
            let (keyword, value) = line
                .split_once(' ')
                .ok_or_else(|| refused(number, "a line has no value"))?;
            if value.is_empty() {
                return Err(refused(number, "a line has no value"));
            }
            match keyword {
                "container" => {
                    if container.replace(value.to_owned()).is_some() {
                        return Err(refused(number, "the container is named twice"));
                    }
                }
                "grants" => {
                    if grants.replace(value.to_owned()).is_some() {
                        return Err(refused(number, "the grants folder is named twice"));
                    }
                }
                "capability" => profile.capabilities.push(
                    Capability::parse(value)
                        .ok_or_else(|| refused(number, "an unknown capability"))?,
                ),
                "read" => profile.read_roots.push(value.to_owned()),
                "write" => profile.write_roots.push(value.to_owned()),
                "readonly" | "hidden" => profile.guarded.push(Guarded {
                    path: value.to_owned(),
                    guard: if keyword == "readonly" {
                        Guard::ReadOnly
                    } else {
                        Guard::Hidden
                    },
                }),
                "processes" => {
                    if value.len() > 4 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                        return Err(refused(number, "the process limit is not a number"));
                    }
                    let parsed = value
                        .parse::<u32>()
                        .map_err(|_| refused(number, "the process limit is not a number"))?;
                    if active_processes.replace(parsed).is_some() {
                        return Err(refused(number, "the process limit is given twice"));
                    }
                }
                _ => return Err(refused(number, "an unknown keyword")),
            }
            if let Some(path) = match keyword {
                "grants" | "read" | "write" | "readonly" | "hidden" => Some(value),
                _ => None,
            } {
                check_path(path).map_err(|reason| refused(number, reason))?;
            }
        }
        profile.container = container.ok_or_else(|| refused(0, "the container is missing"))?;
        profile.grants = grants.ok_or_else(|| refused(0, "the grants folder is missing"))?;
        profile.active_processes =
            active_processes.ok_or_else(|| refused(0, "the process limit is missing"))?;
        profile.validate()?;
        Ok(profile)
    }

    /// Reads and decodes the profile file at `path`, reading at most one byte past the size
    /// limit. Input: the file path. Output: the profile, or the refusal.
    pub fn load(path: &Path) -> Result<Self, ProfileError> {
        let file = File::open(path).map_err(|_| refused(0, "the profile file cannot be opened"))?;
        let mut bytes = Vec::new();
        file.take(PROFILE_BYTES_MAX as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| refused(0, "the profile file cannot be read"))?;
        if bytes.len() > PROFILE_BYTES_MAX {
            return Err(refused(0, "the profile is larger than 64 KiB"));
        }
        let text =
            String::from_utf8(bytes).map_err(|_| refused(0, "the profile is not UTF-8 text"))?;
        Self::decode(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BoundaryProfile {
        BoundaryProfile {
            container: "crazyagent.0123456789abcdef0123456789abcdef".into(),
            capabilities: vec![Capability::InternetClient],
            read_roots: vec![r"C:\Program Files\nodejs".into()],
            write_roots: vec![
                r"C:\Users\mira\Projects\shop".into(),
                r"C:\Users\mira\AppData\Local\CrazyAgent\agents\a1\home".into(),
                r"C:\Users\mira\AppData\Local\CrazyAgent\agents\a1\tmp".into(),
            ],
            guarded: vec![
                Guarded {
                    path: r"C:\Users\mira\Projects\shop\context".into(),
                    guard: Guard::ReadOnly,
                },
                Guarded {
                    path: r"C:\Users\mira\.ssh".into(),
                    guard: Guard::Hidden,
                },
            ],
            active_processes: 512,
            grants: GRANTS.into(),
        }
    }

    const GRANTS: &str = r"C:\Users\mira\AppData\Local\CrazyAgent\engine\boundary-grants";

    fn decode_error(text: &str) -> ProfileError {
        BoundaryProfile::decode(text).unwrap_err()
    }

    #[test]
    fn a_profile_written_to_a_file_reads_back_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.profile");
        let text = sample().encode().unwrap();
        assert!(text.starts_with("crazyagent-win-boundary 2\ncontainer crazyagent."));
        assert!(text.contains(&format!("\ngrants {GRANTS}\n")));
        assert!(text.contains("\nwrite C:\\Users\\mira\\Projects\\shop\n"));
        assert!(text.contains("\nreadonly C:\\Users\\mira\\Projects\\shop\\context\n"));
        assert!(text.contains("\nhidden C:\\Users\\mira\\.ssh\n"));
        assert!(text.ends_with("processes 512\n"));
        std::fs::write(&path, &text).unwrap();
        assert_eq!(BoundaryProfile::load(&path).unwrap(), sample());
        let mut closed = sample();
        closed.capabilities.clear();
        closed.read_roots.clear();
        closed.guarded.clear();
        std::fs::write(&path, closed.encode().unwrap()).unwrap();
        assert_eq!(BoundaryProfile::load(&path).unwrap(), closed);
    }

    #[test]
    fn only_plain_drive_paths_are_accepted() {
        for good in [
            r"C:\a",
            r"d:\Users\mira\Projects\shop",
            r"C:\Program Files (x86)\x",
            r"C:\PROGRA~1\x",
            r"C:\a\ünïcödé",
        ] {
            assert_eq!(check_path(good), Ok(()), "{good}");
        }
        for bad in [
            "",
            r"C:\",
            "C:",
            r"relative\path",
            r"\Users\mira",
            "C:/Users/mira",
            r"C:\Users/mira",
            r"\\server\share\x",
            r"\\?\C:\x",
            r"\\.\pipe\x",
            r"C:\a\..\b",
            r"C:\a\.\b",
            r"C:\a\",
            r"C:\a\\b",
            r#"C:\a"b"#,
            r"C:\a:stream",
            r"C:\a\b.",
            r"C:\a\b ",
            "C:\\a\u{7}b",
            "C:\\a\nb",
            r"C:\a*",
            r"1:\a",
        ] {
            assert!(check_path(bad).is_err(), "{bad:?}");
        }
        assert!(check_path(&format!(r"C:\{}", "a".repeat(PATH_BYTES_MAX))).is_err());
    }

    #[test]
    fn container_names_are_short_plain_words() {
        assert_eq!(check_container("crazyagent.0123abcd"), Ok(()));
        assert_eq!(check_container(&"a".repeat(64)), Ok(()));
        for bad in [
            "",
            ".hidden",
            "-dash",
            "has space",
            "slash/name",
            "back\\slash",
            "ünï",
            &"a".repeat(65),
        ] {
            assert!(check_container(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn decode_refuses_malformed_profiles_and_names_the_line() {
        let good = sample().encode().unwrap();
        assert_eq!(decode_error("not a profile\n").line, 1);
        assert_eq!(
            decode_error(&good.replace("container ", "box ")).reason,
            "an unknown keyword"
        );
        assert_eq!(decode_error(&good.replace("container ", "box ")).line, 2);
        assert_eq!(
            decode_error(&good.replace("processes 512\n", "")).reason,
            "the process limit is missing"
        );
        assert_eq!(
            decode_error(&good.replace(
                "container crazyagent.0123456789abcdef0123456789abcdef\n",
                ""
            ))
            .reason,
            "the container is missing"
        );
        assert_eq!(
            decode_error(&good.replace("processes 512", "processes 512\nprocesses 3")).reason,
            "the process limit is given twice"
        );
        let doubled = good.replacen("container ", "container crazyagent.other\ncontainer ", 1);
        assert_eq!(
            decode_error(&doubled).reason,
            "the container is named twice"
        );
        for limit in ["0", "5000", "-1", "1e3", "+5", "00512", " 5"] {
            let text = good.replace("processes 512", &format!("processes {limit}"));
            assert!(BoundaryProfile::decode(&text).is_err(), "{limit}");
        }
        assert_eq!(
            decode_error(&good.replace("internetClient", "internetClientServer")).reason,
            "an unknown capability"
        );
        assert_eq!(
            decode_error(&good.replace(
                "capability internetClient",
                "capability internetClient\ncapability internetClient"
            ))
            .reason,
            "a capability is listed twice"
        );
        let relative = good.replace(r"write C:\Users\mira\Projects\shop", r"write Projects\shop");
        assert_eq!(decode_error(&relative).line, 6);
        let grants_line = format!("grants {GRANTS}\n");
        assert_eq!(
            decode_error(&good.replace(&grants_line, "")).reason,
            "the grants folder is missing"
        );
        assert_eq!(
            decode_error(&good.replace(&grants_line, &format!("{grants_line}{grants_line}")))
                .reason,
            "the grants folder is named twice"
        );
        assert_eq!(
            decode_error(&good.replace(&grants_line, "grants engine\\boundary-grants\n")).line,
            3
        );
        assert_eq!(
            decode_error(&good.replace("crazyagent-win-boundary 2", "crazyagent-win-boundary 1"))
                .line,
            1,
            "a profile without a grants folder, from before the record, is refused"
        );
        let unc = good.replace(r"hidden C:\Users\mira\.ssh", r"hidden \\server\share");
        assert!(decode_error(&unc).reason.contains("drive path"));
        assert_eq!(decode_error(&good.replace('\n', "\r\n")).line, 1);
        assert_eq!(
            decode_error(&good.replacen("\ncontainer", "\n\ncontainer", 1)).line,
            2
        );
        assert_eq!(
            decode_error(&good.replace(r"read C:\Program Files\nodejs", "read ")).reason,
            "a line has no value"
        );
    }

    #[test]
    fn bounds_hold_on_encode_decode_and_load() {
        let mut many = sample();
        many.read_roots = (0..=READ_ROOTS_MAX).map(|n| format!(r"C:\r{n}")).collect();
        assert_eq!(many.encode().unwrap_err().reason, "too many read roots");
        let mut none = sample();
        none.write_roots.clear();
        assert!(none.encode().is_err());
        let mut guarded = sample();
        guarded.guarded = (0..=GUARDED_MAX)
            .map(|n| Guarded {
                path: format!(r"C:\g{n}"),
                guard: Guard::Hidden,
            })
            .collect();
        assert_eq!(
            guarded.encode().unwrap_err().reason,
            "too many protected paths"
        );
        let mut big = sample();
        big.guarded = (0..GUARDED_MAX)
            .map(|n| Guarded {
                path: format!(r"C:\{}\{n}", "p".repeat(1010)),
                guard: Guard::ReadOnly,
            })
            .collect();
        assert_eq!(
            big.encode().unwrap_err().reason,
            "the profile is larger than 64 KiB"
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.profile");
        std::fs::write(&path, vec![b'x'; PROFILE_BYTES_MAX + 1]).unwrap();
        assert_eq!(
            BoundaryProfile::load(&path).unwrap_err().reason,
            "the profile is larger than 64 KiB"
        );
        std::fs::write(&path, [0xff, 0xfe, b'\n']).unwrap();
        assert_eq!(
            BoundaryProfile::load(&path).unwrap_err().reason,
            "the profile is not UTF-8 text"
        );
        assert_eq!(
            BoundaryProfile::load(&dir.path().join("missing"))
                .unwrap_err()
                .reason,
            "the profile file cannot be opened"
        );
        assert_eq!(
            ProfileError {
                line: 3,
                reason: "an unknown keyword"
            }
            .to_string(),
            "boundary profile refused at line 3: an unknown keyword"
        );
    }
}
