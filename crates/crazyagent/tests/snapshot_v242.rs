#![cfg(target_os = "macos")]

use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The golden folders below tests/snapshots/v242, one per surface kind of the 2.5 plan's
/// section 2.0, plus `runner` for the runner's own example. A scenario names one of these.
const KINDS: [&str; 10] = [
    "runner",
    "harness",
    "embedding",
    "provider",
    "check",
    "role",
    "workflow",
    "landing",
    "packs",
    "storage",
];

/// The longest scenario name a golden file stem may have.
const NAME_MAX: usize = 64;

/// One 2.4.2 surface capture, stored at tests/snapshots/v242/<kind>/<name>.json. `capture`
/// gets a fresh scratch folder under the system temp folder and returns the surface as JSON
/// before normalisation. A driver that needs the engine starts the built binary through
/// e2e_support's `Engine` with a fixture HOME and seeds it only through owner requests, never
/// rows written into engine.db; it reads only public crate items, never src cfg(test) code.
#[derive(Clone, Copy)]
struct Scenario {
    kind: &'static str,
    name: &'static str,
    capture: fn(&Path) -> Result<Value, String>,
}

/// Lists every scenario: the runner's own example, then each kind's drivers.
/// Output: the scenarios in the order the snapshot row runs them.
fn scenarios() -> Vec<Scenario> {
    vec![Scenario {
        kind: "runner",
        name: "masking-example",
        capture: masking_example,
    }]
}

/// Captures a fixed surface that holds one of every value the normalisation list masks: an
/// RFC 3339 time, `*_at` and `*_ms` integers, one id twice and another once, a pid, a port, a
/// CRLF line and sandbox rules under the scratch folder. Input: the scratch folder.
/// Output: the surface as JSON; its golden shows what each value becomes.
fn masking_example(scratch: &Path) -> Result<Value, String> {
    let agent = "3f2a9c1e5b7d4a608e1f2c3b4a5d6e7f";
    let rule = |path: &Path| {
        path.to_str()
            .map(str::to_owned)
            .ok_or_else(|| "scratch path is not UTF-8".to_owned())
    };
    Ok(json!({
        "zebra": true,
        "apple": false,
        "agent": agent,
        "lead": agent,
        "session": "123e4567-e89b-12d3-a456-426614174000",
        "started_at": 1_763_322_000_000_u64,
        "elapsed_ms": 1520,
        "pid": 4242,
        "relay_port": 50123,
        "log": format!("2026-11-16T20:31:07.123Z agent {agent} started on 127.0.0.1:50123\r\n"),
        "sandbox": {
            "write": [rule(&scratch.join("projects").join("app"))?],
            "read": [rule(scratch)?],
        },
    }))
}

/// Names the folder every golden lives below.
/// Output: tests/snapshots/v242 below this crate.
fn snapshots_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/v242")
}

/// Names where a scenario's golden lives. Input: the scenario.
/// Output: tests/snapshots/v242/<kind>/<name>.json below this crate.
fn golden_path(scenario: &Scenario) -> PathBuf {
    snapshots_root()
        .join(scenario.kind)
        .join(format!("{}.json", scenario.name))
}

/// Says whether a scenario name is a safe golden file stem.
/// Input: the name. Output: true for 1 to `NAME_MAX` of a-z, 0-9 and -, not starting with -.
fn safe_name(name: &str) -> bool {
    let safe = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-';
    !name.is_empty() && name.len() <= NAME_MAX && !name.starts_with('-') && name.bytes().all(safe)
}

/// Checks every scenario names a known kind and a safe name, once.
/// Input: the scenarios. Output: the first scenario that breaks a rule, as text.
fn check_names(scenarios: &[Scenario]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for scenario in scenarios {
        let at = format!("{}/{}", scenario.kind, scenario.name);
        if !KINDS.contains(&scenario.kind) {
            return Err(format!("{at}: unknown kind"));
        }
        if !safe_name(scenario.name) {
            return Err(format!("{at}: unsafe name"));
        }
        if !seen.insert(at.clone()) {
            return Err(format!("{at}: named twice"));
        }
    }
    Ok(())
}

/// Words a token starts with; a golden holding one at a word start is refused.
const TOKEN_PREFIXES: [&str; 11] = [
    "sk-ant-",
    "sk-proj-",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "AKIA",
];

/// Loopback and any-address host prefixes whose port a string capture masks to `<port>`.
const PORT_HOSTS: [&str; 4] = ["127.0.0.1:", "localhost:", "0.0.0.0:", "[::1]:"];

/// The shortest lowercase hex run masked as an id: engine ids are 24, 32 or 64 characters.
const HEX_ID_MIN: usize = 16;

/// The numbering state of one capture's normalisation: the system temp folder in its raw and
/// resolved spellings, longest first, and the ids and temp entries seen so far, so equal values
/// get one placeholder number in order of first appearance.
#[derive(Clone)]
struct Masks {
    roots: Vec<String>,
    ids: Vec<String>,
    paths: Vec<String>,
}

impl Masks {
    /// Starts a capture's numbering with the system temp folder's spellings.
    /// Output: masks with no id or path numbered yet.
    fn new() -> Self {
        let raw = std::env::temp_dir();
        let mut roots = vec![raw.to_string_lossy().trim_end_matches('/').to_owned()];
        if let Ok(real) = raw.canonicalize() {
            roots.push(real.to_string_lossy().trim_end_matches('/').to_owned());
        }
        roots.sort_by_key(|root| std::cmp::Reverse(root.len()));
        roots.dedup();
        assert!(
            roots
                .iter()
                .all(|root| root.len() > 1 && root.starts_with('/'))
        );
        Masks {
            roots,
            ids: Vec::new(),
            paths: Vec::new(),
        }
    }

    /// Masks one JSON value, rebuilding maps in sorted key order. Inputs: the key the value
    /// sits under, if any (arrays pass their own key down) and the value. Output: the masked
    /// value, or why a map key cannot be kept (it holds a value the list masks).
    fn value(&mut self, key: Option<&str>, value: &Value) -> Result<Value, String> {
        match value {
            Value::Null | Value::Bool(_) => Ok(value.clone()),
            Value::Number(number) => Ok(match key.and_then(field_mask) {
                Some(mask) if number.is_i64() || number.is_u64() => Value::from(mask),
                _ => value.clone(),
            }),
            Value::String(text) => Ok(Value::from(self.text(text))),
            Value::Array(items) => items
                .iter()
                .map(|item| self.value(key, item))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                let mut sorted = serde_json::Map::new();
                for name in keys {
                    if self.clone().text(name) != *name {
                        return Err(format!(
                            "map key {name:?} holds a value the normalisation masks: capture it as a list"
                        ));
                    }
                    sorted.insert(name.clone(), self.value(Some(name.as_str()), &map[name])?);
                }
                Ok(Value::Object(sorted))
            }
        }
    }

    /// Masks one string: line endings to LF, then temp paths, RFC 3339 times, ids and ports.
    /// Input: the raw string. Output: the masked string.
    fn text(&mut self, raw: &str) -> String {
        let lf = raw.replace("\r\n", "\n").replace('\r', "\n");
        let pathed = self.temp_paths(&lf);
        let timed = replace_matches(&pathed, |rest| {
            rfc3339_len(rest.as_bytes()).map(|len| (len, "<t>".to_owned()))
        });
        let ids = &mut self.ids;
        let named = replace_matches(&timed, |rest| {
            let bytes = rest.as_bytes();
            let len = uuid_len(bytes).or_else(|| hex_run_len(bytes))?;
            if bytes.get(len).is_some_and(u8::is_ascii_alphanumeric) {
                return None;
            }
            let number = number(ids, &rest[..len].to_ascii_lowercase());
            Some((len, format!("<id-{number}>")))
        });
        replace_matches(&named, port_len)
    }

    /// Masks every temp path in a string to `<path-N>` plus its tail below the temp entry, so a
    /// rule on a parent folder still reads differently. Input: the string. Output: the string
    /// with each temp root and its first entry replaced.
    fn temp_paths(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut index = 0;
        'scan: while index < text.len() {
            let rest = &text[index..];
            let boundary = out
                .chars()
                .next_back()
                .is_none_or(|before| !path_char(before));
            for root in &self.roots {
                let Some(after) = rest.strip_prefix(root.as_str()) else {
                    continue;
                };
                if !boundary || after.chars().next().is_some_and(path_char) {
                    continue;
                }
                let tail = after.strip_prefix('/').unwrap_or("");
                let entry_len = tail.find(|c: char| !path_char(c)).unwrap_or(tail.len());
                let used = if entry_len > 0 {
                    root.len() + 1 + entry_len
                } else {
                    root.len()
                };
                let number = number(&mut self.paths, &tail[..entry_len]);
                out.push_str(&format!("<path-{number}>"));
                index += used;
                continue 'scan;
            }
            let next = rest
                .chars()
                .next()
                .expect("index is below the length on a char boundary");
            out.push(next);
            index += next.len_utf8();
        }
        out
    }
}

/// Names the placeholder an integer field gets from its key: `*_at` and `*_ms` are times,
/// `pid` and `*_pid` pids, `port` and `*_port` ports. Input: the key. Output: the placeholder,
/// or none when the field is kept.
fn field_mask(key: &str) -> Option<&'static str> {
    if key.ends_with("_at") || key.ends_with("_ms") {
        Some("<t>")
    } else if key == "pid" || key.ends_with("_pid") {
        Some("<pid>")
    } else if key == "port" || key.ends_with("_port") {
        Some("<port>")
    } else {
        None
    }
}

/// Says whether a character can be part of a path segment's name.
/// Input: the character. Output: true for ASCII letters, digits and `._-+@~`.
fn path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "._-+@~".contains(c)
}

/// Numbers a value by first appearance in a table. Inputs: the table and the value.
/// Output: the value's 1-based number, the same each time it is seen again.
fn number(table: &mut Vec<String>, value: &str) -> usize {
    if let Some(index) = table.iter().position(|seen| seen == value) {
        return index + 1;
    }
    table.push(value.to_owned());
    table.len()
}

/// Replaces every match that starts at a word boundary. Inputs: the string and a matcher that
/// reads the rest of the string and gives the matched byte length and its placeholder.
/// Output: the string with each match replaced.
fn replace_matches(text: &str, mut matcher: impl FnMut(&str) -> Option<(usize, String)>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let rest = &text[index..];
        let boundary = text[..index]
            .chars()
            .next_back()
            .is_none_or(|before| !before.is_ascii_alphanumeric());
        if boundary && let Some((len, mask)) = matcher(rest) {
            assert!(len > 0 && rest.is_char_boundary(len));
            out.push_str(&mask);
            index += len;
            continue;
        }
        let next = rest
            .chars()
            .next()
            .expect("index is below the length on a char boundary");
        out.push(next);
        index += next.len_utf8();
    }
    out
}

/// Reads an RFC 3339 date-time at the start of some bytes: `YYYY-MM-DDTHH:MM:SS`, optional
/// fraction, then `Z` or `+HH:MM`/`-HH:MM` (T, t or a space between date and time).
/// Input: the bytes. Output: its length, or none.
fn rfc3339_len(bytes: &[u8]) -> Option<usize> {
    let digits = |from: usize, count: usize| {
        bytes
            .get(from..from + count)
            .is_some_and(|run| run.iter().all(u8::is_ascii_digit))
    };
    let at = |index: usize, set: &[u8]| bytes.get(index).is_some_and(|byte| set.contains(byte));
    let date = digits(0, 4) && at(4, b"-") && digits(5, 2) && at(7, b"-") && digits(8, 2);
    let time = at(10, b"Tt ")
        && digits(11, 2)
        && at(13, b":")
        && digits(14, 2)
        && at(16, b":")
        && digits(17, 2);
    if !(date && time) {
        return None;
    }
    let mut end = 19;
    if at(end, b".") && digits(end + 1, 1) {
        end += 1;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
    }
    if at(end, b"Zz") {
        return Some(end + 1);
    }
    let offset = at(end, b"+-") && digits(end + 1, 2) && at(end + 3, b":") && digits(end + 4, 2);
    offset.then_some(end + 6)
}

/// Reads a UUID (8-4-4-4-12 hex digits, either case) at the start of some bytes.
/// Input: the bytes. Output: 36, or none.
fn uuid_len(bytes: &[u8]) -> Option<usize> {
    let groups = [8, 4, 4, 4, 12];
    let mut index = 0;
    for (group, len) in groups.iter().enumerate() {
        if group > 0 {
            if bytes.get(index) != Some(&b'-') {
                return None;
            }
            index += 1;
        }
        if !bytes
            .get(index..index + len)
            .is_some_and(|run| run.iter().all(u8::is_ascii_hexdigit))
        {
            return None;
        }
        index += len;
    }
    assert_eq!(index, 36);
    Some(index)
}

/// Reads a run of at least `HEX_ID_MIN` lowercase hex digits at the start of some bytes.
/// Input: the bytes. Output: the run's length, or none when it is shorter.
fn hex_run_len(bytes: &[u8]) -> Option<usize> {
    let len = bytes
        .iter()
        .take_while(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(*byte))
        .count();
    (len >= HEX_ID_MIN).then_some(len)
}

/// Reads a loopback or any-address `host:port` at the start of a string and keeps the host.
/// Input: the rest of the string. Output: the matched length and `host:<port>`, or none.
fn port_len(rest: &str) -> Option<(usize, String)> {
    let host = PORT_HOSTS.iter().find(|host| rest.starts_with(**host))?;
    let digits = rest[host.len()..]
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    (1..=5)
        .contains(&digits)
        .then(|| (host.len() + digits, format!("{host}<port>")))
}

/// Masks a capture per the normalisation list and prints it as golden text.
/// Input: the raw capture. Output: pretty JSON with sorted keys, LF and a trailing newline,
/// or why the capture cannot be normalised.
fn normalise(capture: &Value) -> Result<String, String> {
    let masked = Masks::new().value(None, capture)?;
    let text = serde_json::to_string_pretty(&masked).map_err(|error| error.to_string())?;
    assert!(!text.contains('\r'));
    Ok(text + "\n")
}

/// Compares golden bytes with a normalised capture, byte for byte.
/// Inputs: the golden file's bytes and the capture text. Output: nothing when equal, else the
/// first line that differs, from both sides.
fn compare(golden: &[u8], text: &str) -> Result<(), String> {
    if golden == text.as_bytes() {
        return Ok(());
    }
    let want: Vec<&[u8]> = golden.split(|byte| *byte == b'\n').collect();
    let got: Vec<&[u8]> = text.as_bytes().split(|byte| *byte == b'\n').collect();
    let line = (0..want.len().max(got.len()))
        .find(|index| want.get(*index) != got.get(*index))
        .expect("two unequal byte strings differ on one of their lines");
    let show = |side: Option<&&[u8]>| {
        side.map_or_else(
            || "<none>".to_owned(),
            |bytes| format!("{:?}", String::from_utf8_lossy(bytes)),
        )
    };
    Err(format!(
        "line {}: golden {}, capture {}",
        line + 1,
        show(want.get(line)),
        show(got.get(line))
    ))
}

/// Says whether `word` occurs in `text` as a whole word, ignoring ASCII case.
/// Inputs: the text and the word. Output: true when an occurrence has no letter, digit or `_`
/// on either side.
fn has_word(text: &str, word: &str) -> bool {
    let text = text.to_ascii_lowercase();
    let word = word.to_ascii_lowercase();
    let inner =
        |byte: Option<&u8>| byte.is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_');
    text.match_indices(&word).any(|(start, _)| {
        let bytes = text.as_bytes();
        !inner(start.checked_sub(1).and_then(|before| bytes.get(before)))
            && !inner(bytes.get(start + word.len()))
    })
}

/// Refuses golden text that holds a user path, the home folder, the user name or a token.
/// Inputs: the text, the home folder and the user name when known. Output: nothing when the
/// text is clean, else which kind of private value it holds (never the value itself).
fn refuse_private(text: &str, home: Option<&str>, user: Option<&str>) -> Result<(), String> {
    if text.contains("/Users/") || text.contains("/home/") {
        return Err("holds a user path".to_owned());
    }
    if home.is_some_and(|home| home.len() > 1 && text.contains(home)) {
        return Err("holds the home folder".to_owned());
    }
    if user.is_some_and(|user| user.len() >= 3 && has_word(text, user)) {
        return Err("holds the user name".to_owned());
    }
    let bytes = text.as_bytes();
    for prefix in TOKEN_PREFIXES {
        let starts_word = |start: usize| {
            start == 0 || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_')
        };
        if text
            .match_indices(prefix)
            .any(|(start, _)| starts_word(start))
        {
            return Err(format!("holds a {prefix} token"));
        }
    }
    Ok(())
}

/// Writes a golden only when no file is there yet, after refusing private text with this
/// machine's HOME and USER. Inputs: the golden path and the text. Output: true when written,
/// false when a golden already exists, or why it could not be written.
fn write_missing(path: &Path, text: &str) -> Result<bool, String> {
    let home = std::env::var("HOME").ok();
    let user = std::env::var("USER").ok();
    refuse_private(text, home.as_deref(), user.as_deref())?;
    let folder = path
        .parent()
        .ok_or_else(|| "a golden path has a folder".to_owned())?;
    std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            file.write_all(text.as_bytes())
                .map_err(|error| error.to_string())?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

/// Runs one scenario in a fresh scratch folder and normalises what it captures.
/// Input: the scenario. Output: the golden text, or why the capture failed.
fn capture_text(scenario: &Scenario) -> Result<String, String> {
    let scratch = tempfile::tempdir().map_err(|error| error.to_string())?;
    let raw = (scenario.capture)(scratch.path())?;
    normalise(&raw)
}

/// Lists golden files no scenario names, so a dropped driver cannot drop its capture silently.
/// Input: the scenarios. Output: one line per golden with no scenario, sorted.
fn orphans(scenarios: &[Scenario]) -> Vec<String> {
    let known: BTreeSet<PathBuf> = scenarios.iter().map(golden_path).collect();
    let mut found = Vec::new();
    for kind in KINDS {
        let Ok(entries) = std::fs::read_dir(snapshots_root().join(kind)) else {
            continue;
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
                && !known.contains(&path)
            {
                found.push(format!("{}: a golden with no scenario", path.display()));
            }
        }
    }
    found.sort();
    found
}

/// Checks one scenario against its golden, or captures it when `write` is set and none exists.
/// Inputs: the scenario and the write switch. Output: nothing when it matches, else a failure
/// line (a capture written for review also fails, so a writing run is never green).
fn check(scenario: &Scenario, write: bool) -> Result<(), String> {
    let at = format!("{}/{}", scenario.kind, scenario.name);
    let path = golden_path(scenario);
    let text = capture_text(scenario).map_err(|error| format!("{at}: {error}"))?;
    match std::fs::read(&path) {
        Ok(golden) => compare(&golden, &text).map_err(|diff| format!("{at}: {diff}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && write => {
            let again = capture_text(scenario).map_err(|error| format!("{at}: {error}"))?;
            if again != text {
                return Err(format!(
                    "{at}: two captures differ, {}",
                    compare(again.as_bytes(), &text).unwrap_err()
                ));
            }
            write_missing(&path, &text).map_err(|error| format!("{at}: {error}"))?;
            Err(format!(
                "{at}: captured; review the golden, then run without SNAPSHOT_V242_WRITE"
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "{at}: no golden yet; capture it with SNAPSHOT_V242_WRITE=1 on the 2.4.2 release commit"
        )),
        Err(error) => Err(format!("{at}: {error}")),
    }
}

#[test]
fn every_scenario_matches_its_2_4_2_capture() {
    let all = scenarios();
    assert_eq!(check_names(&all), Ok(()));
    let write = std::env::var_os("SNAPSHOT_V242_WRITE").is_some();
    let mut failures: Vec<String> = all
        .iter()
        .filter_map(|scenario| check(scenario, write).err())
        .collect();
    failures.extend(orphans(&all));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_scenario_names_a_known_kind_and_a_safe_name_once() {
    let all = scenarios();
    assert!(!all.is_empty());
    assert_eq!(check_names(&all), Ok(()));
    assert!(golden_path(&all[0]).ends_with("tests/snapshots/v242/runner/masking-example.json"));
    assert_eq!(
        check_names(&[all[0], all[0]]),
        Err("runner/masking-example: named twice".to_owned())
    );
    let unknown = Scenario {
        kind: "kinds",
        ..all[0]
    };
    assert_eq!(
        check_names(&[unknown]),
        Err("kinds/masking-example: unknown kind".to_owned())
    );
    let long = "a".repeat(NAME_MAX + 1);
    for name in ["", "Upper", "a/b", "a.json", "-lead", "a b", long.as_str()] {
        assert!(!safe_name(name), "{name:?}");
    }
    assert!(safe_name(&"a".repeat(NAME_MAX)));
    assert!(safe_name("claude-code-worker-2"));
}

#[test]
fn the_comparer_fails_on_one_changed_byte_after_normalisation() {
    let text = normalise(&json!({"rule": "allow", "paths": ["projects/app"]})).unwrap();
    assert_eq!(compare(text.as_bytes(), &text), Ok(()));
    let changed = text.replacen("allow", "allox", 1);
    assert_eq!(changed.len(), text.len());
    assert_eq!(
        compare(changed.as_bytes(), &text),
        Err("line 5: golden \"  \\\"rule\\\": \\\"allox\\\"\", capture \"  \\\"rule\\\": \\\"allow\\\"\"".to_owned())
    );
    let unterminated = &text.as_bytes()[..text.len() - 1];
    assert!(compare(unterminated, &text).is_err());
    let crlf = text.replace('\n', "\r\n");
    assert!(compare(crlf.as_bytes(), &text).is_err());
}

#[test]
fn masking_keeps_two_equal_ids_equal_and_two_different_ids_apart() {
    let one = "0123456789abcdef0123456789abcdef";
    let two = "fedcba9876543210fedcba9876543210";
    let capture = json!({
        "third": one,
        "uuid_lower": "123e4567-e89b-12d3-a456-426614174000",
        "second": two,
        "first": one,
        "short": "abcdef012345678",
        "uuid": "123E4567-E89B-12D3-A456-426614174000",
        "in_text": format!("job {two} for {one}"),
    });
    let text = normalise(&capture).unwrap();
    assert_eq!(
        text,
        "{\n  \"first\": \"<id-1>\",\n  \"in_text\": \"job <id-2> for <id-1>\",\n  \"second\": \"<id-2>\",\n  \"short\": \"abcdef012345678\",\n  \"third\": \"<id-1>\",\n  \"uuid\": \"<id-3>\",\n  \"uuid_lower\": \"<id-3>\"\n}\n"
    );
    let reordered = json!({
        "in_text": format!("job {two} for {one}"),
        "first": one,
        "uuid": "123E4567-E89B-12D3-A456-426614174000",
        "short": "abcdef012345678",
        "second": two,
        "uuid_lower": "123e4567-e89b-12d3-a456-426614174000",
        "third": one,
    });
    assert_eq!(normalise(&reordered).unwrap(), text);
    assert!(normalise(&json!({ one: 1 })).is_err());
}

#[test]
fn a_sandbox_rule_widened_to_its_parent_fails_after_normalisation() {
    let scratch = tempfile::tempdir().unwrap();
    let rule = |path: &Path| json!({"sandbox": {"write": [path.to_str().unwrap()]}});
    let narrow = scratch.path().join("projects").join("app");
    let golden = normalise(&rule(&narrow)).unwrap();
    assert!(golden.contains("\"<path-1>/projects/app\""), "{golden}");
    assert_eq!(
        compare(golden.as_bytes(), &normalise(&rule(&narrow)).unwrap()),
        Ok(())
    );
    let real = scratch
        .path()
        .canonicalize()
        .unwrap()
        .join("projects")
        .join("app");
    assert_eq!(
        compare(golden.as_bytes(), &normalise(&rule(&real)).unwrap()),
        Ok(())
    );
    for wider in [
        scratch.path().join("projects"),
        scratch.path().to_path_buf(),
        std::env::temp_dir(),
    ] {
        let text = normalise(&rule(&wider)).unwrap();
        assert!(compare(golden.as_bytes(), &text).is_err(), "{text}");
    }
}

#[test]
fn the_golden_writer_writes_only_a_missing_file_and_refuses_private_text() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("harness").join("claude-code.json");
    assert_eq!(write_missing(&path, "{}\n"), Ok(true));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}\n");
    assert_eq!(write_missing(&path, "{\"x\": 1}\n"), Ok(false));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}\n");
    let names = (Some("/Users/owner"), Some("owner"));
    assert_eq!(
        refuse_private("\"<path-1>/projects/app\"", names.0, names.1),
        Ok(())
    );
    assert_eq!(
        refuse_private("\"owners and co-owners\"", names.0, names.1),
        Ok(())
    );
    assert_eq!(refuse_private("\"task-sk-1\"", names.0, names.1), Ok(()));
    for leak in [
        "\"/Users/someone/projects\"",
        "\"/home/someone/projects\"",
        "\"/Users/owner\"",
        "\"built by Owner on the mac\"",
        "\"key sk-ant-api03-abc\"",
        "\"ghp_abc\"",
        "\"github_pat_11A\"",
        "\"AKIAABC\"",
    ] {
        assert!(refuse_private(leak, names.0, names.1).is_err(), "{leak}");
    }
}
