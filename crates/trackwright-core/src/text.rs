use crate::model::{Refusal, codes};

pub const PROJECT_NAME_BYTES_MAX: usize = 200;
pub const TITLE_BYTES_MAX: usize = 200;
pub const BODY_BYTES_MAX: usize = 16 * 1024;
pub const CRITERION_BYTES_MAX: usize = 500;
pub const CHECK_REF_BYTES_MAX: usize = 512;
pub const CLOSE_REASON_BYTES_MAX: usize = 1024;
pub const VERSION_NAME_BYTES_MAX: usize = 64;
pub const LINK_KIND_BYTES_MAX: usize = 64;
pub const LINK_TARGET_BYTES_MAX: usize = 512;

/// The kinds of text the core stores, each with its byte bound, whether it may be empty
/// and whether it may span lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextField {
    ProjectName,
    Title,
    Body,
    Criterion,
    CheckRef,
    CloseReason,
    VersionName,
    LinkKind,
    LinkTarget,
}

impl TextField {
    /// The field's name in a refusal. Output: the name.
    pub fn name(self) -> &'static str {
        match self {
            TextField::ProjectName => "project name",
            TextField::Title => "title",
            TextField::Body => "body",
            TextField::Criterion => "criterion",
            TextField::CheckRef => "check reference",
            TextField::CloseReason => "close reason",
            TextField::VersionName => "version name",
            TextField::LinkKind => "link kind",
            TextField::LinkTarget => "link target",
        }
    }

    /// The field's bound in bytes. Output: the bound.
    pub fn bytes_max(self) -> usize {
        match self {
            TextField::ProjectName => PROJECT_NAME_BYTES_MAX,
            TextField::Title => TITLE_BYTES_MAX,
            TextField::Body => BODY_BYTES_MAX,
            TextField::Criterion => CRITERION_BYTES_MAX,
            TextField::CheckRef => CHECK_REF_BYTES_MAX,
            TextField::CloseReason => CLOSE_REASON_BYTES_MAX,
            TextField::VersionName => VERSION_NAME_BYTES_MAX,
            TextField::LinkKind => LINK_KIND_BYTES_MAX,
            TextField::LinkTarget => LINK_TARGET_BYTES_MAX,
        }
    }

    fn may_be_empty(self) -> bool {
        matches!(self, TextField::Body)
    }

    fn multiline(self) -> bool {
        matches!(self, TextField::Body | TextField::CloseReason)
    }
}

/// Checks one text before the core stores it. Refuses, never truncates: an empty required
/// text, one over its byte bound, a line break in a one-line field, and any character
/// `hidden` finds. Inputs: the field and the text. Output: Ok, or the refusal.
pub fn check(field: TextField, text: &str) -> Result<(), Refusal> {
    let name = field.name();
    if !field.may_be_empty() && text.trim().is_empty() {
        return Err(Refusal::new(
            codes::TEXT_EMPTY,
            format!("the {name} is empty"),
        ));
    }
    if text.len() > field.bytes_max() {
        return Err(Refusal::new(
            codes::TEXT_TOO_LONG,
            format!("the {name} is over {} bytes", field.bytes_max()),
        ));
    }
    if !field.multiline() && text.contains(['\n', '\r']) {
        return Err(Refusal::new(
            codes::TEXT_LINE_BREAK,
            format!("the {name} is one line"),
        ));
    }
    if hidden(text) {
        return Err(Refusal::new(
            codes::TEXT_HIDDEN_CHARACTERS,
            format!(
                "the {name} holds characters a reader cannot see or that reorder what they see"
            ),
        ));
    }
    Ok(())
}

/// Finds characters a reader cannot see or that reorder what they see: controls other
/// than line breaks and tabs, bidi marks, zero-width, tag and other default-ignorable code
/// points, and line or paragraph separators. One emoji selector or joiner inside an emoji
/// stays allowed. Input: the text. Output: true when the text holds any other one.
pub fn hidden(text: &str) -> bool {
    if !text.chars().any(hidden_char) {
        return false;
    }
    let chars: Vec<char> = text.chars().collect();
    (0..chars.len()).any(|at| hidden_char(chars[at]) && !emoji_part(&chars, at))
}

fn hidden_char(c: char) -> bool {
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
