use super::*;

/// The word an interpreter is handed for a script's real file: on Windows the plain drive
/// spelling of its verbatim path, which Node needs to start a main script, kept only when it
/// names the same file ([`dunce::simplified`]); elsewhere the file as it is. Input: the real
/// file. Output: the word, none when only the verbatim `\\?\` form names the file.
fn script_word(program: &Path) -> Option<&Path> {
    let plain = dunce::simplified(program);
    let verbatim = plain.as_os_str().as_encoded_bytes().starts_with(br"\\?\");
    (!verbatim).then_some(plain)
}

impl Resolved {
    /// A script its interpreter runs, the one way a [`Kind::Script`] is made: its word is the
    /// spelling the interpreter is handed ([`script_word`]), and a script whose real file only
    /// a verbatim `\\?\` path names is refused, as Node cannot start it. Inputs: the entry,
    /// the script's real file, the interpreter, the first line's arguments and whether the
    /// interpreter is found by name. Output: the tool, or why it is refused.
    pub fn script(
        entry: PathBuf,
        program: PathBuf,
        interpreter: Resolved,
        arguments: Vec<String>,
        by_name: bool,
    ) -> Result<Self, Refusal> {
        let word = script_word(&program).ok_or(Refusal::Verbatim)?;
        let word = ScriptWord(word.as_os_str().to_owned());
        assert!(!word.0.is_empty());
        Ok(Self {
            entry,
            program,
            kind: Kind::Script {
                interpreter: Box::new(interpreter),
                arguments,
                by_name,
                word,
            },
        })
    }
}
