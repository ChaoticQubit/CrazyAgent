pub(super) fn text_valid(text: &str, maximum: usize) -> bool {
    !text.trim().is_empty() && text.len() <= maximum && !text.contains('\0')
}

pub(super) fn memory_text_valid(text: &str, maximum: usize) -> bool {
    text_valid(text, maximum) && !hidden_text(text)
}

/// Finds characters a reviewer cannot see or that reorder what they see: controls other
/// than line breaks and tabs, bidi marks, zero-width, tag and other default-ignorable code
/// points, and line or paragraph separators. One emoji selector or joiner inside an emoji
/// stays allowed. Input: the text. Output: true when the text holds any other one.
pub(crate) fn hidden_text(text: &str) -> bool {
    if !text.chars().any(hidden_char) {
        return false;
    }
    let chars: Vec<char> = text.chars().collect();
    (0..chars.len()).any(|at| hidden_at(&chars, at))
}

/// Takes out every character [`hidden_text`] finds; line breaks, tabs and an emoji
/// selector or joiner inside an emoji stay. Input: the text. Output: the text without them.
pub(crate) fn without_hidden(text: &str) -> String {
    if !hidden_text(text) {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let out: String = (0..chars.len())
        .filter(|at| !hidden_at(&chars, *at))
        .map(|at| chars[at])
        .collect();
    assert!(out.len() < text.len());
    assert!(!hidden_text(&out));
    out
}

/// Shows every character hidden_text finds as a visible escape, `\uXXXX` or `\u{XXXXX}`,
/// the same form the desktop shows. Input: the text. Output: the text with those characters
/// escaped; line breaks, tabs and an emoji selector or joiner inside an emoji stay as they are.
pub fn visible_text(text: &str) -> String {
    if !hidden_text(text) {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 16);
    for at in 0..chars.len() {
        let point = u32::from(chars[at]);
        if !hidden_at(&chars, at) {
            out.push(chars[at]);
        } else if point > 0xffff {
            out.push_str(&format!("\\u{{{point:x}}}"));
        } else {
            out.push_str(&format!("\\u{point:04x}"));
        }
    }
    assert!(!hidden_text(&out));
    out
}

fn hidden_at(chars: &[char], at: usize) -> bool {
    hidden_char(chars[at]) && !emoji_part(chars, at)
}

pub(crate) fn hidden_char(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t')
        || matches!(
            c,
            '\u{00ad}'
                | '\u{034f}'
                | '\u{061c}'
                | '\u{115f}'
                | '\u{1160}'
                | '\u{17b4}'
                | '\u{17b5}'
                | '\u{180b}'..='\u{180f}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{3164}'
                | '\u{fe00}'..='\u{fe0f}'
                | '\u{feff}'
                | '\u{ffa0}'
                | '\u{fff0}'..='\u{fffb}'
                | '\u{1bca0}'..='\u{1bca3}'
                | '\u{1d173}'..='\u{1d17a}'
                | '\u{e0000}'..='\u{e0fff}'
        )
}

fn emoji_base(c: char) -> bool {
    matches!(
        c,
        '\u{00a9}'
            | '\u{00ae}'
            | '\u{203c}'
            | '\u{2049}'
            | '\u{2122}'
            | '\u{2139}'
            | '\u{2194}'..='\u{21aa}'
            | '\u{231a}'..='\u{23ff}'
            | '\u{24c2}'
            | '\u{25aa}'..='\u{27bf}'
            | '\u{2934}'
            | '\u{2935}'
            | '\u{2b05}'..='\u{2b55}'
            | '\u{3030}'
            | '\u{303d}'
            | '\u{3297}'
            | '\u{3299}'
            | '\u{1f000}'..='\u{1faff}'
    )
}

fn emoji_part(chars: &[char], at: usize) -> bool {
    let before = at.checked_sub(1).map(|i| chars[i]);
    let after = chars.get(at + 1).copied();
    match chars[at] {
        '\u{fe0e}' | '\u{fe0f}' => {
            before.is_some_and(emoji_base)
                && !after.is_some_and(|c| matches!(c, '\u{fe00}'..='\u{fe0f}'))
        }
        '\u{200d}' => {
            let base = match before {
                Some('\u{fe0f}') => at.checked_sub(2).map(|i| chars[i]),
                other => other,
            };
            base.is_some_and(emoji_base) && after.is_some_and(emoji_base)
        }
        _ => false,
    }
}
