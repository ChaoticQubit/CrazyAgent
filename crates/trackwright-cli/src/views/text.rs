use ratatui::text::Span;

const MARK: char = '\u{fffd}';
const ELLIPSIS: char = '…';

/// Makes stored text safe to draw in a terminal. Input: text from the core or a host,
/// which agents may have written. Output: the text with every control character
/// (C0, DEL, C1, tab), line or paragraph separator, bidi control, zero-width character
/// and tag character replaced by
/// U+FFFD, so no escape sequence or hidden reordering reaches the terminal.
pub fn safe(text: &str) -> String {
    text.chars()
        .map(|character| if hidden(character) { MARK } else { character })
        .collect()
}

fn hidden(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{061c}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{feff}'
                | '\u{e0000}'..='\u{e007f}'
        )
}

/// Display width of text in terminal cells. Input: the text. Output: its width as
/// ratatui draws it.
pub fn width(text: &str) -> usize {
    Span::raw(text).width()
}

fn char_width(character: char) -> usize {
    let mut buffer = [0u8; 4];
    Span::raw(&*character.encode_utf8(&mut buffer)).width()
}

/// Cuts text to a width. Inputs: the text and the width in cells. Output: the text
/// unchanged when it fits, otherwise its longest prefix that fits with `…` after it.
pub fn cut(text: &str, limit: usize) -> String {
    if width(text) <= limit {
        return text.to_owned();
    }
    if limit == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for character in text.chars() {
        let cells = char_width(character);
        if used + cells > limit - 1 {
            break;
        }
        used += cells;
        out.push(character);
    }
    out.push(ELLIPSIS);
    assert!(width(&out) <= limit);
    out
}

/// Fits text to exactly a width. Inputs: the text and the width in cells. Output: the
/// text cut with `…` when too wide, then filled with spaces to the width.
pub fn pad(text: &str, limit: usize) -> String {
    let mut out = cut(text, limit);
    let used = width(&out);
    out.extend(std::iter::repeat_n(' ', limit - used));
    assert_eq!(width(&out), limit);
    out
}

/// Wraps one paragraph on spaces. Inputs: text without line breaks and a width of at
/// least one cell. Output: lines no wider than the width (a word wider than the width is
/// split across lines; a single character wider than the width gets its own line); runs
/// of spaces collapse and blank text gives no lines.
pub fn wrap(text: &str, limit: usize) -> Vec<String> {
    assert!(limit > 0);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split(' ').filter(|word| !word.is_empty()) {
        let joined = if current.is_empty() {
            width(word)
        } else {
            width(&current) + 1 + width(word)
        };
        if joined <= limit {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
            continue;
        }
        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        let mut rest = word;
        while width(rest) > limit {
            let mut used = 0;
            let mut split = 0;
            for (index, character) in rest.char_indices() {
                let cells = char_width(character);
                if used + cells > limit {
                    break;
                }
                used += cells;
                split = index + character.len_utf8();
            }
            if split == 0 {
                split = rest.chars().next().map_or(rest.len(), char::len_utf8);
            }
            lines.push(rest[..split].to_owned());
            rest = &rest[split..];
        }
        current = rest.to_owned();
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests;
