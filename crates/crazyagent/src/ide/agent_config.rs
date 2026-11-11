/// The folder whose files the IDE reads as a workspace folder's settings, tasks and launch
/// configurations.
pub const FOLDER: &str = ".vscode";

/// The ending of a file the IDE opens as a multi-root workspace with its own settings.
pub const WORKSPACE_SUFFIX: &str = ".code-workspace";

/// Whether the IDE reads a repository path as its settings: anything under a [`FOLDER`] at
/// any depth, or a file whose name ends in [`WORKSPACE_SUFFIX`], each compared the way a
/// case-insensitive APFS volume compares names. Input: the repository path. Output: true
/// for IDE settings.
pub fn is_ide_config(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    path.split('/').any(|part| same_name(part, FOLDER)) || fold(name).ends_with(WORKSPACE_SUFFIX)
}

/// Whether a file name opens the same file as a lower-case ASCII name on a
/// case-insensitive APFS volume. Inputs: the name and the ASCII name. Output: true when
/// they are the same name there.
pub fn same_name(name: &str, ascii: &str) -> bool {
    assert!(ascii.is_ascii() && !ascii.bytes().any(|byte| byte.is_ascii_uppercase()));
    fold(name) == ascii
}

/// Folds a name for comparison with ASCII names the way a case-insensitive APFS volume
/// does after its canonical decomposition and case folding: ASCII letters become lower
/// case, U+017F (long s) becomes `s`, U+212A (Kelvin sign) becomes `k`, U+037E (Greek
/// question mark) becomes `;` and U+1FEF (Greek varia) becomes a backtick. Trying every
/// non-ASCII code point on a real volume found no other one that opens a file named with
/// one printable ASCII character, and none that APFS ignores. So every other character,
/// composed or decomposed, stays itself and never equals one ASCII character.
/// Input: the name. Output: the folded name, one character for each.
fn fold(name: &str) -> String {
    let folded: String = name
        .chars()
        .map(|character| match character {
            '\u{17f}' => 's',
            '\u{212a}' => 'k',
            '\u{37e}' => ';',
            '\u{1fef}' => '`',
            _ => character.to_ascii_lowercase(),
        })
        .collect();
    assert_eq!(folded.chars().count(), name.chars().count());
    folded
}

#[cfg(test)]
mod tests;
