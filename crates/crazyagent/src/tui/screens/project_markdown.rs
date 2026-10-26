use super::new_project_input::char_cells;

/// Names how one piece of a rendered report is styled.
/// Heading and Bold are bold, Code and Rule are muted, Link uses the accent.
/// Plain uses the body text color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum Role {
    Plain,
    Heading,
    Bold,
    Italic,
    Code,
    Link,
    Muted,
}

/// One display line of a rendered report: styled pieces that fit the width.
/// Each piece holds text without control characters and its role.
/// Adjacent pieces never share a role.
pub(in crate::tui) type MdLine = Vec<(String, Role)>;

/// Renders the Markdown subset of research reports into wrapped, styled lines.
/// Takes untrusted report text and the width in cells; control and terminal escape characters are removed.
/// Returns lines no wider than the width.
pub(in crate::tui) fn render(text: &str, width: usize) -> Vec<MdLine> {
    let width = width.max(8);
    let clean = sanitize(text);
    let mut lines: Vec<MdLine> = Vec::new();
    let mut fenced = false;
    for raw in clean.split('\n') {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            let pieces: Vec<(char, Role)> = raw.chars().map(|c| (c, Role::Code)).collect();
            lines.extend(wrap(&pieces, width, 0, false));
            continue;
        }
        if trimmed.is_empty() {
            lines.push(Vec::new());
            continue;
        }
        if is_rule(trimmed) {
            lines.push(vec![("\u{2500}".repeat(width), Role::Muted)]);
            continue;
        }
        if let Some(heading) = heading(trimmed) {
            let pieces = inline(heading, Role::Heading);
            lines.extend(wrap(&pieces, width, 0, false));
            continue;
        }
        let indent = raw.len() - trimmed.len();
        let nested = (indent / 2).min(4) * 2;
        let (prefix, body, role) = if let Some(rest) = bullet(trimmed) {
            (format!("{}- ", " ".repeat(nested)), rest, Role::Plain)
        } else if let Some((number, rest)) = ordered(trimmed) {
            (
                format!("{}{number}. ", " ".repeat(nested)),
                rest,
                Role::Plain,
            )
        } else if let Some(rest) = trimmed.strip_prefix('>') {
            ("\u{2502} ".to_owned(), rest.trim_start(), Role::Muted)
        } else {
            (String::new(), trimmed, Role::Plain)
        };
        let mut pieces: Vec<(char, Role)> = prefix
            .chars()
            .map(|c| {
                (
                    c,
                    if role == Role::Muted {
                        Role::Muted
                    } else {
                        Role::Plain
                    },
                )
            })
            .collect();
        let hang = prefix.chars().map(char_cells).sum::<usize>().min(width / 2);
        pieces.extend(inline(body, role));
        lines.extend(wrap(&pieces, width, hang, true));
    }
    while lines.last().is_some_and(Vec::is_empty) {
        lines.pop();
    }
    assert!(lines.iter().all(|line| line_width(line) <= width));
    lines
}

/// Measures a rendered line.
/// Takes the line.
/// Returns its width in cells.
pub(in crate::tui) fn line_width(line: &MdLine) -> usize {
    line.iter()
        .map(|(text, _)| text.chars().map(char_cells).sum::<usize>())
        .sum()
}

/// Gives the plain text of a rendered line.
/// Takes the line.
/// Returns the pieces joined without styles.
#[cfg(test)]
pub(in crate::tui) fn line_text(line: &MdLine) -> String {
    line.iter().map(|(text, _)| text.as_str()).collect()
}

/// Removes terminal escape sequences and control characters, and shows hidden, zero-width,
/// tag and direction-changing characters as visible escapes. Takes untrusted text.
/// Returns printable text with newlines kept and tabs expanded to spaces.
pub(in crate::tui) fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\u{1b}' => match characters.peek() {
                Some('[') => {
                    characters.next();
                    for next in characters.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') | Some('P') | Some('_') | Some('^') => {
                    characters.next();
                    while let Some(next) = characters.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' && characters.peek() == Some(&'\\') {
                            characters.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    characters.next();
                }
                None => {}
            },
            '\r' => {}
            '\n' => out.push('\n'),
            '\t' => out.push_str("    "),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    crate::memory::visible_text(&out)
}

fn is_rule(line: &str) -> bool {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.len() >= 3
        && (compact.chars().all(|c| c == '-')
            || compact.chars().all(|c| c == '*')
            || compact.chars().all(|c| c == '_'))
}

fn heading(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &line[hashes..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some(rest.trim().trim_end_matches('#').trim_end())
}

fn bullet(line: &str) -> Option<&str> {
    ["- ", "* ", "+ "]
        .iter()
        .find_map(|marker| line.strip_prefix(marker))
        .map(str::trim_start)
}

fn ordered(line: &str) -> Option<(&str, &str)> {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 4 {
        return None;
    }
    let rest = &line[digits..];
    let rest = rest
        .strip_prefix(". ")
        .or_else(|| rest.strip_prefix(") "))?;
    Some((&line[..digits], rest.trim_start()))
}

fn inline(text: &str, base: Role) -> Vec<(char, Role)> {
    let characters: Vec<char> = text.chars().collect();
    let mut out = Vec::with_capacity(characters.len());
    let mut index = 0;
    let mut bold = false;
    let mut italic = false;
    let emphasis = |bold: bool, italic: bool| match (base, bold, italic) {
        (Role::Plain, true, _) => Role::Bold,
        (Role::Plain, false, true) => Role::Italic,
        (role, _, _) => role,
    };
    while index < characters.len() {
        let character = characters[index];
        let rest = &characters[index..];
        if character == '`'
            && let Some(end) = rest[1..].iter().position(|c| *c == '`')
        {
            out.extend(rest[1..=end].iter().map(|c| (*c, Role::Code)));
            index += end + 2;
            continue;
        }
        if character == '['
            && let Some((label, url, used)) = link(rest)
        {
            let role = emphasis(bold, italic);
            out.extend(label.chars().map(|c| (c, role)));
            if !url.is_empty() && url != label {
                out.extend("  ".chars().map(|c| (c, role)));
                out.extend(url.chars().map(|c| (c, Role::Link)));
            }
            index += used;
            continue;
        }
        if starts_with(rest, "https://") || starts_with(rest, "http://") {
            let end = rest
                .iter()
                .position(|c| c.is_whitespace() || matches!(c, ')' | '>' | '"'))
                .unwrap_or(rest.len());
            out.extend(rest[..end].iter().map(|c| (*c, Role::Link)));
            index += end;
            continue;
        }
        if starts_with(rest, "**") || starts_with(rest, "__") {
            bold = !bold;
            index += 2;
            continue;
        }
        if (character == '*' || character == '_') && emphasis_marker(&characters, index) {
            italic = !italic;
            index += 1;
            continue;
        }
        out.push((character, emphasis(bold, italic)));
        index += 1;
    }
    out
}

fn emphasis_marker(characters: &[char], index: usize) -> bool {
    let before = index.checked_sub(1).map(|i| characters[i]);
    let after = characters.get(index + 1).copied();
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    !(word(before) && word(after)) && (after.is_some_and(|c| !c.is_whitespace()) || word(before))
}

fn starts_with(characters: &[char], prefix: &str) -> bool {
    prefix
        .chars()
        .enumerate()
        .all(|(index, expected)| characters.get(index) == Some(&expected))
}

fn link(characters: &[char]) -> Option<(String, String, usize)> {
    let close = characters.iter().position(|c| *c == ']')?;
    if characters.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = characters[close + 2..].iter().position(|c| *c == ')')? + close + 2;
    let label: String = characters[1..close].iter().collect();
    let url: String = characters[close + 2..end].iter().collect();
    let url = url.split_whitespace().next().unwrap_or("").to_owned();
    Some((label, url, end + 1))
}

fn wrap(pieces: &[(char, Role)], width: usize, hang: usize, words: bool) -> Vec<MdLine> {
    let prefix = pieces
        .iter()
        .scan(0, |used, (c, _)| {
            *used += char_cells(*c);
            Some(*used)
        })
        .take_while(|used| *used <= hang)
        .count();
    let mut lines: Vec<MdLine> = Vec::new();
    let mut start = 0;
    loop {
        let first = lines.is_empty();
        let room = if first { width } else { width - hang };
        let mut used = 0;
        let mut end = start;
        let mut space = None;
        while end < pieces.len() {
            let size = char_cells(pieces[end].0);
            if used + size > room {
                break;
            }
            if pieces[end].0 == ' ' && end >= prefix {
                space = Some(end);
            }
            used += size;
            end += 1;
        }
        let (cut, next) = if end >= pieces.len() {
            (end, end)
        } else if let Some(space) = space.filter(|space| words && *space > start) {
            (space, space + 1)
        } else if end == start {
            (start + 1, start + 1)
        } else {
            (end, end)
        };
        let mut line: MdLine = Vec::new();
        if !first && hang > 0 {
            line.push((" ".repeat(hang), Role::Plain));
        }
        for (character, role) in &pieces[start..cut] {
            match line.last_mut() {
                Some((text, last)) if last == role => text.push(*character),
                _ => line.push((character.to_string(), *role)),
            }
        }
        lines.push(line);
        start = next;
        while words && start < pieces.len() && pieces[start].0 == ' ' {
            start += 1;
        }
        if start >= pieces.len() {
            break;
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[MdLine]) -> Vec<String> {
        lines.iter().map(line_text).collect()
    }

    #[test]
    fn headings_lists_links_and_code_render_as_the_design_shows() {
        let report = "# Market research: Tidepool\n\nOcean labs and port authorities watch buoy data through vendor portals that refresh slowly and alert poorly. Most want one live view across vendors.\n\n## Who pays\n- University ocean labs, mostly grant funded\n* Port and harbor authorities\n\n## Sources\n1. [NOAA National Data Buoy Center](https://www.ndbc.noaa.gov)\n2. See https://coastwatch.pfeg.noaa.gov/erddap for **live** data and `erddap` exports.\n\n---\n";
        let lines = render(report, 76);
        assert_eq!(
            texts(&lines),
            [
                "Market research: Tidepool",
                "",
                "Ocean labs and port authorities watch buoy data through vendor portals that",
                "refresh slowly and alert poorly. Most want one live view across vendors.",
                "",
                "Who pays",
                "- University ocean labs, mostly grant funded",
                "- Port and harbor authorities",
                "",
                "Sources",
                "1. NOAA National Data Buoy Center  https://www.ndbc.noaa.gov",
                "2. See https://coastwatch.pfeg.noaa.gov/erddap for live data and erddap",
                "   exports.",
                "",
                &"\u{2500}".repeat(76),
            ]
        );
        assert_eq!(
            lines[0],
            [("Market research: Tidepool".into(), Role::Heading)]
        );
        assert_eq!(
            lines[10],
            [
                ("1. NOAA National Data Buoy Center  ".into(), Role::Plain),
                ("https://www.ndbc.noaa.gov".into(), Role::Link)
            ]
        );
        assert!(lines[11].contains(&("live".into(), Role::Bold)));
        assert!(lines[11].contains(&("erddap".into(), Role::Code)));
        assert!(
            lines[11].contains(&("https://coastwatch.pfeg.noaa.gov/erddap".into(), Role::Link))
        );
    }

    #[test]
    fn terminal_escapes_and_control_characters_never_reach_the_screen() {
        let hostile = "\u{1b}[2J\u{1b}[31mRed\u{1b}[0m text\u{7}\u{0}\u{8}\r\n\u{1b}]52;c;cGF5bG9hZA==\u{7}clip\u{1b}]8;;https://x\u{1b}\\link\u{9b}31m\u{202e}evil\ttab";
        let lines = render(hostile, 40);
        assert_eq!(texts(&lines), ["Red text", "cliplink31m\\u202eevil    tab"]);
        for line in &lines {
            for (text, _) in line {
                assert!(text.chars().all(|c| !c.is_control()), "{text:?}");
            }
        }
        assert_eq!(sanitize("a\u{1b}"), "a");
        assert_eq!(sanitize("\u{1b}Xb"), "b");
    }

    #[test]
    fn hidden_and_reordering_characters_show_as_escapes() {
        assert_eq!(
            sanitize("Plain\u{200b} text\u{e0041}\u{e0042} and \u{202e}evil\u{2028}end"),
            "Plain\\u200b text\\u{e0041}\\u{e0042} and \\u202eevil\\u2028end"
        );
        assert_eq!(
            sanitize("Thumbs \u{1f44d}\u{fe0f} up"),
            "Thumbs \u{1f44d}\u{fe0f} up"
        );
        assert_eq!(
            sanitize("\u{1f469}\u{200d}\u{1f4bb} codes"),
            "\u{1f469}\u{200d}\u{1f4bb} codes"
        );
        assert_eq!(
            super::super::new_project_view::printable("tag\u{e0067}\nnext\u{feff}".into()),
            "tag\\u{e0067} next\\ufeff"
        );
        assert_eq!(
            texts(&render("Saved \u{2060}text", 40)),
            ["Saved \\u2060text"]
        );
    }

    #[test]
    fn very_long_lines_and_words_wrap_inside_the_width() {
        let word = "x".repeat(200);
        let lines = render(&format!("- {word} tail"), 40);
        assert!(lines.iter().all(|line| line_width(line) <= 40));
        assert_eq!(
            texts(&lines).concat().replace([' ', '-'], ""),
            format!("{word}tail")
        );
        assert!(texts(&lines)[1].starts_with("  x"));
        let long = "word ".repeat(5000);
        let lines = render(&long, 76);
        assert!(lines.len() > 300);
        assert!(lines.iter().all(|line| line_width(line) <= 76));
        let wide = "\u{6f22}".repeat(50);
        let lines = render(&wide, 9);
        assert!(lines.iter().all(|line| line_width(line) <= 9));
        assert_eq!(lines.iter().map(line_width).sum::<usize>(), 100);
        assert_eq!(render("", 40), Vec::<MdLine>::new());
        assert_eq!(texts(&render("a", 1)), ["a"]);
    }

    #[test]
    fn fenced_code_quotes_and_nested_lists_keep_their_shape() {
        let lines = render(
            "Intro\n```\nfn main() {}\n```\n> quoted words\n- top\n  - nested\n3) third\n#not heading\n####### seven",
            40,
        );
        assert_eq!(
            texts(&lines),
            [
                "Intro",
                "fn main() {}",
                "\u{2502} quoted words",
                "- top",
                "  - nested",
                "3. third",
                "#not heading",
                "####### seven"
            ]
        );
        assert_eq!(lines[1], [("fn main() {}".into(), Role::Code)]);
        assert_eq!(lines[2], [("\u{2502} quoted words".into(), Role::Muted)]);
        assert_eq!(
            texts(&render("snake_case_name and *em* here", 40)),
            ["snake_case_name and em here"]
        );
        assert!(render("*em*", 40)[0].contains(&("em".into(), Role::Italic)));
    }
}
