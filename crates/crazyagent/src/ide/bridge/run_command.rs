/// The most bytes one command typed into a project terminal may hold.
pub const COMMAND_BYTES_MAX: usize = 512;
/// The refusal of a blank command.
pub const EMPTY: &str = "ide_run_empty";
/// The refusal of a command over `COMMAND_BYTES_MAX` bytes.
pub const TOO_LONG: &str = "ide_run_too_long";
/// The refusal of a command with a line break.
pub const NOT_ONE_LINE: &str = "ide_run_not_one_line";
/// The refusal of a command with a control, bidirectional or invisible character.
pub const CONTROL: &str = "ide_run_control_character";
/// The refusal of a command that chains, pipes, redirects, substitutes or recalls history.
pub const CHAINED: &str = "ide_run_chained";
const CHAINING: [char; 7] = [';', '|', '&', '`', '<', '>', '!'];

/// Says why a command may not be typed into a project terminal, or that it may. The text is
/// checked as written, with no shell parsing, so a quoted `;` is refused too.
/// Input: the command as the owner saw it. Output: None for one plain single command, else
/// the refusal code.
pub fn problem(command: &str) -> Option<&'static str> {
    if command.trim().is_empty() {
        return Some(EMPTY);
    }
    if command.len() > COMMAND_BYTES_MAX {
        return Some(TOO_LONG);
    }
    if command.contains(['\n', '\r']) {
        return Some(NOT_ONE_LINE);
    }
    if command.chars().any(hidden) {
        return Some(CONTROL);
    }
    if command.contains(CHAINING) || command.contains("$(") || command.trim_start().starts_with('^')
    {
        return Some(CHAINED);
    }
    assert!(command.len() <= COMMAND_BYTES_MAX);
    assert!(!command.contains('\n'));
    None
}

/// Whether a character can hide or reorder what the owner reads: a control character, a
/// bidirectional mark, a line or paragraph separator, or an invisible format character.
/// Input: the character. Output: the answer.
fn hidden(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{ad}'
                | '\u{61c}'
                | '\u{180e}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{feff}'
                | '\u{fff9}'..='\u{fffb}'
                | '\u{e0000}'..='\u{e007f}'
        )
}

#[cfg(test)]
mod tests;
