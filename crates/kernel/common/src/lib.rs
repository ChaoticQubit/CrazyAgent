#![forbid(unsafe_code)]

use sha2::{Digest, Sha256};

/// The variable names that never reach an agent process or a pane, and the check that tells
/// them.
pub mod env_names;
pub mod names;
pub mod net;
/// The relay token and local placeholder names a model key value is told by, and the
/// checks that tell them.
pub mod relay_names;

#[cfg(test)]
mod names_tests;
#[cfg(test)]
mod perf_kit;
#[cfg(test)]
mod perf_kit_tests;
#[cfg(test)]
mod net_tests;
#[cfg(test)]
mod relay_names_tests;

/// The three desktop platforms the data root and services are defined for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Linux,
    Windows,
}

impl Platform {
    pub const CURRENT: Self = if cfg!(target_os = "macos") {
        Self::MacOs
    } else if cfg!(windows) {
        Self::Windows
    } else {
        Self::Linux
    };
}

/// Hashes bytes with SHA-256, the digest the engine keys socket names, profile references
/// and stored records by. Input: the bytes. Output: the digest as 64 lowercase hex digits.
pub fn hash(bytes: &[u8]) -> String {
    let digest = format!("{:x}", Sha256::digest(bytes));
    assert_eq!(digest.len(), 64);
    digest
}

/// The device names Windows opens alone, in upper case.
const WINDOWS_DEVICE_NAMES: [&str; 6] = ["AUX", "CON", "CONIN$", "CONOUT$", "NUL", "PRN"];

/// The device names Windows opens with one digit after them, in upper case.
const WINDOWS_NUMBERED_DEVICES: [&str; 2] = ["COM", "LPT"];

/// The superscript digits one, two and three, which Windows takes after COM and LPT.
const SUPERSCRIPT_DIGITS: [char; 3] = ['\u{b9}', '\u{b2}', '\u{b3}'];

/// True when Windows opens the name as a device instead of a file: CON, PRN, AUX, NUL,
/// CONIN$, CONOUT$, or COM or LPT followed by one ASCII digit or a superscript one, two or
/// three. Case is compared on the Unicode upper case, so a dotless i counts as an I. The
/// name may stand alone, before an extension or before one trailing colon, with or without
/// spaces before the dot. Input: one path component. Output: whether it is reserved on
/// Windows.
pub fn windows_device(name: &str) -> bool {
    let name = name.strip_suffix(':').unwrap_or(name);
    let stem = name
        .split_once('.')
        .map_or(name, |(stem, _)| stem)
        .trim_end_matches(' ')
        .to_uppercase();
    let numbered = WINDOWS_NUMBERED_DEVICES.iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|tail| {
            let mut tail = tail.chars();
            matches!(
                (tail.next(), tail.next()),
                (Some(digit), None)
                    if digit.is_ascii_digit() || SUPERSCRIPT_DIGITS.contains(&digit)
            )
        })
    });
    numbered || WINDOWS_DEVICE_NAMES.contains(&stem.as_str())
}

#[cfg(test)]
mod tests {
    #[test]
    fn platform_current_matches_this_build() {
        #[cfg(target_os = "macos")]
        let expected = super::Platform::MacOs;
        #[cfg(target_os = "windows")]
        let expected = super::Platform::Windows;
        #[cfg(target_os = "linux")]
        let expected = super::Platform::Linux;
        assert_eq!(super::Platform::CURRENT, expected);
    }

    #[test]
    fn hash_is_the_lowercase_hex_sha256() {
        assert_eq!(
            super::hash(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            super::hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn conin_and_conout_are_devices() {
        for name in [
            "CONIN$",
            "conin$",
            "ConOut$",
            "conout$.txt",
            "CONIN$ .log",
            "conout$:",
        ] {
            assert!(super::windows_device(name), "{name:?}");
        }
        for name in ["conin", "conout", "conin$$", "xconin$"] {
            assert!(!super::windows_device(name), "{name:?}");
        }
    }

    #[test]
    fn com_and_lpt_with_a_superscript_digit_are_devices() {
        for prefix in ["COM", "com", "LPT", "lpt"] {
            for digit in ['\u{b9}', '\u{b2}', '\u{b3}'] {
                for name in [
                    format!("{prefix}{digit}"),
                    format!("{prefix}{digit}.txt"),
                    format!("{prefix}{digit} .txt"),
                ] {
                    assert!(super::windows_device(&name), "{name:?}");
                }
            }
            for other in ['\u{b4}', '\u{2074}', '\u{2079}', '\u{2070}'] {
                let name = format!("{prefix}{other}");
                assert!(!super::windows_device(&name), "{name:?}");
            }
        }
    }

    #[test]
    fn com0_and_lpt0_are_devices() {
        for name in ["COM0", "com0.log", "LPT0", "lpt0 .txt", "com0:"] {
            assert!(super::windows_device(name), "{name:?}");
        }
        for name in ["com00", "lpt01", "com", "lpt"] {
            assert!(!super::windows_device(name), "{name:?}");
        }
    }

    #[test]
    fn a_dotless_i_spelling_of_conin_is_a_device() {
        for name in [
            "con\u{131}n$",
            "CON\u{131}N$",
            "CON\u{131}N$.txt",
            "con\u{131}n$:",
        ] {
            assert!(super::windows_device(name), "{name:?}");
        }
        for name in ["con\u{130}n$", "con\u{131}n"] {
            assert!(!super::windows_device(name), "{name:?}");
        }
    }

    #[test]
    fn every_ascii_answer_of_the_twenty_two_name_rule_is_kept() {
        let mut devices = vec!["con", "prn", "aux", "nul"];
        let numbered: Vec<String> = ["com", "lpt"]
            .iter()
            .flat_map(|prefix| (1..=9).map(move |digit| format!("{prefix}{digit}")))
            .collect();
        devices.extend(numbered.iter().map(String::as_str));
        assert_eq!(devices.len(), 22);
        for device in devices {
            for name in [
                device.to_owned(),
                device.to_ascii_uppercase(),
                format!("{device}.txt"),
                format!("{device} .txt"),
                format!("{device}:"),
            ] {
                assert!(super::windows_device(&name), "{name:?}");
            }
        }
        for name in [
            "com10",
            "lpt10",
            "console",
            "connie",
            "nuly",
            "a.con",
            "v1.2",
            "owner:abc",
            "console:",
            "com10:",
            "nul-free",
            "a b",
        ] {
            assert!(!super::windows_device(name), "{name:?}");
        }
    }

    #[test]
    fn the_sandbox_name_rule_device_lists_keep_their_answers() {
        for name in [
            "CON",
            "con.txt",
            "Prn",
            "AUX.json",
            "nul",
            "COM1",
            "com0.log",
            "LPT9",
            "COM\u{b9}",
            "lpt\u{b2}.txt",
            "CONIN$",
            "conout$",
            "CON .txt",
        ] {
            assert!(super::windows_device(name), "{name:?}");
        }
        for name in [
            "a b",
            "skill-\u{fc}.md",
            "COM10",
            "console",
            "nul-free",
            "a.b",
        ] {
            assert!(!super::windows_device(name), "{name:?}");
        }
    }
}
