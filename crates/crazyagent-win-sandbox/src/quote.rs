use std::fmt;
use std::iter::repeat_n;

/// The longest command line `CreateProcessW` accepts, in UTF-16 units, without its final NUL.
pub const COMMAND_LINE_MAX: usize = 32_766;

const QUOTE: u16 = b'"' as u16;
const BACKSLASH: u16 = b'\\' as u16;
const SPACE: u16 = b' ' as u16;
const TAB: u16 = b'\t' as u16;
const NEWLINE: u16 = b'\n' as u16;
const VERTICAL_TAB: u16 = 0x0b;

/// Why a program and its arguments cannot become one command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteError {
    EmptyProgram,
    ProgramQuote,
    Nul,
    TooLong,
}

impl QuoteError {
    /// The plain reason for this refusal. Input: the error. Output: one sentence.
    pub fn reason(self) -> &'static str {
        match self {
            Self::EmptyProgram => "the program path is empty",
            Self::ProgramQuote => "the program path holds a double quote",
            Self::Nul => "an argument holds a NUL character",
            Self::TooLong => "the command line is longer than Windows allows",
        }
    }
}

impl fmt::Display for QuoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reason())
    }
}

impl std::error::Error for QuoteError {}

/// Builds the `CreateProcessW` command line that a child using the Microsoft C runtime splits
/// back into exactly these arguments: the program in quotes with its backslashes as they are,
/// then each argument, quoted when it is empty or holds a space, tab, newline or quote, with
/// backslashes doubled before a quote or the closing quote. Inputs: the program and arguments
/// as UTF-16 units. Output: the line without its final NUL, or the refusal.
pub fn command_line(program: &[u16], args: &[Vec<u16>]) -> Result<Vec<u16>, QuoteError> {
    if program.is_empty() {
        return Err(QuoteError::EmptyProgram);
    }
    if program.contains(&0) || args.iter().any(|arg| arg.contains(&0)) {
        return Err(QuoteError::Nul);
    }
    if program.contains(&QUOTE) {
        return Err(QuoteError::ProgramQuote);
    }
    let mut line = Vec::with_capacity(program.len() + 2);
    line.push(QUOTE);
    line.extend_from_slice(program);
    line.push(QUOTE);
    for arg in args {
        line.push(SPACE);
        push_arg(arg, &mut line);
        if line.len() > COMMAND_LINE_MAX {
            return Err(QuoteError::TooLong);
        }
    }
    if line.len() > COMMAND_LINE_MAX {
        return Err(QuoteError::TooLong);
    }
    assert!(!line.contains(&0));
    assert_eq!(line.first(), Some(&QUOTE));
    Ok(line)
}

fn push_arg(arg: &[u16], line: &mut Vec<u16>) {
    let quoted = arg.is_empty()
        || arg
            .iter()
            .any(|unit| matches!(*unit, SPACE | TAB | NEWLINE | VERTICAL_TAB | QUOTE));
    if !quoted {
        line.extend_from_slice(arg);
        return;
    }
    line.push(QUOTE);
    let mut backslashes = 0usize;
    for &unit in arg {
        if unit == BACKSLASH {
            backslashes += 1;
            continue;
        }
        let escaped = if unit == QUOTE {
            backslashes * 2 + 1
        } else {
            backslashes
        };
        line.extend(repeat_n(BACKSLASH, escaped));
        line.push(unit);
        backslashes = 0;
    }
    line.extend(repeat_n(BACKSLASH, backslashes * 2));
    line.push(QUOTE);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn line(program: &str, args: &[&str]) -> String {
        let args: Vec<Vec<u16>> = args.iter().map(|arg| units(arg)).collect();
        String::from_utf16(&command_line(&units(program), &args).unwrap()).unwrap()
    }

    fn split(line: &str) -> (String, Vec<String>) {
        let chars: Vec<char> = line.chars().collect();
        assert_eq!(chars[0], '"');
        let end = chars[1..].iter().position(|c| *c == '"').unwrap() + 1;
        let program: String = chars[1..end].iter().collect();
        let mut args = Vec::new();
        let mut at = end + 1;
        loop {
            while at < chars.len() && matches!(chars[at], ' ' | '\t') {
                at += 1;
            }
            if at >= chars.len() {
                break;
            }
            let mut arg = String::new();
            let mut quoted = false;
            while at < chars.len() {
                let mut backslashes = 0;
                while at < chars.len() && chars[at] == '\\' {
                    backslashes += 1;
                    at += 1;
                }
                if at < chars.len() && chars[at] == '"' {
                    arg.extend(std::iter::repeat_n('\\', backslashes / 2));
                    if backslashes % 2 == 1 {
                        arg.push('"');
                    } else {
                        quoted = !quoted;
                    }
                    at += 1;
                    continue;
                }
                arg.extend(std::iter::repeat_n('\\', backslashes));
                if at >= chars.len() || (!quoted && matches!(chars[at], ' ' | '\t')) {
                    break;
                }
                arg.push(chars[at]);
                at += 1;
            }
            args.push(arg);
        }
        (program, args)
    }

    #[test]
    fn plain_arguments_stay_as_they_are_and_the_program_is_always_quoted() {
        assert_eq!(
            line(r"C:\Windows\System32\cmd.exe", &["/C", "exit", "0"]),
            r#""C:\Windows\System32\cmd.exe" /C exit 0"#
        );
        assert_eq!(
            line(r"C:\Program Files\node\node.exe", &[r"C:\work\a\b"]),
            r#""C:\Program Files\node\node.exe" C:\work\a\b"#
        );
    }

    #[test]
    fn empty_spaced_and_quoted_arguments_follow_the_microsoft_runtime_rules() {
        assert_eq!(line(r"C:\p.exe", &[""]), r#""C:\p.exe" """#);
        assert_eq!(line(r"C:\p.exe", &["a b"]), r#""C:\p.exe" "a b""#);
        assert_eq!(line(r"C:\p.exe", &["a\tb"]), "\"C:\\p.exe\" \"a\tb\"");
        assert_eq!(line(r"C:\p.exe", &[r#"a"b"#]), r#""C:\p.exe" "a\"b""#);
        assert_eq!(line(r"C:\p.exe", &[r#"a\"b"#]), r#""C:\p.exe" "a\\\"b""#);
        assert_eq!(
            line(r"C:\p.exe", &[r"C:\dir with space\"]),
            r#""C:\p.exe" "C:\dir with space\\""#
        );
        assert_eq!(line(r"C:\p.exe", &[r"C:\dir\"]), r#""C:\p.exe" C:\dir\"#);
        assert_eq!(line(r"C:\dir\", &["x"]), r#""C:\dir\" x"#);
    }

    #[test]
    fn every_argument_splits_back_to_itself() {
        let args = [
            "",
            "plain",
            "two words",
            r#"say "hi""#,
            r"trailing\",
            r"trailing space\ ",
            r"\\server\share",
            r#"\""#,
            r#"\\""#,
            "\"",
            "tab\tand\nnewline",
            r"C:\Program Files\x\",
            "ünïcödé 😀",
            "a\u{0b}b",
        ];
        let program = r"C:\Program Files\tool\tool.exe";
        let (back_program, back_args) = split(&line(program, &args));
        assert_eq!(back_program, program);
        assert_eq!(back_args, args);
    }

    #[test]
    fn nul_quotes_in_the_program_empty_programs_and_long_lines_are_refused() {
        let program = units(r"C:\p.exe");
        assert_eq!(
            command_line(&program, &[vec![b'a' as u16, 0]]),
            Err(QuoteError::Nul)
        );
        assert_eq!(
            command_line(&units(r#"C:\a"b.exe"#), &[]),
            Err(QuoteError::ProgramQuote)
        );
        assert_eq!(command_line(&[], &[]), Err(QuoteError::EmptyProgram));
        let long = vec![b'x' as u16; COMMAND_LINE_MAX];
        assert_eq!(command_line(&program, &[long]), Err(QuoteError::TooLong));
        let fits = vec![b'x' as u16; COMMAND_LINE_MAX - program.len() - 3];
        assert_eq!(
            command_line(&program, &[fits]).unwrap().len(),
            COMMAND_LINE_MAX
        );
        assert_eq!(
            QuoteError::TooLong.to_string(),
            "the command line is longer than Windows allows"
        );
    }

    #[test]
    fn lone_surrogates_pass_through_unchanged() {
        let program = units(r"C:\p.exe");
        let arg = vec![0xd800, b' ' as u16, 0xdc00];
        let line = command_line(&program, std::slice::from_ref(&arg)).unwrap();
        let tail = &line[program.len() + 3..];
        assert_eq!(tail, [QUOTE, 0xd800, SPACE, 0xdc00, QUOTE]);
    }
}
