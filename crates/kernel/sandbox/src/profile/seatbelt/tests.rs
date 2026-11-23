#[cfg(target_os = "macos")]
use super::priming_profile;
use super::{regex_bodies, root_pin, sbpl_overlong};
use crate::profile::consts::SBPL_STRING_MAX;
use std::path::Path;

/// Alternatives too many for one regex string pack into several bodies, each within the
/// Seatbelt string bound and each alternative in exactly one, and a profile of those bodies
/// and a root pin holds no string its reader refuses.
#[test]
fn packed_regex_bodies_and_a_root_pin_stay_within_the_string_bound() {
    let alternatives: Vec<String> = (0..200).map(|at| format!("name{at:04}")).collect();
    let bodies = regex_bodies("^/private/tmp/agent/", &alternatives, "$");
    assert!(bodies.len() >= 2, "{bodies:?}");
    assert!(bodies.iter().all(|body| body.len() <= SBPL_STRING_MAX));
    for alternative in &alternatives {
        let holding = bodies
            .iter()
            .filter(|body| {
                body.contains(&format!("({alternative}|"))
                    || body.contains(&format!("|{alternative}|"))
                    || body.contains(&format!("|{alternative})"))
                    || body.contains(&format!("({alternative})"))
            })
            .count();
        assert_eq!(holding, 1, "{alternative}");
    }
    let mut profile: String = bodies
        .iter()
        .map(|body| format!("(deny file-write*\n  (regex #\"{body}\"))\n"))
        .collect();
    profile.push_str(&root_pin(Path::new("/private/tmp/agent")));
    assert!(sbpl_overlong(&profile).is_empty());
    assert!(profile.contains("(literal \"/private/tmp/agent\")"));
}

/// The uv priming profile denies by default, writes only in the priming folder, reads the uv
/// binary and holds no string its reader refuses.
#[cfg(target_os = "macos")]
#[test]
fn the_priming_profile_writes_only_in_its_folder_and_reads_uv() {
    let text = priming_profile(Path::new("/private/tmp/prime"), Path::new("/opt/uv/bin/uv"));
    assert!(text.starts_with("(version 1)\n(deny default)\n"));
    assert!(text.contains("(allow file-read* file-write*\n  (subpath \"/private/tmp/prime\"))"));
    assert!(text.contains("(literal \"/opt/uv/bin/uv\")"));
    assert_eq!(text.matches("file-write*").count(), 1);
    assert!(sbpl_overlong(&text).is_empty());
}
