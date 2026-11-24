/// Tells whether a line is one of Claude Code's own limit lines, by the check Claude
/// Code 2.1.309 ships (`Bii`): one of its limit prefixes, or a model family, an optional
/// name of 1 to 40 characters without a middle dot or line break, then
/// ` requires usage credits.`. Takes the line. Returns true for a limit line.
pub(super) fn claude_limit_line(line: &str) -> bool {
    const PREFIXES: [&str; 12] = [
        "You've hit your",
        "You've reached your",
        "You're out of usage credits",
        "Your org is out of usage \u{b7} add funds to continue",
        "Your org is out of usage \u{b7} contact your admin",
        "Your seat type doesn't include usage credits",
        "Your seat type doesn't include usage",
        "Your usage allocation has been disabled by your admin",
        "Your group's usage limit is set to $0",
        "Fable 5 requires usage credits",
        "You're out of extra usage",
        "Your seat type doesn't include extra usage",
    ];
    const FAMILIES: [&str; 5] = ["Fable", "Haiku", "Mythos", "Opus", "Sonnet"];
    const CREDITS: &str = " requires usage credits.";
    PREFIXES.iter().any(|prefix| line.starts_with(prefix))
        || FAMILIES.iter().any(|family| {
            line.strip_prefix(family).is_some_and(|rest| {
                rest.starts_with(CREDITS)
                    || rest
                        .strip_prefix(' ')
                        .and_then(|tail| tail.find(CREDITS).map(|end| &tail[..end]))
                        .is_some_and(|name| {
                            (1..=40).contains(&name.chars().count())
                                && !name.contains(['\u{b7}', '\n'])
                        })
            })
        })
}
