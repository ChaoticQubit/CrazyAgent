use super::*;

/// Hides words and lines that look like credentials before text leaves the engine: a
/// research log, or a past conversation's message, tool input or tool output. Takes the
/// text, one line or many. Returns it with the same line breaks and white space: each
/// line naming a credential field, and each line of a private key block from its BEGIN line
/// to its END line, a fixed notice, also every line up to an END line met before any BEGIN
/// or END line, since such text starts inside a block; each credential-like word, the value
/// after a key named like a credential, a URL's user part and a wrapped token's tail
/// replaced. This masks known shapes, not every secret: one in another shape, a short value
/// under a key not named like a credential or one split across messages stays.
pub(crate) fn redact(text: &str) -> String {
    let mut key_block = false;
    let mut marked = false;
    let mut wrapped = false;
    let mut shown: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let lower = line.to_ascii_lowercase();
        let begins = lower.contains(KEY_BLOCK_BEGIN);
        let ends = lower.contains(KEY_BLOCK_END);
        if ends && !begins && !marked {
            shown.fill_with(|| HIDDEN_LINE.to_owned());
            key_block = true;
        }
        marked |= begins || ends;
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
