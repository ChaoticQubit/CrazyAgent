use crate::{ParseError, SOURCE_BYTES_MAX};

/// The kinds of token the grammar has: a word (name, keyword or scalar, dots kept inside),
/// the five punctuation marks, the arrow, and the end of the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Word,
    OpenBrace,
    CloseBrace,
    OpenParen,
    CloseParen,
    Comma,
    Colon,
    Arrow,
    End,
}

/// One token: its kind, its text in the source and the 1-based line and byte column it
/// starts at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Token<'s> {
    pub(crate) kind: Kind,
    pub(crate) text: &'s str,
    pub(crate) line: u32,
    pub(crate) column: u32,
}

impl Token<'_> {
    /// The refusal of this token. Input: the refusal code. Output: the refusal at its position.
    pub(crate) fn refuse(&self, code: &'static str) -> ParseError {
        ParseError {
            line: self.line,
            column: self.column,
            code,
        }
    }
}

/// Splits a source into tokens, ending with one `End` token. Only ASCII words, the
/// punctuation of the grammar and whitespace are accepted. Input: the source.
/// Output: the tokens, `cap_source_too_large` or `cap_unexpected_token`.
pub(crate) fn lex(source: &str) -> Result<Vec<Token<'_>>, ParseError> {
    if source.len() > SOURCE_BYTES_MAX {
        return Err(ParseError {
            line: 1,
            column: 1,
            code: "cap_source_too_large",
        });
    }
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let (mut index, mut line, mut column) = (0usize, 1u32, 1u32);
    while index < bytes.len() {
        let start = index;
        let byte = bytes[index];
        let kind = match byte {
            b'\n' => {
                index += 1;
                line += 1;
                column = 1;
                continue;
            }
            b' ' | b'\t' | b'\r' => {
                index += 1;
                column += 1;
                continue;
            }
            b'{' => Kind::OpenBrace,
            b'}' => Kind::CloseBrace,
            b'(' => Kind::OpenParen,
            b')' => Kind::CloseParen,
            b',' => Kind::Comma,
            b':' => Kind::Colon,
            b'-' if bytes.get(index + 1) == Some(&b'>') => {
                index += 1;
                Kind::Arrow
            }
            b'a'..=b'z' | b'A'..=b'Z' => {
                while index + 1 < bytes.len() && is_word_byte(bytes[index + 1]) {
                    index += 1;
                }
                Kind::Word
            }
            _ => {
                return Err(ParseError {
                    line,
                    column,
                    code: "cap_unexpected_token",
                });
            }
        };
        index += 1;
        tokens.push(Token {
            kind,
            text: &source[start..index],
            line,
            column,
        });
        column += u32::try_from(index - start).expect("a token fits the source bound");
    }
    tokens.push(Token {
        kind: Kind::End,
        text: "",
        line,
        column,
    });
    assert!(tokens.len() <= source.len() + 1);
    Ok(tokens)
}

/// Whether a byte may continue a word: ASCII letters, digits, `_` and `.`.
/// Input: the byte. Output: true when it continues a word.
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.'
}
