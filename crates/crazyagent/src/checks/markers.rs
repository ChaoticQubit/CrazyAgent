use std::collections::HashMap;

/// The Rust marker: an attribute naming `ignore`, `#[ignore]` or inside `cfg_attr`.
pub const RUST_IGNORE: &str = "#[ignore]";
/// The Rust marker for a test that passes only by panicking, `#[should_panic]` in any form.
pub const RUST_SHOULD_PANIC: &str = "#[should_panic]";
/// The Node.js marker for a skipped test or suite call, as in `test.skip(`.
pub const NODE_SKIP_CALL: &str = ".skip(";
/// The Node.js marker for a `skip` option, as in `test(name, { skip: true }, ...)`.
pub const NODE_SKIP_OPTION: &str = "{ skip:";
/// The Node.js marker for a test written as a todo, as in `test.todo(`.
pub const NODE_TODO_CALL: &str = ".todo(";
/// The Node.js marker for a `todo` option, as in `test(name, { todo: true }, ...)`.
pub const NODE_TODO_OPTION: &str = "{ todo:";
/// The Node.js marker for a call that runs only some tests, as in `test.only(`.
pub const NODE_ONLY_CALL: &str = ".only(";
/// The Node.js marker for an `only` option, as in `test(name, { only: true }, ...)`.
pub const NODE_ONLY_OPTION: &str = "{ only:";
/// The Node.js marker for a skipped `it`, written `xit(`.
pub const NODE_XIT_CALL: &str = "xit(";
/// The Node.js marker for a skipped suite, written `xdescribe(`.
pub const NODE_XDESCRIBE_CALL: &str = "xdescribe(";
/// The Node.js marker for a skipped `test`, written `xtest(`.
pub const NODE_XTEST_CALL: &str = "xtest(";
/// The Python marker `@pytest.mark.skip`, with or without the decorator sign.
pub const PYTHON_SKIP: &str = "@pytest.mark.skip";
/// The Python marker `skipif`, as in `@pytest.mark.skipif(...)`.
pub const PYTHON_SKIPIF: &str = "skipif";
/// The Python marker `xfail`, as in `@pytest.mark.xfail`.
pub const PYTHON_XFAIL: &str = "xfail";
/// The Python marker for a skip raised while a test runs, `pytest.skip(`.
pub const PYTHON_SKIP_CALL: &str = "pytest.skip(";
/// The Python marker for a skip raised from a `unittest` case, `self.skipTest(`.
pub const PYTHON_SKIP_TEST: &str = "skipTest(";
/// The Python marker `@unittest.skip`.
pub const PYTHON_UNITTEST_SKIP: &str = "@unittest.skip";
/// The Python marker `skipIf`, as in `@unittest.skipIf(...)`.
pub const PYTHON_UNITTEST_SKIP_IF: &str = "skipIf";
/// The Python marker `skipUnless`, as in `@unittest.skipUnless(...)`.
pub const PYTHON_UNITTEST_SKIP_UNLESS: &str = "skipUnless";
/// The Python marker for a module skipped when an import fails, `pytest.importorskip(`.
pub const PYTHON_IMPORTORSKIP: &str = "importorskip(";
/// The Python marker `expectedFailure`, as in `@unittest.expectedFailure`.
pub const PYTHON_EXPECTED_FAILURE: &str = "expectedFailure";
/// The Python marker for a raised `unittest.SkipTest`, as in `raise SkipTest(...)`.
pub const PYTHON_SKIP_EXCEPTION: &str = "raise SkipTest";

const ATTRIBUTE_TOKENS_MAX: usize = 256;
const CHAR_ESCAPE_CHARS_MAX: usize = 16;
const INTERPOLATION_DEPTH_MAX: usize = 8;
const REGEX_KEYWORDS: [&str; 14] = [
    "return",
    "typeof",
    "case",
    "do",
    "else",
    "in",
    "of",
    "new",
    "delete",
    "void",
    "throw",
    "yield",
    "await",
    "instanceof",
];

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

/// One string literal, regular expression or comment: where it ends, whether its closing
/// delimiter was found before the text ended, and whether it is a value an operator may
/// follow rather than a comment.
#[derive(Debug, Clone, Copy)]
struct Span {
    end: usize,
    closed: bool,
    value: bool,
}

/// What the code before the current character ends with: the byte length of the code up
/// to its last character kept as code, and whether a value literal follows that character.
#[derive(Debug, Clone, Copy, Default)]
struct Before {
    code: usize,
    value: bool,
}

/// Lists the skip markers a change adds to one file: each marker in the head text that
/// the base text does not hold on an equal line followed by an equal next line, so an
/// untouched or moved skip is not new, a removed one is never listed and a marker inside
/// a string or a comment is never a marker. Inputs: the language, the base text (empty
/// for an added file) and the head text. Output: the added markers in head line order, or
/// None when the head leaves a string, regular expression or comment open, or nests
/// interpolations past `INTERPOLATION_DEPTH_MAX`, since every later line could then be
/// misread.
pub fn added(language: Language, base: &str, head: &str) -> Option<Vec<Marker>> {
    let (head_hits, head_closed) = found(language, head);
    if !head_closed {
        return None;
    }
    let (base_hits, _) = found(language, base);
    let mut known: HashMap<(&'static str, &str, &str), u32> = HashMap::new();
    for hit in base_hits {
        *known.entry(hit.key).or_default() += 1;
    }
    let markers: Vec<Marker> = head_hits
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
    Some(markers)
}

fn found(language: Language, text: &str) -> (Vec<Hit<'_>>, bool) {
    let (code, closed) = code_only(language, text);
    let tokens = tokens(&code);
    let lines: Vec<&str> = text.split('\n').collect();
    let hits = match language {
        Language::Rust => rust_hits(&tokens),
        Language::Node => node_hits(&tokens),
        Language::Python => python_hits(&tokens),
    };
    let hits = hits
        .into_iter()
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
        .collect();
    (hits, closed)
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
        let mut named: Vec<&'static str> = Vec::new();
        for inner in tokens.iter().skip(open).take(ATTRIBUTE_TOKENS_MAX) {
            let marker = match inner.text {
                "[" => {
                    depth += 1;
                    continue;
                }
                "]" => {
                    assert!(depth > 0);
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    continue;
                }
                "ignore" => RUST_IGNORE,
                "should_panic" => RUST_SHOULD_PANIC,
                _ => continue,
            };
            if !named.contains(&marker) {
                named.push(marker);
                hits.push((token.line, marker));
            }
        }
    }
    hits
}

fn node_hits(tokens: &[Token<'_>]) -> Vec<(u32, &'static str)> {
    let mut hits = Vec::new();
    for at in 0..tokens.len() {
        let (name, marker) = match texts(tokens, at, 3).as_slice() {
            [".", "skip", "("] => (at + 1, NODE_SKIP_CALL),
            [".", "todo", "("] => (at + 1, NODE_TODO_CALL),
            [".", "only", "("] => (at + 1, NODE_ONLY_CALL),
            ["{" | ",", "skip", ":"] => (at + 1, NODE_SKIP_OPTION),
            ["{" | ",", "todo", ":"] => (at + 1, NODE_TODO_OPTION),
            ["{" | ",", "only", ":"] => (at + 1, NODE_ONLY_OPTION),
            ["xit", "(", ..] => (at, NODE_XIT_CALL),
            ["xdescribe", "(", ..] => (at, NODE_XDESCRIBE_CALL),
            ["xtest", "(", ..] => (at, NODE_XTEST_CALL),
            _ => continue,
        };
        hits.push((tokens[name].line, marker));
    }
    hits
}

fn python_hits(tokens: &[Token<'_>]) -> Vec<(u32, &'static str)> {
    let mut hits = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        let marker = match token.text {
            "skipif" => PYTHON_SKIPIF,
            "xfail" => PYTHON_XFAIL,
            "skipTest" => PYTHON_SKIP_TEST,
            "skipIf" => PYTHON_UNITTEST_SKIP_IF,
            "skipUnless" => PYTHON_UNITTEST_SKIP_UNLESS,
            "importorskip" => PYTHON_IMPORTORSKIP,
            "expectedFailure" => PYTHON_EXPECTED_FAILURE,
            "SkipTest"
                if at
                    .checked_sub(1)
                    .is_some_and(|before| matches!(tokens[before].text, "raise" | "."))
                    || texts(tokens, at + 1, 1) == ["("] =>
            {
                PYTHON_SKIP_EXCEPTION
            }
            "pytest" if texts(tokens, at, 5) == ["pytest", ".", "mark", ".", "skip"] => PYTHON_SKIP,
            "pytest" if texts(tokens, at, 3) == ["pytest", ".", "skip"] => PYTHON_SKIP_CALL,
            "unittest" if texts(tokens, at, 3) == ["unittest", ".", "skip"] => PYTHON_UNITTEST_SKIP,
            _ => continue,
        };
        hits.push((token.line, marker));
    }
    hits
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_name(c: char) -> bool {
    is_word(c) || c == '$'
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

/// Blanks every string literal, regular expression and comment of a source text with
/// spaces, keeping each newline, so markers are read only from code and keep their line
/// numbers. Inputs: the language and the text. Output: the code with the same lines, and
/// whether every literal and comment closed before the text ended.
fn code_only(language: Language, text: &str) -> (String, bool) {
    let chars: Vec<char> = text.chars().collect();
    let mut code = String::with_capacity(text.len());
    let mut closed = true;
    let mut before = Before::default();
    let mut at = 0;
    while at < chars.len() {
        let span = match language {
            Language::Rust => rust_literal(&chars, at),
            Language::Node => node_literal(&chars, at, &code[..before.code], before.value),
            Language::Python => python_literal(&chars, at),
        };
        match span {
            Some(span) => {
                assert!(span.end > at && span.end <= chars.len());
                for c in &chars[at..span.end] {
                    code.push(if *c == '\n' { '\n' } else { ' ' });
                }
                closed &= span.closed;
                before.value |= span.value;
                at = span.end;
            }
            None => {
                code.push(chars[at]);
                if !chars[at].is_whitespace() {
                    before = Before {
                        code: code.len(),
                        value: false,
                    };
                }
                at += 1;
            }
        }
    }
    assert_eq!(code.matches('\n').count(), text.matches('\n').count());
    (code, closed)
}

fn rust_literal(chars: &[char], at: usize) -> Option<Span> {
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

fn node_literal(chars: &[char], at: usize, code: &str, after_value: bool) -> Option<Span> {
    let next = chars.get(at + 1).copied();
    match chars[at] {
        '/' if next == Some('/') => Some(line_end(chars, at)),
        '/' if next == Some('*') => Some(block_comment_end(chars, at)),
        '/' if !after_value && regex_allowed(code) => Some(regex_end(chars, at)),
        quote @ ('"' | '\'') => Some(quoted_end(chars, at, quote, false)),
        '`' => Some(template_end(chars, at, 0)),
        _ => None,
    }
}

fn python_literal(chars: &[char], at: usize) -> Option<Span> {
    match chars[at] {
        '#' => Some(line_end(chars, at)),
        '"' | '\'' => Some(python_string_end(chars, at, 0)),
        _ => None,
    }
}

/// Finds the end of one JavaScript template starting at its backtick, lexing each `${...}`
/// field so a quote or backtick inside it never ends the template. Inputs: the text, the
/// backtick's index and the nesting depth. Output: the span, open when the text ends
/// first or a field stays open.
fn template_end(chars: &[char], at: usize, depth: usize) -> Span {
    let mut index = at + 1;
    while index < chars.len() {
        match chars[index] {
            '\\' => index += 2,
            '`' => {
                return Span {
                    end: index + 1,
                    closed: true,
                    value: true,
                };
            }
            '$' if chars.get(index + 1) == Some(&'{') => {
                match template_field_end(chars, index + 2, depth) {
                    Some(end) => index = end,
                    None => break,
                }
            }
            _ => index += 1,
        }
    }
    Span {
        end: chars.len(),
        closed: false,
        value: true,
    }
}

/// Finds the end of one `${...}` field of a JavaScript template, lexing the strings and
/// templates inside it. Inputs: the text, the index after `${` and the nesting depth.
/// Output: the index after the closing brace, or None when a literal inside stays open,
/// the text ends or the nesting reaches `INTERPOLATION_DEPTH_MAX`.
fn template_field_end(chars: &[char], from: usize, depth: usize) -> Option<usize> {
    if depth >= INTERPOLATION_DEPTH_MAX {
        return None;
    }
    let mut braces = 1usize;
    let mut index = from;
    while index < chars.len() {
        let span = match chars[index] {
            '{' => {
                braces += 1;
                index += 1;
                continue;
            }
            '}' => {
                braces -= 1;
                index += 1;
                if braces == 0 {
                    return Some(index);
                }
                continue;
            }
            quote @ ('"' | '\'') => quoted_end(chars, index, quote, false),
            '`' => template_end(chars, index, depth + 1),
            _ => {
                index += 1;
                continue;
            }
        };
        if !span.closed {
            return None;
        }
        index = span.end;
    }
    None
}

/// Finds the end of one Python string starting at its quote, single or triple quoted; in
/// an f-string or t-string each `{...}` field is lexed with its own strings, so a quote
/// inside a field never ends the outer string. Inputs: the text, the quote's index and
/// the nesting depth. Output: the span, open when the text ends first, a single-quoted
/// string meets a newline or a field stays open.
fn python_string_end(chars: &[char], at: usize, depth: usize) -> Span {
    let quote = chars[at];
    let triple = chars.get(at + 1) == Some(&quote) && chars.get(at + 2) == Some(&quote);
    let fields = interpolated(chars, at);
    let width = if triple { 3 } else { 1 };
    let mut index = at + width;
    while index < chars.len() {
        let c = chars[index];
        let closes = c == quote
            && (!triple
                || (chars.get(index + 1) == Some(&quote) && chars.get(index + 2) == Some(&quote)));
        if closes {
            return Span {
                end: index + width,
                closed: true,
                value: true,
            };
        }
        if c == '\n' && !triple {
            return Span {
                end: index,
                closed: false,
                value: true,
            };
        }
        index = match c {
            '\\' => index + 2,
            '{' if fields && chars.get(index + 1) == Some(&'{') => index + 2,
            '{' if fields => match python_field_end(chars, index + 1, depth) {
                Some(end) => end,
                None => break,
            },
            _ => index + 1,
        };
    }
    Span {
        end: chars.len(),
        closed: false,
        value: true,
    }
}

/// Finds the end of one `{...}` field of a Python f-string: its expression with nested
/// strings, brackets and braces, then a format spec after a top-level `:`. Inputs: the
/// text, the index after `{` and the nesting depth. Output: the index after the closing
/// brace, or None when a string inside stays open, the text ends or the nesting reaches
/// `INTERPOLATION_DEPTH_MAX`.
fn python_field_end(chars: &[char], from: usize, depth: usize) -> Option<usize> {
    if depth >= INTERPOLATION_DEPTH_MAX {
        return None;
    }
    let mut braces = 1usize;
    let mut brackets = 0usize;
    let mut index = from;
    while index < chars.len() {
        match chars[index] {
            '{' => braces += 1,
            '}' => {
                braces -= 1;
                if braces == 0 {
                    return Some(index + 1);
                }
            }
            '(' | '[' => brackets += 1,
            ')' | ']' => brackets = brackets.saturating_sub(1),
            ':' if braces == 1 && brackets == 0 => return python_spec_end(chars, index + 1, depth),
            '"' | '\'' => {
                let span = python_string_end(chars, index, depth + 1);
                if !span.closed {
                    return None;
                }
                index = span.end;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn python_spec_end(chars: &[char], from: usize, depth: usize) -> Option<usize> {
    let mut index = from;
    while index < chars.len() {
        match chars[index] {
            '{' => index = python_field_end(chars, index + 1, depth + 1)?,
            '}' => return Some(index + 1),
            _ => index += 1,
        }
    }
    None
}

/// Tells an f-string or t-string by the one or two prefix letters before its quote, each
/// one of `r`, `b`, `u`, `f`, `t` in any case, with no name character before them and at
/// least one `f` or `t`. Inputs: the text and the quote's index. Output: whether `{`
/// opens a field.
fn interpolated(chars: &[char], at: usize) -> bool {
    let prefix = chars[..at]
        .iter()
        .rev()
        .take(3)
        .take_while(|c| is_word(**c))
        .count();
    if prefix == 0 || prefix > 2 {
        return false;
    }
    let letters = &chars[at - prefix..at];
    letters
        .iter()
        .all(|c| matches!(c.to_ascii_lowercase(), 'r' | 'b' | 'u' | 'f' | 't'))
        && letters
            .iter()
            .any(|c| matches!(c.to_ascii_lowercase(), 'f' | 't'))
}

/// Tells a JavaScript regular expression from a division by the code before the slash,
/// which ends with its last character kept as code: a value (a name that is not a
/// keyword, a number, a closing bracket) is divided, anything else starts an expression.
/// A slash after `<` closes a JSX tag. Input: the code before. Output: whether a regular
/// expression may start here.
fn regex_allowed(code: &str) -> bool {
    let Some(last) = code.chars().next_back() else {
        return true;
    };
    if matches!(last, ')' | ']' | '<') {
        return false;
    }
    if !is_name(last) {
        return true;
    }
    REGEX_KEYWORDS.iter().any(|keyword| {
        code.strip_suffix(*keyword)
            .is_some_and(|rest| !rest.chars().next_back().is_some_and(is_name))
    })
}

fn regex_end(chars: &[char], at: usize) -> Span {
    let mut index = at + 1;
    let mut class = false;
    while index < chars.len() {
        match chars[index] {
            '\n' => break,
            '\\' if matches!(chars.get(index + 1), None | Some('\n')) => break,
            '\\' => index += 2,
            '[' => {
                class = true;
                index += 1;
            }
            ']' => {
                class = false;
                index += 1;
            }
            '/' if !class => {
                let flags = chars[index + 1..]
                    .iter()
                    .take_while(|c| is_word(**c))
                    .count();
                return Span {
                    end: index + 1 + flags,
                    closed: true,
                    value: true,
                };
            }
            _ => index += 1,
        }
    }
    Span {
        end: line_end(chars, at).end,
        closed: false,
        value: true,
    }
}

fn line_end(chars: &[char], at: usize) -> Span {
    let end = chars[at..]
        .iter()
        .position(|c| *c == '\n')
        .map_or(chars.len(), |offset| at + offset);
    Span {
        end,
        closed: true,
        value: false,
    }
}

fn block_comment_end(chars: &[char], at: usize) -> Span {
    let mut index = at + 2;
    while index < chars.len() {
        if chars[index] == '*' && chars.get(index + 1) == Some(&'/') {
            return Span {
                end: index + 2,
                closed: true,
                value: false,
            };
        }
        index += 1;
    }
    Span {
        end: chars.len(),
        closed: false,
        value: false,
    }
}

fn nested_comment_end(chars: &[char], at: usize) -> Span {
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
    Span {
        end: index.min(chars.len()),
        closed: depth == 0,
        value: false,
    }
}

fn quoted_end(chars: &[char], at: usize, quote: char, multiline: bool) -> Span {
    let mut index = at + 1;
    while index < chars.len() {
        match chars[index] {
            '\\' => index += 2,
            c if c == quote => {
                return Span {
                    end: index + 1,
                    closed: true,
                    value: true,
                };
            }
            '\n' if !multiline => {
                return Span {
                    end: index,
                    closed: false,
                    value: true,
                };
            }
            _ => index += 1,
        }
    }
    Span {
        end: chars.len(),
        closed: false,
        value: true,
    }
}

fn rust_char_end(chars: &[char], at: usize) -> Option<Span> {
    let end = match (chars.get(at + 1), chars.get(at + 2)) {
        (Some('\\'), _) => {
            let limit = (at + CHAR_ESCAPE_CHARS_MAX).min(chars.len());
            (at + 3..limit)
                .find(|index| chars[*index] == '\'' || chars[*index] == '\n')
                .filter(|index| chars[*index] == '\'')
                .map(|index| index + 1)
        }
        (Some(c), Some('\'')) if *c != '\n' => Some(at + 3),
        _ => None,
    }?;
    Some(Span {
        end,
        closed: true,
        value: true,
    })
}

fn raw_prefix_allowed(chars: &[char], at: usize) -> bool {
    let before = |offset: usize| at.checked_sub(offset).map(|index| chars[index]);
    match before(1) {
        None => true,
        Some('b' | 'c') => before(2).is_none_or(|c| !is_word(c)),
        Some(c) => !is_word(c),
    }
}

fn raw_end(chars: &[char], at: usize) -> Option<Span> {
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
            return Some(Span {
                end: index + 1 + hashes,
                closed: true,
                value: true,
            });
        }
        index += 1;
    }
    Some(Span {
        end: chars.len(),
        closed: false,
        value: true,
    })
}
