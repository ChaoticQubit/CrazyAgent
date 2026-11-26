use super::*;

/// Hides words and lines that look like credentials before text leaves the engine: a
/// research log, or a past conversation's message, tool input or tool output. Takes the
/// text, one line or many. Returns it with the same line breaks and white space: each
/// line naming a credential field, and each line of a private key block from its BEGIN line
/// to its END line, a fixed notice, also every line up to a key block's END line met before
/// any BEGIN line or such END line, since such text starts inside a block; each
/// credential-like word, the value after a key named like a credential, a URL's user part
/// and a wrapped token's tail replaced. This masks known shapes, not every secret: one in
/// another shape, a short value under a key not named like a credential or one split across
/// messages stays.
pub(crate) fn redact(text: &str) -> String {
    let mut key_block = false;
    let mut marked = false;
    let mut wrapped = false;
    let mut shown: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let lower = line.to_ascii_lowercase();
        let begins = lower.contains(KEY_BLOCK_BEGIN);
        let ends = lower.contains(KEY_BLOCK_END);
        let key_end = ends && block_end(line);
        if key_end && !begins && !marked {
            shown.fill_with(|| HIDDEN_LINE.to_owned());
            key_block = true;
        }
        marked |= begins || key_end;
        key_block |= begins;
        let hidden = key_block || SECRET_MARKERS.iter().any(|marker| lower.contains(marker));
        key_block &= !ends;
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

/// Whether a line holds a key block's END line as PEM writes it: `-----END `, in any case,
/// then a label of capitals, digits, spaces and hyphens with at least one capital, then
/// `-----`. A separator such as `-----End of output-----` is not one. Input: the line.
/// Output: true when one is found anywhere in it, also inside a joined key such as a JSON
/// key file's value.
fn block_end(line: &str) -> bool {
    pem_line(line, "-----end ")
}

/// Whether a line holds a key block's BEGIN line as PEM writes it, with the label rule of
/// [`block_end`]. A separator such as `-----Begin output-----` is not one. Input: the line.
/// Output: true when one is found anywhere in it.
#[cfg(unix)]
pub(super) fn block_begin(line: &str) -> bool {
    pem_line(line, "-----begin ")
}

/// Whether a line holds `marker`, in any case, then a label of capitals, digits, spaces and
/// hyphens with at least one capital, then `-----`. Inputs: the line and the marker in lower
/// case. Output: true when one is found anywhere in the line.
fn pem_line(line: &str, marker: &str) -> bool {
    assert_eq!(marker, marker.to_ascii_lowercase());
    let lower = line.to_ascii_lowercase();
    assert_eq!(lower.len(), line.len());
    lower.match_indices(marker).any(|(at, found)| {
        let rest = &line[at + found.len()..];
        rest.find("-----").is_some_and(|size| {
            let label = &rest.as_bytes()[..size];
            label.iter().any(u8::is_ascii_uppercase)
                && label.iter().all(|byte| {
                    byte.is_ascii_uppercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b' ' | b'-')
                })
        })
    })
}
