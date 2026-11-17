use super::normalise;
use serde_json::json;

/// One run of Seatbelt caseless pattern text read back as the plain path it spells, the way
/// 2.4.2's `any_case` (sandbox.rs) writes one: the lowercase characters, and for each the byte
/// length of pattern read up to the end of the token that spelled it.
pub(super) struct Unspelled {
    pub(super) plain: Vec<char>,
    pub(super) ends: Vec<usize>,
    pub(super) folded: bool,
}

/// Reads the caseless pattern at the start of `text` back to plain lowercase text, token by
/// token: a letter class `[sS]`, a group whose first alternative spells it letter by letter
/// (`(s|S|ſ)`, `((s|S|ſ)[tT]|ﬅ|ﬆ)`), an escaped character or a plain path character. It
/// stops before a token it cannot read or one a quantifier follows, so the superset form and
/// `(/.*)?` end a run. Input: the pattern text. Output: what it read; `folded` is true when a
/// class or a group was among it.
pub(super) fn unspell(text: &str) -> Unspelled {
    let mut read = Unspelled {
        plain: Vec::new(),
        ends: Vec::new(),
        folded: false,
    };
    let mut at = 0;
    while let Some((plain, end, folded)) = token(text, at) {
        if text[end..].starts_with(['?', '*', '+', '{']) {
            break;
        }
        read.ends.extend(std::iter::repeat_n(end, plain.len()));
        read.plain.extend(plain);
        read.folded |= folded;
        at = end;
    }
    assert_eq!(read.plain.len(), read.ends.len());
    assert!(read.ends.windows(2).all(|pair| pair[0] <= pair[1]));
    read
}

/// Reads one token of caseless pattern text at byte `at`. Inputs: the text and the place.
/// Output: the plain lowercase characters it spells, where it ends and whether it was a class
/// or a group, or none when no readable token starts there.
fn token(text: &str, at: usize) -> Option<(Vec<char>, usize, bool)> {
    let rest = &text[at..];
    match rest.chars().next()? {
        '[' => {
            let close = rest.find(']')?;
            let class = &rest[1..close];
            let letter = class.chars().next()?;
            let same = class
                .chars()
                .all(|c| c.is_ascii_alphabetic() && c.eq_ignore_ascii_case(&letter));
            (class.len() >= 2 && same)
                .then(|| (vec![letter.to_ascii_lowercase()], at + close + 1, true))
        }
        '(' => {
            let close = group_end(rest)?;
            let first = first_alternative(&rest[1..close]);
            let read = unspell(first);
            let whole = !read.plain.is_empty() && read.ends.last() == Some(&first.len());
            whole.then(|| (read.plain, at + close + 1, true))
        }
        '\\' => {
            let next = rest[1..].chars().next()?;
            Some((
                vec![next.to_ascii_lowercase()],
                at + 1 + next.len_utf8(),
                false,
            ))
        }
        c if c.is_ascii_alphanumeric() || "/_-~@".contains(c) => {
            Some((vec![c.to_ascii_lowercase()], at + 1, false))
        }
        _ => None,
    }
}

/// Finds the `)` that closes the group a string starts with, skipping escapes and letter
/// classes. Input: text starting with `(`. Output: the closing byte offset, or none.
fn group_end(text: &str) -> Option<usize> {
    assert!(text.starts_with('('));
    let mut depth = 0usize;
    let mut chars = text.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next()?;
            }
            '[' => {
                chars.find(|(_, c)| *c == ']')?;
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

/// The first top-level alternative inside a group. Input: the text between the parentheses.
/// Output: the text before the first `|` no nested group or class holds.
fn first_alternative(inner: &str) -> &str {
    let mut depth = 0usize;
    let mut chars = inner.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '[' => {
                chars.find(|(_, c)| *c == ']');
            }
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            '|' if depth == 0 => return &inner[..at],
            _ => {}
        }
    }
    inner
}

/// Says whether a character ends a token inside a caseless run or a placeholder, so a `/`
/// after it continues a path rather than starting one. Input: the character before the `/`.
/// Output: true when the `/` is inside a path.
pub(super) fn inside_path(before: char) -> bool {
    super::path_char(before) || matches!(before, ']' | ')' | '>')
}

/// Reads the path component a caseless run starts with, when a class or a group spells part
/// of it and it ends at a `/` or where the name ends, not where the reader stopped inside it.
/// Input: text starting with `/`. Output: the component in lowercase and the bytes it takes
/// with its `/`, or none.
pub(super) fn component(text: &str) -> Option<(String, usize)> {
    assert!(text.starts_with('/'));
    let read = unspell(text);
    let len = read
        .plain
        .iter()
        .skip(1)
        .position(|c| *c == '/')
        .map_or(read.plain.len(), |at| at + 1);
    if len < 2 {
        return None;
    }
    let taken = read.ends[len - 1];
    let whole = if len < read.plain.len() {
        read.ends[len] > taken
    } else {
        let next = &text[taken..];
        !(next.starts_with(|c: char| super::path_char(c) || matches!(c, '[' | '\\'))
            || (next.starts_with('(') && !next.starts_with("(/")))
    };
    let name: String = read.plain[1..len].iter().collect();
    (whole && unspell(&text[..taken]).folded).then_some((name, taken))
}

/// Reads the folder name at the start of text, as a plain path or as 2.4.2's `regex_escape`
/// spells one for a Seatbelt regex: path characters, and `\.` or `\+` (the only path
/// characters it escapes; two backslashes in JSON text) as the character escaped. Input: the
/// text after a `/`. Output: the plain name and the bytes it takes, empty when none starts.
pub(super) fn escaped_name(text: &str) -> (String, usize) {
    let bytes = text.as_bytes();
    let mut name = String::new();
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        let slashes = bytes[at..].iter().take_while(|b| **b == b'\\').count();
        let escaped = bytes
            .get(at + slashes)
            .filter(|next| (1..=2).contains(&slashes) && matches!(next, b'.' | b'+'));
        if let Some(&next) = escaped {
            name.push(char::from(next));
            at += slashes + 1;
        } else if super::path_char(char::from(byte)) {
            name.push(char::from(byte));
            at += 1;
        } else {
            break;
        }
    }
    assert!(text.is_char_boundary(at) && name.len() <= at);
    (name, at)
}

/// Finds a caseless spelling that still names a user folder or a path below a temp folder
/// once read back, the per-user temp parents below /var/folders included, which the plain
/// checks cannot see. Input: the text. Output: the kind of path it holds, never the path,
/// or none.
pub(super) fn private(text: &str) -> Option<String> {
    for (at, _) in text.match_indices('/') {
        if text[..at].chars().next_back().is_some_and(inside_path) {
            continue;
        }
        let read = unspell(&text[at..]);
        if !read.folded {
            continue;
        }
        let plain: String = read.plain.iter().collect();
        if ["/users/", "/home/"]
            .iter()
            .any(|folder| plain.starts_with(folder))
        {
            return Some("holds a user path in caseless spelling".to_owned());
        }
        if [
            "/private/var/folders/",
            "/var/folders/",
            "/private/tmp/",
            "/tmp/",
        ]
        .iter()
        .any(|folder| plain.len() > folder.len() && plain.starts_with(folder))
        {
            return Some("holds a temp path in caseless spelling".to_owned());
        }
    }
    None
}

/// Spells a plain path as 2.4.2's `any_case` does for the letters these rows use: a letter
/// class, `s`, `k` and `i` as their groups, `st` as its fold group and `.` escaped.
/// Input: the path. Output: the pattern.
pub(super) fn spell(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    let mut pattern = String::new();
    let mut at = 0;
    while at < path.len() {
        let c = path.as_bytes()[at] as char;
        if lower[at..].starts_with("st") {
            pattern.push_str("((s|S|ſ)[tT]|ﬅ|ﬆ)");
            at += 2;
            continue;
        }
        match c.to_ascii_lowercase() {
            's' => pattern.push_str("(s|S|ſ)"),
            'k' => pattern.push_str("(k|K|K)"),
            'i' => pattern.push_str("(i|I|ı)"),
            '.' => pattern.push_str("\\."),
            l if l.is_ascii_alphabetic() => {
                pattern.push_str(&format!("[{l}{}]", l.to_ascii_uppercase()))
            }
            other => pattern.push(other),
        }
        at += 1;
    }
    pattern
}

#[test]
fn a_caseless_rule_reads_back_and_stops_at_a_quantified_group() {
    let read = unspell("((s|S|ſ)[tT]|ﬅ|ﬆ)[aA]\\.[kK]9(/.*)?$");
    assert_eq!(read.plain.iter().collect::<String>(), "sta.k9");
    assert!(read.folded);
    assert_eq!(
        read.ends.last(),
        Some(&"((s|S|ſ)[tT]|ﬅ|ﬆ)[aA]\\.[kK]9".len())
    );
    let superset = "/[aA]((s|S|ſ)|ß)((s|S|ſ)|ß)?";
    assert_eq!(unspell(superset).plain.iter().collect::<String>(), "/as");
    assert!(!unspell("/plain/path").folded);
    assert_eq!(spell("/Ab.st"), "/[aA][bB]\\.((s|S|ſ)[tT]|ﬅ|ﬆ)");
}

#[test]
fn the_temp_folder_in_caseless_spelling_reads_as_its_plain_placeholders() {
    let real = std::env::temp_dir().canonicalize().unwrap();
    let real = real.to_string_lossy();
    let entry = ".tmpAb12st";
    let rule = format!(
        "(regex #\"^({})?{}/{}/[pP][rR][oO][jJ][eE][cC][tT](s|S|ſ)/.+\")",
        spell("/System/Volumes/Data"),
        spell(&real),
        spell(entry)
    );
    let text = normalise(&json!({
        "a_plain": format!("{real}/{entry}/projects"),
        "b_rule": rule,
    }))
    .unwrap();
    assert!(text.contains("\"<path-1>/projects\""), "{text}");
    assert!(
        text.contains("<path-1>/[pP][rR][oO][jJ][eE][cC][tT](s|S|ſ)/.+"),
        "{text}"
    );
    assert!(!text.contains("[fF][oO][lL][dD]"), "{text}");
    assert_eq!(private(&text), None);
    assert_eq!(
        private(&rule).is_some(),
        real.starts_with("/private/var/folders/")
    );
}

#[test]
fn a_caseless_hex_id_component_reads_as_the_plain_id_s_number() {
    let id = "8[aA]479[dD][bB][cC]30[bB]22([fF][fF]|ﬀ)3017[fF][eE][dD]699[eE]7[eE][eE]85[dD]";
    let rules = |plain: &str, caseless: &str| {
        normalise(&json!({
            "a_plain": format!("tool-cache/{plain}"),
            "b_rule": format!("^/x/{}/{caseless}/(s|S|ſ)/.+", spell("tool-cache")),
            "c_kept": [
                format!("/{}/x", spell("a1234567890123b")),
                format!("/{}[bB]?/x", spell("a234567890123456")),
            ],
        }))
        .unwrap()
    };
    let first = rules("8a479dbc30b22ff3017fed699e7ee85d", id);
    assert!(first.contains("\"tool-cache/<id-1>\""), "{first}");
    assert!(first.contains("/<id-1>/(s|S|ſ)/.+"), "{first}");
    assert!(!first.contains("<id-2>"), "{first}");
    assert!(first.contains(&spell("a1234567890123b")), "{first}");
    assert!(first.contains(&spell("a234567890123456")), "{first}");
    let other = "07[aA]1[bB]38[fF]1182[aA]45[dD][bB][dD][eE][cC]3210[bB]8[fF]85[cC]48";
    assert_eq!(
        super::compare(
            first.as_bytes(),
            &rules("07a1b38f1182a45dbdec3210b8f85c48", other)
        ),
        Ok(())
    );
    assert_eq!(
        component(&format!("/{id}(/.*)?")).map(|(_, n)| n),
        Some(id.len() + 1)
    );
    assert_eq!(component("/[aA]1((s|S|ſ)|ß)?"), None);
    assert_eq!(component("/plain1234567890abcdef/x"), None);
}

#[test]
fn a_temp_entry_in_a_regex_escaped_path_reads_as_its_plain_placeholder() {
    let real = std::env::temp_dir().canonicalize().unwrap();
    let real = real.to_string_lossy();
    assert!(!real.contains(['.', '+']), "{real}");
    let text = normalise(&json!({
        "a_plain": format!("{real}/.tmpAb12st/projects"),
        "b_rule": format!("(regex #\"^{real}/\\.tmpAb12st/projects/(.*/)?(\\.npmrc)(/.*)?$\")"),
    }))
    .unwrap();
    assert!(text.contains("\"<path-1>/projects\""), "{text}");
    assert!(text.contains("^<path-1>/projects/(.*/)?"), "{text}");
    assert!(!text.contains("tmpAb12st"), "{text}");
    assert_eq!(escaped_name("a\\+b\\.c/x"), ("a+b.c".to_owned(), 7));
    assert_eq!(escaped_name("a\\\\.b\""), ("a.b".to_owned(), 5));
    assert_eq!(escaped_name("ab\\(c"), ("ab".to_owned(), 2));
    assert_eq!(escaped_name("\\\\\\.x"), (String::new(), 0));
}

#[test]
fn an_escaped_tool_root_below_the_owner_home_passes_and_an_unknown_name_is_refused() {
    let names = (Some("/Users/owner"), Some("owner"));
    let golden = |root: &str| {
        normalise(&json!([format!(
            "(regex #\"^<owner-home>/{}/(.*/)?(\\.npmrc)(/.*)?$\")",
            root.replace('.', "\\.")
        )]))
        .unwrap()
    };
    for known in [".cargo", ".rustup", ".local"] {
        let text = golden(known);
        assert!(text.contains("<owner-home>/\\\\."), "{text}");
        assert_eq!(super::refuse_private(&text, names.0, names.1), Ok(()));
    }
    assert!(super::refuse_private(&golden(".unlisted"), names.0, names.1).is_err());
    assert!(super::refuse_private("\"<owner-home>/\\(x\"", names.0, names.1).is_err());
}

#[test]
fn a_caseless_user_or_per_user_temp_path_is_refused_and_a_masked_one_is_not() {
    let names = (Some("/Users/owner"), Some("owner"));
    for leak in [
        format!("\"^{}/x\"", spell("/Users/someone")),
        format!(
            "\"^({})?{}\"",
            spell("/System/Volumes/Data"),
            spell("/private/var/folders/_c/ab12/T")
        ),
        format!("\"{}((s|S|ſ)|ß)?\"", spell("/var/folders/ab")),
        format!("\"^{}/.+\"", spell("/private/tmp/ca-p5-home-Ab12")),
    ] {
        assert!(private(&leak).is_some(), "{leak}");
        assert!(
            super::refuse_private(&leak, names.0, names.1).is_err(),
            "{leak}"
        );
    }
    for clean in [
        format!("\"<path-1>{}\"", spell("/home/x")),
        format!("\"{}\"", spell("/private/var/folders")),
        format!("\"{}\"", spell("/Applications/Xcode.app")),
        "\"/Users-like plain text\"".to_owned(),
    ] {
        assert_eq!(private(&clean), None, "{clean}");
    }
}
