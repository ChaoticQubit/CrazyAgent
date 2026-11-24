/// Every source file outside tests, as its path under `src` with `/` between parts and its
/// code ([`code_only`]). A file or folder whose name ends in `tests` or `tests.rs` is left
/// out. Input: the source folder. Output: the files.
pub(super) fn sources(src: &std::path::Path) -> Vec<(String, String)> {
    const ENTRIES_MAX: usize = 100_000;
    let mut folders = vec![src.to_path_buf()];
    let mut found = Vec::new();
    let mut seen = 0;
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            seen += 1;
            assert!(seen <= ENTRIES_MAX);
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name.ends_with("tests") || name.ends_with("tests.rs") {
                continue;
            }
            if path.is_dir() {
                folders.push(path);
                continue;
            }
            if !name.ends_with(".rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let parts: Vec<String> = path
                .strip_prefix(src)
                .unwrap()
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect();
            found.push((parts.join("/"), code_only(&text)));
        }
    }
    found
}

/// Rust source with every comment, string and character literal turned to spaces, newlines
/// kept, so a scan of its words and braces reads only code. Input: the source. Output: the
/// code, one character for each of the source's.
fn code_only(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut code = String::with_capacity(text.len());
    let mut at = 0;
    while at < chars.len() {
        let rest = &chars[at..];
        let after_word = at > 0 && (chars[at - 1].is_alphanumeric() || chars[at - 1] == '_');
        let skipped = if rest.starts_with(&['/', '/']) {
            rest.iter().position(|&c| c == '\n').unwrap_or(rest.len())
        } else if rest.starts_with(&['/', '*']) {
            comment_length(rest)
        } else {
            literal_length(rest, after_word)
        };
        assert!(skipped <= rest.len());
        if skipped == 0 {
            code.push(chars[at]);
            at += 1;
        } else {
            code.extend(
                rest[..skipped]
                    .iter()
                    .map(|&c| if c == '\n' { c } else { ' ' }),
            );
            at += skipped;
        }
    }
    code
}

/// The length of the block comment `rest` starts with, nested ones included, or all of
/// `rest` when it never closes. Input: characters starting at `/*`. Output: the length.
fn comment_length(rest: &[char]) -> usize {
    let mut depth = 0_usize;
    let mut at = 0;
    while at + 1 < rest.len() {
        match (rest[at], rest[at + 1]) {
            ('/', '*') => {
                depth += 1;
                at += 2;
            }
            ('*', '/') => {
                depth -= 1;
                at += 2;
                if depth == 0 {
                    return at;
                }
            }
            _ => at += 1,
        }
    }
    rest.len()
}

/// The length of the string, byte, C or raw string, or character literal `rest` starts
/// with, or 0 for anything else, a lifetime included; a letter right after a word starts no
/// prefixed literal. Inputs: the characters and whether a word ends just before them.
/// Output: the length, or all of `rest` for a string that never closes.
fn literal_length(rest: &[char], after_word: bool) -> usize {
    let prefix = if after_word {
        0
    } else {
        rest.iter()
            .take(2)
            .take_while(|&&c| matches!(c, 'b' | 'c' | 'r'))
            .count()
    };
    let raw = rest[..prefix].contains(&'r');
    let hashes = if raw {
        rest[prefix..].iter().take_while(|&&c| c == '#').count()
    } else {
        0
    };
    let open = prefix + hashes;
    match rest.get(open) {
        Some('"') if raw => {
            let close: Vec<char> = std::iter::once('"')
                .chain(std::iter::repeat_n('#', hashes))
                .collect();
            rest[open + 1..]
                .windows(close.len())
                .position(|window| window == close.as_slice())
                .map_or(rest.len(), |at| open + 1 + at + close.len())
        }
        Some('"') => {
            let mut at = open + 1;
            while at < rest.len() && rest[at] != '"' {
                at += if rest[at] == '\\' { 2 } else { 1 };
            }
            (at + 1).min(rest.len())
        }
        Some('\'') if !raw => match &rest[open..] {
            ['\'', '\\', _, tail @ ..] => tail
                .iter()
                .position(|&c| c == '\'')
                .map_or(0, |at| open + at + 4),
            ['\'', _, '\'', ..] => open + 3,
            _ => 0,
        },
        _ => 0,
    }
}

/// The byte offsets where `word` stands in `code` as a whole identifier, not inside a
/// longer one. Inputs: the code and the word. Output: the offsets, in order.
fn word_starts<'a>(code: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    let in_word = |c: char| c.is_alphanumeric() || c == '_';
    code.match_indices(word)
        .map(|(at, _)| at)
        .filter(move |&at| {
            !code[..at].chars().next_back().is_some_and(in_word)
                && !code[at + word.len()..].chars().next().is_some_and(in_word)
        })
}

/// Whether `code` holds `word` as a whole identifier. Inputs: the code and the word.
/// Output: the answer.
pub(super) fn has_word(code: &str, word: &str) -> bool {
    word_starts(code, word).next().is_some()
}

/// The kinds of the log drafts `code` builds, in order: the code after `kind:` with its
/// spaces removed, or None for a draft whose kind the scan cannot read, as with a `kind`
/// shorthand, a draft taken from another by `..` or a pattern. A `struct`, `impl` or other
/// item line naming the type is no draft. Input: code ([`code_only`]). Output: the kinds.
pub(super) fn draft_kinds(code: &str) -> Vec<Option<String>> {
    let mut kinds = Vec::new();
    for at in word_starts(code, "Draft") {
        let before = code[..at].trim_end();
        let item = before
            .rsplit(|c: char| !(c.is_alphanumeric() || c == '_'))
            .next()
            .unwrap_or_default();
        let after = code[at + "Draft".len()..].trim_start();
        if ["struct", "enum", "impl", "trait", "type"].contains(&item) || !after.starts_with('{') {
            continue;
        }
        kinds.push(literal_kind(&after[1..]));
    }
    kinds
}

/// The kind one draft literal names: the code after its own `kind:` field with the spaces
/// removed, or None for a `kind` shorthand or no kind field. Fields split only at commas
/// outside nested values. Input: the code right after the literal's `{`. Output: the kind.
fn literal_kind(body: &str) -> Option<String> {
    let mut depth = 0_usize;
    let mut field = String::new();
    for c in body.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth > 0 => depth -= 1,
            ',' | '}' if depth == 0 => {
                let value = field
                    .trim()
                    .strip_prefix("kind")
                    .map(str::trim_start)
                    .and_then(|rest| rest.strip_prefix(':'))
                    .filter(|value| !value.starts_with(':'));
                if let Some(value) = value {
                    return Some(value.split_whitespace().collect());
                }
                if c == '}' {
                    return None;
                }
                field.clear();
                continue;
            }
            _ => {}
        }
        field.push(c);
    }
    None
}

/// Whether `code` renames the log's draft (`Draft as`), re-exports it or the whole log
/// module with a `pub use`, or names it in a type alias, any of which would let a file build
/// drafts under a name [`draft_kinds`] does not read. Input: code ([`code_only`]). Output:
/// the answer.
pub(super) fn hides_draft(code: &str) -> bool {
    let aliased = word_starts(code, "type")
        .any(|at| has_word(code[at..].split(';').next().unwrap_or_default(), "Draft"));
    aliased
        || word_starts(code, "use").any(|at| {
            let statement = code[at..].split(';').next().unwrap_or_default();
            let flat = statement.split_whitespace().collect::<Vec<_>>().join(" ");
            let before = code[..at].trim_end();
            let before = before
                .strip_suffix(')')
                .and_then(|rest| rest.rsplit_once('('))
                .map_or(before, |(head, _)| head.trim_end());
            let public = before.ends_with("pub");
            let glob = flat.ends_with('*') && has_word(statement, "state_log");
            (has_word(statement, "Draft") && (flat.contains("Draft as ") || public))
                || (glob && public)
        })
}

/// V250-L1-N1-WALK-SHAPES: the draft scan reads a kind on the line after its field, refuses
/// a `kind` shorthand and a `..` base, and skips drafts in comments and strings, the type's
/// own `struct` line, other names ending in Draft, lifetimes and brace characters; renaming
/// or re-exporting the draft is caught.
#[test]
fn the_draft_scan_reads_every_literal_shape() {
    let code = code_only(concat!(
        "/// builds a Draft { kind: DOC } in docs\n",
        "pub struct Draft<'b> { kind: &'b str }\n",
        "fn a(log: &mut Appender<'_, '_>) -> &'static str {\n",
        "    let text = \"Draft { kind: TEXT }\"; let open = '{'; let quote = '\\'';\n",
        "    log.append(&Draft {\n        aggregate: Aggregate::Sessions,\n",
        "        kind:\n            LONG_KIND,\n        body: &[],\n    });\n",
        "    log.append(&state_log::Draft { kind, schema: 1 });\n",
        "    log.append(&Draft { ..other });\n",
        "    let MyDraft { kind: NOT_A_LOG_DRAFT } = mine;\n",
        "    /* outer /* Draft { kind: BLOCK } */ */\n",
        "    log.append(&Draft { body: b\"}\", note: r#\"{\"#, kind: SECOND });\n",
        "}\n",
    ));
    assert_eq!(
        draft_kinds(&code),
        [
            Some("LONG_KIND".to_owned()),
            None,
            None,
            Some("SECOND".to_owned())
        ]
    );
    assert!(hides_draft(&code_only(
        "pub use crate::state_log::{Appender, Draft};"
    )));
    assert!(hides_draft(&code_only(
        "use crate::state_log::Draft as Entry;"
    )));
    assert!(hides_draft(&code_only("pub(crate) use super::Draft;")));
    assert!(!hides_draft(&code_only(
        "use crate::state_log::{Aggregate as Kind, Appender, Draft};"
    )));
}

/// A file that re-exports the whole log module, or names the log's draft through a type
/// alias, hides its drafts from the scan just as a renamed import does; a private glob import
/// and a type that does not name the draft hide nothing.
#[test]
fn a_glob_re_export_or_a_type_alias_hides_the_draft() {
    assert!(hides_draft(&code_only("pub use crate::state_log::*;")));
    assert!(hides_draft(&code_only("pub(crate) use crate::state_log::*;")));
    assert!(hides_draft(&code_only("type D = Draft;")));
    assert!(hides_draft(&code_only(
        "type Entry<'a> = state_log::Draft<'a>;"
    )));
    assert!(!hides_draft(&code_only("use crate::state_log::*;")));
    assert!(!hides_draft(&code_only("type Kind = &'static str;")));
}

/// A comma inside a value nested in the draft literal does not end the field, so the
/// draft's own kind is read and a `kind` field of a nested literal is not.
#[test]
fn a_comma_inside_a_nested_value_does_not_end_the_kind() {
    let code = code_only(concat!(
        "log.append(&Draft { body: &to_vec(&Note { id: 1, kind: INNER, n: 2 })?, ",
        "kind: OUTER, schema: 1 });\n",
        "log.append(&Draft { kind: pick(A, B), schema: 1 });\n",
    ));
    assert_eq!(
        draft_kinds(&code),
        [Some("OUTER".to_owned()), Some("pick(A,B)".to_owned())]
    );
}

/// A link to a folder under the source folder is not walked, so no file is read twice under
/// two names.
#[cfg(unix)]
#[test]
fn a_folder_link_under_the_sources_is_not_walked() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("src");
    std::fs::create_dir_all(src.join("real")).unwrap();
    std::fs::write(src.join("real/a.rs"), "fn a() {}\n").unwrap();
    std::os::unix::fs::symlink(src.join("real"), src.join("linked")).unwrap();

    let names: Vec<String> = sources(&src).into_iter().map(|(name, _)| name).collect();

    assert_eq!(names, ["real/a.rs"]);
}
