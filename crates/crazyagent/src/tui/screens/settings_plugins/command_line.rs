use super::*;

/// Splits "command and arguments" as typed into a server spec.
/// Input: the typed line. Output: the spec, or the plain reason it cannot be used.
pub fn command_spec(line: &str) -> Result<ServerSpec, &'static str> {
    let mut words = words(line)?.into_iter();
    let command = words
        .next()
        .ok_or("Type the command that starts the server.")?;
    if !std::path::Path::new(&command).is_absolute() {
        return Err(COMMAND_HINT);
    }
    let spec = ServerSpec::Stdio {
        command,
        args: words.collect(),
    };
    spec.validate().map_err(|error| plugin_plain(&error.0))?;
    Ok(spec)
}

/// The server spec typed into a command or URL field.
/// Inputs: whether the field holds a URL, and the typed text. Output: the
/// spec, or the plain reason it cannot be used.
pub fn target_spec(url: bool, typed: &str) -> Result<ServerSpec, &'static str> {
    if !url {
        return command_spec(typed);
    }
    let spec = ServerSpec::Http {
        url: typed.trim().to_owned(),
    };
    spec.validate()
        .map(|()| spec)
        .map_err(|error| plugin_plain(&error.0))
}

/// Splits a typed command line into words. Single quotes keep spaces and a backslash keeps
/// the next character, so a server's `runs` line reads back as the same command. On Windows
/// a backslash separates folders, so it keeps only a following quote and is otherwise kept.
/// Input: the line. Output: the words, or the plain reason it cannot be read.
pub fn words(line: &str) -> Result<Vec<String>, &'static str> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut characters = line.chars();
    while let Some(character) = characters.next() {
        match character {
            '\'' => {
                let text = word.get_or_insert_with(String::new);
                loop {
                    match characters.next() {
                        Some('\'') => break,
                        Some(inner) => text.push(inner),
                        None => return Err("A quote is not closed."),
                    }
                }
            }
            '\\' if cfg!(windows) && !characters.as_str().starts_with('\'') => {
                word.get_or_insert_with(String::new).push('\\');
            }
            '\\' => {
                let kept = characters.next().ok_or("A backslash ends the line.")?;
                word.get_or_insert_with(String::new).push(kept);
            }
            space if space.is_whitespace() => words.extend(word.take()),
            other => word.get_or_insert_with(String::new).push(other),
        }
    }
    words.extend(word);
    assert!(words.len() <= line.len());
    Ok(words)
}
