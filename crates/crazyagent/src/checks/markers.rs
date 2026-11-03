use std::collections::HashMap;

/// The Rust marker: an attribute naming `ignore`, `#[ignore]` or inside `cfg_attr`.
pub const RUST_IGNORE: &str = "#[ignore]";
/// The Node.js marker for a skipped test or suite call, as in `test.skip(`.
pub const NODE_SKIP_CALL: &str = ".skip(";
/// The Node.js marker for a `skip` option, as in `test(name, { skip: true }, ...)`.
pub const NODE_SKIP_OPTION: &str = "{ skip:";
/// The Node.js marker for a test written as a todo, as in `test.todo(`.
pub const NODE_TODO_CALL: &str = ".todo(";
/// The Python marker `@pytest.mark.skip`, with or without the decorator sign.
pub const PYTHON_SKIP: &str = "@pytest.mark.skip";
/// The Python marker `skipif`, as in `@pytest.mark.skipif(...)`.
pub const PYTHON_SKIPIF: &str = "skipif";
/// The Python marker `xfail`, as in `@pytest.mark.xfail`.
pub const PYTHON_XFAIL: &str = "xfail";

const ATTRIBUTE_TOKENS_MAX: usize = 256;
const CHAR_ESCAPE_CHARS_MAX: usize = 16;

/// The languages whose test skip markers the engine reads, each chosen by file name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Node,
    Python,
}

impl Language {
    /// Picks the language of a changed path by its extension, in any letter case, so a
    /// test file a case-insensitive disk runs is never passed over. Input: the path.
    /// Output: the language, or None for a file no supported toolchain reads as a test.
    pub fn of_path(path: &str) -> Option<Self> {
        let name = path.rsplit('/').next().unwrap_or(path);
        let (_, extension) = name.rsplit_once('.')?;
        match extension.to_ascii_lowercase().as_str() {
            "rs" => Some(Self::Rust),
            "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" => Some(Self::Node),
            "py" => Some(Self::Python),
            _ => None,
        }
    }
}

/// One skip marker a change adds: the 1-based line of the head text it starts on and the
/// marker as the integrity block names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marker {
    pub line: u32,
    pub marker: &'static str,
}

struct Hit<'a> {
    line: u32,
    marker: &'static str,
    key: (&'static str, &'a str, &'a str),
}

struct Token<'a> {
    line: u32,
    text: &'a str,
}

/// Lists the skip markers a change adds to one file: each marker in the head text that
/// the base text does not hold on an equal line followed by an equal next line, so an
/// untouched or moved skip is not new, a removed one is never listed and a marker inside
/// a string or a comment is never a marker. Inputs: the language, the base text (empty
/// for an added file) and the head text. Output: the added markers in head line order.
pub fn added(language: Language, base: &str, head: &str) -> Vec<Marker> {
    let mut known: HashMap<(&'static str, &str, &str), u32> = HashMap::new();
    for hit in found(language, base) {
        *known.entry(hit.key).or_default() += 1;
    }
    let markers: Vec<Marker> = found(language, head)
        .into_iter()
        .filter_map(|hit| match known.get_mut(&hit.key) {
            Some(count) if *count > 0 => {
                *count -= 1;
                None
            }
            _ => Some(Marker {
                line: hit.line,
                marker: hit.marker,
            }),
        })
        .collect();
    assert!(markers.windows(2).all(|pair| pair[0].line <= pair[1].line));
    markers
}

fn found(language: Language, text: &str) -> Vec<Hit<'_>> {
    let code = code_only(language, text);
    let tokens = tokens(&code);
    let lines: Vec<&str> = text.split('\n').collect();
    let hits = match language {
        Language::Rust => rust_hits(&tokens),
        Language::Node => node_hits(&tokens),
        Language::Python => python_hits(&tokens),
    };
    hits.into_iter()
        .map(|(line, marker)| {
            let index = usize::try_from(line).expect("a line number fits in usize") - 1;
            assert!(index < lines.len());
            let next = lines[index + 1..]
                .iter()
                .map(|line| line.trim())
                .find(|line| !line.is_empty())
                .unwrap_or("");
            Hit {
                line,
                marker,
                key: (marker, lines[index].trim(), next),
            }
        })
        .collect()
}

fn texts<'a>(tokens: &'a [Token<'_>], at: usize, count: usize) -> Vec<&'a str> {
    tokens
        .iter()
        .skip(at)
        .take(count)
        .map(|token| token.text)
        .collect()
}

fn rust_hits(tokens: &[Token<'_>]) -> Vec<(u32, &'static str)> {
    let mut hits = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        if token.text != "#" {
            continue;
        }
        let mut open = at + 1;
        if tokens.get(open).map(|token| token.text) == Some("!") {
            open += 1;
        }
        if tokens.get(open).map(|token| token.text) != Some("[") {
            continue;
        }
        let mut depth = 0usize;
        for inner in tokens.iter().skip(open).take(ATTRIBUTE_TOKENS_MAX) {
            match inner.text {
                "[" => depth += 1,
                "]" => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                "ignore" => {
                    hits.push((token.line, RUST_IGNORE));
                    break;
                }
                _ => {}
            }
        }
    }
    hits
}

fn node_hits(tokens: &[Token<'_>]) -> Vec<(u32, &'static str)> {
    let mut hits = Vec::new();
    for at in 0..tokens.len() {
        let marker = match texts(tokens, at, 3).as_slice() {
            [".", "skip", "("] => NODE_SKIP_CALL,
            [".", "todo", "("] => NODE_TODO_CALL,
            ["{" | ",", "skip", ":"] => NODE_SKIP_OPTION,
            _ => continue,
        };
        hits.push((tokens[at + 1].line, marker));
    }
    hits
}

fn python_hits(tokens: &[Token<'_>]) -> Vec<(u32, &'static str)> {
    let mut hits = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        let marker = match token.text {
            "skipif" => PYTHON_SKIPIF,
            "xfail" => PYTHON_XFAIL,
            "pytest" if texts(tokens, at, 5) == ["pytest", ".", "mark", ".", "skip"] => PYTHON_SKIP,
            _ => continue,
        };
        hits.push((token.line, marker));
    }
    hits
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn tokens(code: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut line = 1u32;
    let mut word: Option<usize> = None;
    for (index, c) in code.char_indices() {
        if is_word(c) {
            word.get_or_insert(index);
            continue;
        }
        if let Some(from) = word.take() {
            tokens.push(Token {
                line,
                text: &code[from..index],
            });
        }
        if c == '\n' {
            line += 1;
        } else if !c.is_whitespace() {
            tokens.push(Token {
                line,
                text: &code[index..index + c.len_utf8()],
            });
        }
    }
    if let Some(from) = word {
        tokens.push(Token {
            line,
            text: &code[from..],
        });
    }
    tokens
}

/// Blanks every string literal and comment of a source text with spaces, keeping each
/// newline, so markers are read only from code and keep their line numbers. Inputs: the
/// language and the text. Output: the code with the same lines.
fn code_only(language: Language, text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut code = String::with_capacity(text.len());
    let mut at = 0;
    while at < chars.len() {
        let end = match language {
            Language::Rust => rust_literal(&chars, at),
            Language::Node => node_literal(&chars, at),
            Language::Python => python_literal(&chars, at),
        };
        match end {
            Some(end) => {
                assert!(end > at && end <= chars.len());
                for c in &chars[at..end] {
                    code.push(if *c == '\n' { '\n' } else { ' ' });
                }
                at = end;
            }
            None => {
                code.push(chars[at]);
                at += 1;
            }
        }
    }
    assert_eq!(code.matches('\n').count(), text.matches('\n').count());
    code
}

fn rust_literal(chars: &[char], at: usize) -> Option<usize> {
    let next = chars.get(at + 1).copied();
    match chars[at] {
        '/' if next == Some('/') => Some(line_end(chars, at)),
        '/' if next == Some('*') => Some(nested_comment_end(chars, at)),
        '"' => Some(quoted_end(chars, at, '"', true)),
        '\'' => rust_char_end(chars, at),
        'r' if raw_prefix_allowed(chars, at) => raw_end(chars, at),
        _ => None,
    }
}

fn node_literal(chars: &[char], at: usize) -> Option<usize> {
    let next = chars.get(at + 1).copied();
    match chars[at] {
        '/' if next == Some('/') => Some(line_end(chars, at)),
        '/' if next == Some('*') => Some(block_comment_end(chars, at)),
        quote @ ('"' | '\'') => Some(quoted_end(chars, at, quote, false)),
        '`' => Some(quoted_end(chars, at, '`', true)),
        _ => None,
    }
}

fn python_literal(chars: &[char], at: usize) -> Option<usize> {
    match chars[at] {
        '#' => Some(line_end(chars, at)),
        quote @ ('"' | '\'') => {
            let triple = chars.get(at + 1) == Some(&quote) && chars.get(at + 2) == Some(&quote);
            Some(if triple {
                triple_end(chars, at, quote)
            } else {
                quoted_end(chars, at, quote, false)
            })
        }
        _ => None,
    }
}

fn line_end(chars: &[char], at: usize) -> usize {
    chars[at..]
        .iter()
        .position(|c| *c == '\n')
        .map_or(chars.len(), |offset| at + offset)
}

fn block_comment_end(chars: &[char], at: usize) -> usize {
    let mut index = at + 2;
    while index < chars.len() {
        if chars[index] == '*' && chars.get(index + 1) == Some(&'/') {
            return index + 2;
        }
        index += 1;
    }
    chars.len()
}

fn nested_comment_end(chars: &[char], at: usize) -> usize {
    let mut depth = 1usize;
    let mut index = at + 2;
    while index < chars.len() && depth > 0 {
        let next = chars.get(index + 1).copied();
        if chars[index] == '/' && next == Some('*') {
            depth += 1;
            index += 2;
        } else if chars[index] == '*' && next == Some('/') {
            depth -= 1;
            index += 2;
        } else {
            index += 1;
        }
    }
    index.min(chars.len())
}

fn quoted_end(chars: &[char], at: usize, quote: char, multiline: bool) -> usize {
    let mut index = at + 1;
    while index < chars.len() {
        match chars[index] {
            '\\' => index += 2,
            c if c == quote => return index + 1,
            '\n' if !multiline => return index,
            _ => index += 1,
        }
    }
    index.min(chars.len())
}

fn triple_end(chars: &[char], at: usize, quote: char) -> usize {
    let mut index = at + 3;
    while index < chars.len() {
        if chars[index] == '\\' {
            index += 2;
            continue;
        }
        if chars[index] == quote
            && chars.get(index + 1) == Some(&quote)
            && chars.get(index + 2) == Some(&quote)
        {
            return index + 3;
        }
        index += 1;
    }
    index.min(chars.len())
}

fn rust_char_end(chars: &[char], at: usize) -> Option<usize> {
    match (chars.get(at + 1), chars.get(at + 2)) {
        (Some('\\'), _) => {
            let limit = (at + CHAR_ESCAPE_CHARS_MAX).min(chars.len());
            (at + 3..limit)
                .find(|index| chars[*index] == '\'' || chars[*index] == '\n')
                .filter(|index| chars[*index] == '\'')
                .map(|index| index + 1)
        }
        (Some(c), Some('\'')) if *c != '\n' => Some(at + 3),
        _ => None,
    }
}

fn raw_prefix_allowed(chars: &[char], at: usize) -> bool {
    let before = |offset: usize| at.checked_sub(offset).map(|index| chars[index]);
    match before(1) {
        None => true,
        Some('b' | 'c') => before(2).is_none_or(|c| !is_word(c)),
        Some(c) => !is_word(c),
    }
}

fn raw_end(chars: &[char], at: usize) -> Option<usize> {
    let hashes = chars[at + 1..].iter().take_while(|c| **c == '#').count();
    let open = at + 1 + hashes;
    if chars.get(open) != Some(&'"') {
        return None;
    }
    let mut index = open + 1;
    while index < chars.len() {
        if chars[index] == '"'
            && chars[index + 1..]
                .iter()
                .take(hashes)
                .filter(|c| **c == '#')
                .count()
                == hashes
        {
            return Some(index + 1 + hashes);
        }
        index += 1;
    }
    Some(chars.len())
}
