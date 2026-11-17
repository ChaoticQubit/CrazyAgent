use super::*;

fn nothing(_: &str) -> Option<bool> {
    None
}

fn one(note: &str, lines: &[u32]) -> (String, Vec<u32>) {
    (note.to_owned(), lines.to_vec())
}

fn noted(found: Vec<Finding>) -> Vec<(String, Vec<u32>)> {
    let mut pairs = Vec::new();
    for finding in found {
        assert_eq!(finding.kind, FindingKind::BrokenReference);
        pairs.push((finding.note, finding.lines));
    }
    pairs
}

#[test]
fn a_clean_note_shows_no_hidden_character_and_fits_its_budget() {
    assert_eq!(clean_note("  Two\tspaced\n\nwords. "), "Two spaced words.");
    assert_eq!(clean_note("a\u{202e}b\u{200b}c\u{1b}d\u{feff}"), "abcd");
    assert_eq!(clean_note(" \u{2066}\u{2069} "), "");
    let long = "é".repeat(NOTE_BYTES_MAX);
    assert_eq!(clean_note(&long).len(), NOTE_BYTES_MAX);
    let odd = clean_note(&format!("a{long}"));
    assert_eq!(odd.len(), NOTE_BYTES_MAX - 1, "no split character");
}

#[test]
fn a_findings_lines_read_as_the_whole_file_one_line_or_several() {
    assert_eq!(lines_label(&[]), "whole file");
    assert_eq!(lines_label(&[7]), "line 7");
    assert_eq!(lines_label(&[2, 9, 14]), "lines 2, 9, 14");
}

#[test]
fn each_kind_has_one_name_on_the_wire_and_in_the_prompt() {
    let kinds = [
        FindingKind::OverBudget,
        FindingKind::Duplicate,
        FindingKind::BrokenReference,
        FindingKind::Contradiction,
    ];
    for kind in kinds {
        assert_eq!(serde_json::to_value(kind).unwrap(), kind.name());
    }
    let extra = r#"{"kind":"duplicate","lines":[1],"note":"x","extra":1}"#;
    assert!(serde_json::from_str::<Finding>(extra).is_err());
}

#[test]
fn repeats_names_lines_said_twice_in_one_place_and_app_lines_only_when_given_them() {
    let text = "# One\n- A.\n- B.\n- A.\n# Two\n- A.\n";
    let note = "Line 4 repeats line 2.";
    let within = Finding::new(FindingKind::Duplicate, vec![2, 4], note);
    assert_eq!(repeats(text, None), std::slice::from_ref(&within));
    let had = "The app instructions already have this line.";
    let in_app = Finding::new(FindingKind::Duplicate, vec![3], had);
    assert_eq!(
        repeats(text, Some("# One\n- B.\n")),
        [in_app, within.clone()],
        "in line order"
    );
    assert_eq!(repeats(text, Some("")), [within]);
}

#[test]
fn a_file_reference_counts_only_when_shaped_as_a_file_the_project_lacks() {
    let directory = tempfile::tempdir().unwrap();
    let folder = directory.path().canonicalize().unwrap();
    std::fs::create_dir_all(folder.join("docs")).unwrap();
    std::fs::create_dir_all(folder.join("src")).unwrap();
    std::fs::write(folder.join("docs/kept.md"), "kept").unwrap();
    let text = "See `docs/kept.md`, `docs/kept.md:12:3` and `./docs/kept.md`.\n\
                Read `docs/missing.md` first.\n\
                Branch `feat/x`, `type/short-slug` or `https://example.com/a.md`.\n\
                Not `../outside.md`, `/etc/hosts.md`, `~/notes.md` or `src/*.rs`.\n\
                New `lib/new.rs` here, in `src/`.\n\
                ```\n\
                `docs/inside-a-block.md`\n\
                ```\n\
                Again `docs/missing.md`.\n";
    let known = Known {
        folder: Some(folder.as_path()),
        skills: None,
        servers: None,
        program: &nothing,
    };
    let missing = "`docs/missing.md` is not a file or folder in this project.";
    let new = "`lib/new.rs` is not a file or folder in this project.";
    let found = noted(broken(text, &known));
    assert_eq!(found, [one(missing, &[2, 9]), one(new, &[5])]);
    let app = Known {
        folder: None,
        ..known
    };
    assert!(broken(text, &app).is_empty(), "no files for the app");
}

/// DEC-172 r2 F1: a path through a link inside the project is never judged, so whether the
/// link's outside target is there never decides a finding and nothing outside is read, while
/// a missing plain file beside the links still counts.
#[cfg(unix)]
#[test]
fn a_file_reference_through_a_link_is_never_judged_whatever_lies_outside() {
    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let folder = directory.path().canonicalize().unwrap();
    std::fs::create_dir_all(folder.join("docs")).unwrap();
    let kept = outside.path().join("kept.md");
    let gone = outside.path().join("gone.md");
    std::fs::write(&kept, "kept").unwrap();
    std::os::unix::fs::symlink(&kept, folder.join("docs/kept.md")).unwrap();
    std::os::unix::fs::symlink(&gone, folder.join("docs/gone.md")).unwrap();
    std::os::unix::fs::symlink(outside.path(), folder.join("away")).unwrap();
    let text = "Read `docs/kept.md`, `docs/gone.md`, `away/kept.md` and `away/gone.md`.\n\
                Then `docs/plain.md`.\n";
    let known = Known {
        folder: Some(folder.as_path()),
        skills: None,
        servers: None,
        program: &nothing,
    };
    let plain = "`docs/plain.md` is not a file or folder in this project.";
    assert_eq!(noted(broken(text, &known)), [one(plain, &[2])]);
}

#[test]
fn a_skill_reference_counts_only_on_a_skill_line_and_only_for_a_skill_not_given() {
    let skills = vec!["grill-me".to_owned(), "brainstorming".to_owned()];
    let text = "Use the `grill-me` skill, then the `superpowers:brainstorming` skill.\n\
                Run the `ghost-skill` skill.\n\
                Plain `ghost` here.\n\
                Skills: `SKILL.md`, `Ghost` and `a:b:c`.\n\
                The `ghost-skill` skill again.\n";
    let known = Known {
        folder: None,
        skills: Some(skills.as_slice()),
        servers: None,
        program: &nothing,
    };
    let ghost = "`ghost-skill` is not a skill these agents get.";
    assert_eq!(noted(broken(text, &known)), [one(ghost, &[2, 5])]);
    let unread = Known {
        skills: None,
        ..known
    };
    assert!(broken(text, &unread).is_empty(), "no list, no guess");
}

#[test]
fn a_tool_reference_counts_when_its_mcp_server_is_not_given() {
    let servers = vec!["docs".to_owned(), "crazyagent-team".to_owned()];
    let text = "Call `mcp__docs__search`, then `mcp__ghost__search` and `mcp__ghost__search`.\n\
                Start with `mcp__crazyagent-team__start_work`.\n\
                Not `mcp__Bad__x` or `mcp__`.\n";
    let known = Known {
        folder: None,
        skills: None,
        servers: Some(servers.as_slice()),
        program: &nothing,
    };
    let ghost = "`mcp__ghost__search` names the MCP server ghost, which these agents do not get.";
    assert_eq!(noted(broken(text, &known)), [one(ghost, &[1])]);
    let unread = Known {
        servers: None,
        ..known
    };
    assert!(broken(text, &unread).is_empty());
}

#[test]
fn a_command_counts_only_when_this_computer_is_known_not_to_have_its_program() {
    let program = |name: &str| match name {
        "sh" => Some(true),
        "gone-tool" => Some(false),
        _ => None,
    };
    let text = "Run `sh -c true` and `gone-tool --flag`.\n\
                Then `cd src`, `echo hi`, `unknown-tool x` and `Make all`.\n\
                Also `gone-tool -v`.\n\
                One word `gone-tool` is no command line.\n";
    let known = Known {
        folder: None,
        skills: None,
        servers: None,
        program: &program,
    };
    let gone = "`gone-tool` is not a command on this computer.";
    assert_eq!(noted(broken(text, &known)), [one(gone, &[1, 3])]);
    let pending = Known {
        program: &nothing,
        ..known
    };
    assert!(broken(text, &pending).is_empty(), "not known yet");
}

#[test]
fn spans_inside_code_blocks_and_over_long_lines_are_never_read() {
    let servers: Vec<String> = Vec::new();
    let known = Known {
        folder: None,
        skills: None,
        servers: Some(servers.as_slice()),
        program: &nothing,
    };
    let text = "~~~\n`mcp__ghost__x`\n~~~\n    ```\n`mcp__ghost__y`\n```rust\n`mcp__ghost__z`\n";
    let ghost = "`mcp__ghost__y` names the MCP server ghost, which these agents do not get.";
    assert_eq!(noted(broken(text, &known)), [one(ghost, &[5])]);
    let long = format!("`mcp__ghost__x` {}\n", "a".repeat(LINE_BYTES_MAX));
    assert!(broken(&long, &known).is_empty());
}

#[test]
fn one_missing_name_is_one_finding_and_a_text_judges_at_most_its_reference_budget() {
    let servers: Vec<String> = Vec::new();
    let known = Known {
        folder: None,
        skills: None,
        servers: Some(servers.as_slice()),
        program: &nothing,
    };
    let again = "Use `mcp__ghost__x` and `mcp__ghost__x`.\n".repeat(LINES_PER_FINDING_MAX + 4);
    let found = broken(&again, &known);
    let most = u32::try_from(LINES_PER_FINDING_MAX).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].lines, (1..=most).collect::<Vec<u32>>());
    let many: String = (0..REFERENCES_MAX + 6)
        .map(|at| format!("`mcp__ghost{at}__x`\n"))
        .collect();
    assert_eq!(broken(&many, &known).len(), REFERENCES_MAX);
    let filler = "`Plain`\n".repeat(REFERENCES_MAX + 6);
    let plain = format!("{filler}`mcp__ghost__x`\n");
    assert_eq!(
        broken(&plain, &known).len(),
        1,
        "a span that names nothing costs nothing"
    );
}
