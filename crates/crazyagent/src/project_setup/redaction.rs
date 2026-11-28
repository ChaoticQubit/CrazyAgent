use super::*;

/// Hides words and lines that look like credentials before text leaves the engine: a
/// research log, or a past conversation's message, tool input or tool output. Takes the
/// text, one line or many. Returns it with the same line breaks and white space: each
/// line naming a credential field, and each line of a private key block from its BEGIN line
/// to its END line, a fixed notice, also every line up to a key block's END line met before
/// any BEGIN line or such END line, since such text starts inside a block; each
/// credential-like word, the value after a key named like a credential, a URL's user part
/// and a wrapped token's tail replaced. A block opened by a PEM-shaped BEGIN line closes only
/// on a PEM-shaped END line. This masks known shapes, not every secret: one in another
/// shape, a short value under a key not named like a credential or one split across
/// messages stays.
pub(crate) fn redact(text: &str) -> String {
    let mut block = Block::Closed;
    let mut marked = false;
    let mut wrapped = false;
    let mut shown: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let lower = line.to_ascii_lowercase();
        let begins = lower.contains(KEY_BLOCK_BEGIN);
        let key_end = lower.contains(KEY_BLOCK_END) && block_end(line);
        if key_end && !begins && !marked {
            shown.fill_with(|| HIDDEN_LINE.to_owned());
            block = Block::Strict;
        }
        marked |= begins || key_end;
        let opened = block.opened_by(line, true);
        let hidden =
            opened != Block::Closed || SECRET_MARKERS.iter().any(|marker| lower.contains(marker));
        block = opened.closed_by(line);
        if hidden {
            wrapped = false;
            shown.push(HIDDEN_LINE.to_owned());
            continue;
        }
        let (kept, ends_hidden) = redact_words(line, wrapped);
        wrapped = ends_hidden;
        shown.push(kept);
    }
    assert_eq!(shown.len(), text.split('\n').count());
    shown.join("\n")
}

/// Whether a text is inside a key block, and how the block opened: by a PEM-shaped BEGIN
/// line, which only a PEM-shaped END line closes, or by a looser `-----begin`, which any
/// `-----end` closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Block {
    Closed,
    Strict,
    Loose,
}

impl Block {
    /// The block after a line's BEGIN, if any: a closed block opens, strict for a PEM-shaped
    /// BEGIN line, else loose when `loose` allows a looser `-----begin`; an open block stays
    /// as it is. Inputs: the block, the line and whether a loose BEGIN opens one. Output: the
    /// block for this line.
    pub(super) fn opened_by(self, line: &str, loose: bool) -> Block {
        if self != Block::Closed {
            return self;
        }
        if block_begin(line) {
            Block::Strict
        } else if loose && line.to_ascii_lowercase().contains(KEY_BLOCK_BEGIN) {
            Block::Loose
        } else {
            Block::Closed
        }
    }

    /// The block after a line's END, if any: a strict block closes only on a PEM-shaped END
    /// line, a loose one on any `-----end`. Inputs: the block and the line. Output: the
    /// block for the next line.
    pub(super) fn closed_by(self, line: &str) -> Block {
        let closes = match self {
            Block::Closed => false,
            Block::Strict => block_end(line),
            Block::Loose => line.to_ascii_lowercase().contains(KEY_BLOCK_END),
        };
        if closes { Block::Closed } else { self }
    }
}

/// Whether a line holds a key block's END line as PEM writes it: `-----END `, in any case,
/// then a key label (see [`pem_line`]), then `-----`. A separator such as
/// `-----End of output-----` is not one. Input: the line.
/// Output: true when one is found anywhere in it, also inside a joined key such as a JSON
/// key file's value.
fn block_end(line: &str) -> bool {
    pem_line(line, "-----end ")
}

/// Whether a line holds a key block's BEGIN line as PEM writes it, with the label rule of
/// [`block_end`]. A separator such as `-----Begin output-----` is not one. Input: the line.
/// Output: true when one is found anywhere in it.
fn block_begin(line: &str) -> bool {
    pem_line(line, "-----begin ")
}

/// Whether a line holds `marker`, in any case, then a key label, then `-----`. A key label is
/// letters, digits, spaces and hyphens, either with at least one capital and no lower-case
/// letter, as PEM writes them, or holding the word `key` in any case, as OpenVPN's mixed-case
/// key labels do. Inputs: the line and the marker in lower case. Output: true when one is
/// found anywhere in the line.
fn pem_line(line: &str, marker: &str) -> bool {
    assert_eq!(marker, marker.to_ascii_lowercase());
    let lower = line.to_ascii_lowercase();
    assert_eq!(lower.len(), line.len());
    lower.match_indices(marker).any(|(at, found)| {
        let rest = &line[at + found.len()..];
        rest.find("-----").is_some_and(|size| {
            let label = &rest.as_bytes()[..size];
            let shaped = |letter: fn(&u8) -> bool| {
                label.iter().all(|byte| {
                    letter(byte) || byte.is_ascii_digit() || matches!(byte, b' ' | b'-')
                })
            };
            let pem = label.iter().any(u8::is_ascii_uppercase) && shaped(u8::is_ascii_uppercase);
            let keyed = shaped(u8::is_ascii_alphabetic)
                && rest[..size]
                    .split([' ', '-'])
                    .any(|word| word.eq_ignore_ascii_case("key"));
            pem || keyed
        })
    })
}
