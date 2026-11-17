use super::{compare, normalise, path_char, without_placeholders};
use serde_json::{Value, json};

/// Reads a keychain lock file below `Library/Keychains/`, `.fl` and 8 hex digits that differ
/// on every run, at the start of a string. Input: the rest of the string. Output: the matched
/// length and the folder with `<keychain-lock>` for the file, or none.
pub(super) fn keychain_lock_len(rest: &str) -> Option<(usize, String)> {
    const FOLDER: &str = "Library/Keychains/";
    let name = rest.strip_prefix(FOLDER)?.strip_prefix(".fl")?;
    let digits = name.bytes().take_while(u8::is_ascii_hexdigit).count();
    (digits == 8 && !name[digits..].starts_with(path_char)).then(|| {
        (
            FOLDER.len() + 3 + digits,
            format!("{FOLDER}<keychain-lock>"),
        )
    })
}

/// Reads a socket generation folder, which 2.4.2's engine names `o` and 8 random lowercase
/// hex digits on every start, as the whole name of a folder right below a masked temp entry,
/// at the start of a string. Input: the rest of the string. Output: the matched length and the
/// entry with `o<generation>` for the folder, or none.
pub(super) fn generation_len(rest: &str) -> Option<(usize, String)> {
    let mark = placeholder_len(rest)?;
    let after = rest[mark..].strip_prefix('/')?;
    let name = after.find(|c: char| !path_char(c)).unwrap_or(after.len());
    is_generation(&after[..name])
        .then(|| (mark + 1 + name, format!("{}/o<generation>", &rest[..mark])))
}

/// Says whether a folder name is a socket generation name. Input: the name. Output: true for
/// `o` and exactly 8 lowercase hex digits.
pub(super) fn is_generation(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 9
        && bytes[0] == b'o'
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

/// Says whether text ends in a masked temp placeholder, so a folder after it is a temp entry's
/// child. Input: the text. Output: true when it ends in `<temp>` or `<path-N>`.
pub(super) fn ends_in_temp(text: &str) -> bool {
    text.rfind('<')
        .is_some_and(|at| placeholder_len(&text[at..]) == Some(text.len() - at))
}

/// Sorts each run of consecutive `(subpath "...")` lines that holds a socket generation
/// folder, every line's closing parentheses kept in place: 2.4.2's `engine_secrets_in` adds
/// the runtime's socket places in directory order, which follows the random generation name.
/// Input: a masked array. Output: none; the array is reordered in place.
pub(super) fn sort_socket_places(items: &mut [Value]) {
    let mut start = 0;
    while start < items.len() {
        let len = items[start..]
            .iter()
            .take_while(|item| item.as_str().and_then(subpath_line).is_some())
            .count();
        let run = &mut items[start..start + len];
        let generation = |item: &Value| {
            item.as_str()
                .is_some_and(|line| line.contains("/o<generation>\""))
        };
        if run.iter().any(generation) {
            let (mut rules, closers): (Vec<String>, Vec<String>) = run
                .iter()
                .filter_map(|item| item.as_str().and_then(subpath_line))
                .map(|(rule, closer)| (rule.to_owned(), closer.to_owned()))
                .unzip();
            assert_eq!(rules.len(), run.len());
            rules.sort();
            for ((item, rule), closer) in run.iter_mut().zip(rules).zip(closers) {
                *item = Value::from(rule + &closer);
            }
        }
        start += len.max(1);
    }
}

/// Splits a `(subpath "...")` line into the rule and the parentheses after it that close its
/// block. Input: the line. Output: both parts, or none for any other line.
fn subpath_line(line: &str) -> Option<(&str, &str)> {
    let rule_len = line.trim_end_matches(')').len() + 1;
    if rule_len > line.len() {
        return None;
    }
    let (rule, closer) = line.split_at(rule_len);
    let quoted = rule
        .trim_start()
        .strip_prefix("(subpath \"")?
        .strip_suffix("\")")?;
    (!quoted.contains('"')).then_some((rule, closer))
}

/// Reads a masked temp placeholder at the start of a string. Input: the string. Output: the
/// length of `<temp>` or `<path-N>` there, or none.
fn placeholder_len(rest: &str) -> Option<usize> {
    if rest.starts_with("<temp>") {
        return Some("<temp>".len());
    }
    let after = rest.strip_prefix("<path-")?;
    let digits = after.bytes().take_while(u8::is_ascii_digit).count();
    (digits > 0 && after.as_bytes().get(digits) == Some(&b'>'))
        .then_some("<path-".len() + digits + 1)
}

#[test]
fn a_keychain_lock_file_reads_as_one_mark_and_other_keychain_files_stay() {
    let files = |lock: &str| {
        normalise(&json!({"files": [
            format!("Library/Keychains/.fl{lock}"),
            "Library/Keychains/login.keychain-db",
            "Library/Keychains/.flAB69C6F5x",
            "Library/Keychains/.flAB69C6F",
        ]}))
        .unwrap()
    };
    let first = files("AB69C6F5");
    assert!(
        first.contains("\"Library/Keychains/<keychain-lock>\""),
        "{first}"
    );
    assert!(
        first.contains("\"Library/Keychains/login.keychain-db\""),
        "{first}"
    );
    assert!(
        first.contains("\"Library/Keychains/.flAB69C6F5x\""),
        "{first}"
    );
    assert!(
        first.contains("\"Library/Keychains/.flAB69C6F\""),
        "{first}"
    );
    assert_eq!(compare(first.as_bytes(), &files("1217E02B")), Ok(()));
    assert_eq!(without_placeholders("<keychain-lock>"), " ");
}

#[test]
fn a_socket_generation_folder_below_a_temp_entry_reads_as_one_mark() {
    let real = std::env::temp_dir().canonicalize().unwrap();
    let real = real.to_string_lossy();
    let entry = ".tmpAb12st";
    let rules = |generation: &str| {
        normalise(&json!({
            "a_plain": format!("(subpath \"{real}/{entry}/{generation}\")"),
            "b_caseless": format!(
                "(regex #\"^{}/{}/{}(/.*)?$\")",
                super::caseless::spell(&real),
                super::caseless::spell(entry),
                super::caseless::spell(generation)
            ),
            "c_kept": [
                format!("{real}/{entry}/o0b1988cbx"),
                format!("{real}/{entry}/run/o0b1988cb"),
                format!("{real}/{entry}/o0B1988CB"),
            ],
        }))
        .unwrap()
    };
    let first = rules("o0b1988cb");
    assert!(
        first.contains("(subpath \\\"<path-1>/o<generation>\\\")"),
        "{first}"
    );
    assert!(first.contains("^<path-1>/o<generation>(/.*)?$"), "{first}");
    for kept in ["/o0b1988cbx\"", "/run/o0b1988cb\"", "/o0B1988CB\""] {
        assert!(first.contains(&format!("<path-1>{kept}")), "{first}");
    }
    assert_eq!(compare(first.as_bytes(), &rules("oce310940")), Ok(()));
    assert!(!is_generation("o0b1988c") && !is_generation("p0b1988cb"));
    assert!(ends_in_temp("x <path-12>") && !ends_in_temp("<path-1>/x"));
    assert_eq!(without_placeholders("o<generation>"), "o ");
}

#[test]
fn socket_places_in_either_directory_order_read_the_same_and_other_runs_keep_their_order() {
    let real = std::env::temp_dir().canonicalize().unwrap();
    let real = real.to_string_lossy();
    let entry = ".tmpAb12st";
    let lines = |first: &str, second: &str| {
        normalise(&json!({"lines": [
            "(deny file-read* file-write*",
            "  (subpath \"/fixed/terminal\")",
            format!("  (subpath \"{real}/{entry}/{first}\")"),
            format!("  (subpath \"{real}/{entry}/{second}\"))"),
            "(allow file-read*",
            "  (subpath \"/b\")",
            "  (subpath \"/a\"))",
        ]}))
        .unwrap()
    };
    let first = lines("o0b1988cb", "engine.sock");
    assert_eq!(
        compare(first.as_bytes(), &lines("engine.sock", "oce310940")),
        Ok(())
    );
    let at = |line: &str| {
        first
            .find(line)
            .unwrap_or_else(|| panic!("{line}: {first}"))
    };
    assert!(at("(subpath \\\"/fixed/terminal\\\")\"") < at("<path-1>/engine.sock\\\")\""));
    assert!(at("<path-1>/engine.sock\\\")\"") < at("<path-1>/o<generation>\\\"))\""));
    assert!(at("(subpath \\\"/b\\\")\"") < at("(subpath \\\"/a\\\"))\""));
    assert_eq!(
        subpath_line("  (subpath \"x\")))"),
        Some(("  (subpath \"x\")", "))"))
    );
    assert_eq!(subpath_line("  (literal \"x\")"), None);
    assert_eq!(subpath_line("(deny file-read*"), None);
}
