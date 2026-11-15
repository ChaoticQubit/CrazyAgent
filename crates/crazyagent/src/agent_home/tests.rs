use super::*;

fn folder() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    (directory, root)
}

fn project(root: &Path) -> AgentProject {
    let data = root.join("data");
    let folder = root.join("project");
    fs::create_dir_all(&data).unwrap();
    fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    AgentProject {
        data,
        id: ProjectId::new("process").unwrap(),
        folder: Some(folder),
        mode: crate::instruction_files::Mode::Both,
        repeated: crate::agent_home::Repeated::LeaveOut,
    }
}

#[cfg(unix)]
fn given(text: &str) -> Given {
    Given {
        text: text.into(),
        files: Vec::new(),
        section_bytes: 0,
    }
}

#[test]
fn both_texts_are_saved_and_read_back_and_a_missing_file_is_empty() {
    let (_directory, root) = folder();
    let project = project(&root);
    let folder = project.folder.clone().unwrap();
    assert_eq!(read_app(&project.data).unwrap(), "");
    assert_eq!(read_project(&folder).unwrap(), "");
    assert_eq!(text(&project).unwrap().text, "");

    save_app(&project.data, "Use TDD.\n").unwrap();
    save_project(&folder, "Run make check.\n").unwrap();

    assert_eq!(read_app(&project.data).unwrap(), "Use TDD.\n");
    assert_eq!(read_project(&folder).unwrap(), "Run make check.\n");
    assert_eq!(
        fs::read_to_string(folder.join(".crazyagent/instructions.md")).unwrap(),
        "Run make check.\n"
    );
    assert_eq!(
        fs::read_to_string(project.data.join("instructions.md")).unwrap(),
        "Use TDD.\n"
    );
    save_app(&project.data, "").unwrap();
    assert_eq!(read_app(&project.data).unwrap(), "");
    let left: Vec<_> = fs::read_dir(&project.data)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(
        left,
        ["instructions.md"],
        "no temporary file is left behind"
    );
}

/// SA vid 11 r2 (rv-sec3 F1): a project instructions save writes its temporary file in the
/// state folder's `context/`, made when missing, and leaves only that folder, empty, and the
/// instructions file in the state folder; a `context` that is a link is refused with
/// nothing written through it and the saved text kept.
#[cfg(unix)]
#[test]
fn a_project_save_stages_in_the_context_folder_and_refuses_a_linked_one() {
    let (_directory, root) = folder();
    let project = project(&root);
    let folder = project.folder.clone().unwrap();
    let state = folder.join(".crazyagent");
    save_project(&folder, "Owner rule.\n").unwrap();
    let mut left: Vec<_> = fs::read_dir(&state)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    left.sort();
    assert_eq!(left, ["context", "instructions.md"]);
    assert_eq!(
        fs::read_dir(state.join("context")).unwrap().count(),
        0,
        "no temporary file is left behind"
    );
    assert_eq!(read_project(&folder).unwrap(), "Owner rule.\n");

    fs::remove_dir(state.join("context")).unwrap();
    let elsewhere = root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, state.join("context")).unwrap();
    assert_eq!(
        save_project(&folder, "Planted.\n").unwrap_err().0,
        "process_text_unwritable"
    );
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    assert_eq!(read_project(&folder).unwrap(), "Owner rule.\n");
}

#[test]
fn a_project_without_its_state_folder_is_refused_and_reads_empty() {
    let (_directory, root) = folder();
    let legacy = root.join("legacy");
    fs::create_dir_all(&legacy).unwrap();
    assert_eq!(read_project(&legacy).unwrap(), "");
    assert_eq!(
        save_project(&legacy, "x").unwrap_err().0,
        "process_project_folder_missing"
    );
    assert!(!legacy.join(".crazyagent").exists());
}

#[test]
fn the_size_bound_holds_on_save_and_on_read() {
    let (_directory, root) = folder();
    let project = project(&root);
    let folder = project.folder.clone().unwrap();
    let full = "a".repeat(TEXT_BYTES_MAX);
    save_app(&project.data, &full).unwrap();
    save_project(&folder, &full).unwrap();
    assert_eq!(read_app(&project.data).unwrap().len(), TEXT_BYTES_MAX);
    assert_eq!(
        text(&project).unwrap_err().0,
        "agent_instructions_too_large_app",
        "two full parts are more than one launch is given, and the refusal names the larger"
    );

    let over = "a".repeat(TEXT_BYTES_MAX + 1);
    assert_eq!(
        save_app(&project.data, &over).unwrap_err().0,
        "process_text_too_large"
    );
    assert_eq!(
        save_project(&folder, &over).unwrap_err().0,
        "process_text_too_large"
    );
    assert_eq!(
        read_app(&project.data).unwrap(),
        full,
        "a refused save keeps the old text"
    );

    fs::write(folder.join(".crazyagent/instructions.md"), &over).unwrap();
    assert_eq!(
        read_project(&folder).unwrap_err().0,
        "process_text_too_large"
    );
    assert_eq!(text(&project).unwrap_err().0, "process_text_too_large");
}

#[test]
fn text_that_is_not_plain_is_refused() {
    let (_directory, root) = folder();
    let project = project(&root);
    assert_eq!(
        save_app(&project.data, "a\0b").unwrap_err().0,
        "process_text_invalid"
    );
    fs::write(project.data.join("instructions.md"), [0xff, 0xfe]).unwrap();
    assert_eq!(
        read_app(&project.data).unwrap_err().0,
        "process_text_invalid"
    );
}

#[cfg(unix)]
#[test]
fn a_linked_instruction_file_or_state_folder_is_never_read() {
    let (_directory, root) = folder();
    let project = project(&root);
    let folder = project.folder.clone().unwrap();
    let secret = root.join("secret");
    fs::write(&secret, "token").unwrap();
    std::os::unix::fs::symlink(&secret, folder.join(".crazyagent/instructions.md")).unwrap();
    assert_eq!(read_project(&folder).unwrap_err().0, "process_text_unsafe");

    let cloned = root.join("cloned");
    fs::create_dir_all(&cloned).unwrap();
    let elsewhere = root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("instructions.md"), "token").unwrap();
    std::os::unix::fs::symlink(&elsewhere, cloned.join(".crazyagent")).unwrap();
    assert_eq!(read_project(&cloned).unwrap_err().0, "process_text_unsafe");
    assert_eq!(
        save_project(&cloned, "x").unwrap_err().0,
        "process_project_folder_missing"
    );
    assert_eq!(
        fs::read_to_string(elsewhere.join("instructions.md")).unwrap(),
        "token"
    );
}

#[test]
fn render_puts_app_text_first_and_project_text_last_under_headings() {
    assert_eq!(render("", " \n", Repeated::LeaveOut), "");
    assert_eq!(
        render("Use TDD.\n", "Run make check.\n", Repeated::LeaveOut),
        "# Instructions from CrazyAgent\n\n## App instructions\n\nUse TDD.\n\n## Project instructions\n\nRun make check.\n"
    );
    assert_eq!(
        render("", "Only project.\n", Repeated::LeaveOut),
        "# Instructions from CrazyAgent\n\n## Project instructions\n\nOnly project.\n"
    );
}

#[test]
fn render_leaves_out_project_lines_the_app_text_already_has() {
    let app = "## Rules\n\n- Use TDD.\n- No em dash.\n";
    let project = "## Rules\n\n- Use TDD.\n- Run make check.\n";
    assert_eq!(
        render(app, project, Repeated::LeaveOut),
        "# Instructions from CrazyAgent\n\n## App instructions\n\n## Rules\n\n- Use TDD.\n- No em dash.\n\n## Project instructions\n\n## Rules\n\n- Run make check.\n"
    );
    assert_eq!(repeated_lines(app, project), 1);
}

#[test]
fn render_keeps_headings_fences_and_rules_even_when_repeated() {
    let app = "## Build\n\n```\nmake\n```\n\n---\n";
    let project = "## Build\n\n```\nmake test\n```\n\n---\n";
    let joined = render(app, project, Repeated::LeaveOut);
    assert!(
        joined.ends_with("## Project instructions\n\n## Build\n\n```\nmake test\n```\n\n---\n")
    );
    assert_eq!(repeated_lines(app, project), 0);
}

#[test]
fn render_keeps_every_line_when_the_setting_says_keep() {
    let joined = render("- Use TDD.\n", "- Use TDD.\n", Repeated::Keep);
    assert_eq!(joined.matches("- Use TDD.").count(), 2);
}

#[test]
fn a_project_text_made_only_of_repeats_adds_no_project_heading() {
    let joined = render("- Use TDD.\n", "- Use TDD.\n\n", Repeated::LeaveOut);
    assert!(!joined.contains("## Project instructions"));
    let headed = render(
        "## Rules\n\n- Use TDD.\n",
        "## Rules\n\n- Use TDD.\n\n---\n",
        Repeated::LeaveOut,
    );
    assert!(!headed.contains("## Project instructions"), "{headed}");
    assert_eq!(headed.matches("- Use TDD.").count(), 1);
}

#[test]
fn render_keeps_every_line_of_a_code_block_and_every_table_row() {
    let app = "## Tools\n\n- Use TDD.\n- Run it.\n\n```\nmake\n\n\nmake test\n```\n\n| Tool | Use |\n|---|---|\n| make | build |\n";
    let project = "## Tools\n\n- Run it.\n- Use TDD.\n- Own rule.\n\n\n\n~~~sh\nmake\n\n\nmake test\n~~~\n\n| Tool | Use |\n|---|---|\n| make | build |\n";
    let joined = render(app, project, Repeated::LeaveOut);
    assert!(
        joined.ends_with(
            "## Project instructions\n\n## Tools\n\n- Own rule.\n\n\n\n~~~sh\nmake\n\n\nmake test\n~~~\n\n| Tool | Use |\n|---|---|\n| make | build |\n"
        ),
        "{joined}"
    );
    assert_eq!(repeated_lines(app, project), 2);
}

#[test]
fn a_project_text_with_no_repeat_is_given_byte_for_byte() {
    let project = "Run make check.\n\n\n\n  Indented stays.\n";
    assert_eq!(
        render("Use TDD.\n", project, Repeated::LeaveOut),
        render("Use TDD.\n", project, Repeated::Keep)
    );
    assert_eq!(
        render("", "- Use TDD.\n", Repeated::LeaveOut),
        "# Instructions from CrazyAgent\n\n## Project instructions\n\n- Use TDD.\n",
        "a blank app text has no line to repeat"
    );
    assert_eq!(repeated_lines("", "- Use TDD.\n"), 0);
    assert_eq!(
        repeated_lines("  - Use TDD.  \n", "  - Use TDD.\n"),
        1,
        "spaces at a line's end do not count"
    );
    for project in ["- Use TDD.\n", " - Use TDD.\n", "\t- Use TDD.\n"] {
        assert_eq!(
            repeated_lines("  - Use TDD.  \n", project),
            0,
            "a line repeats only at the same indent: {project:?}"
        );
    }
}

#[test]
fn the_same_rule_under_another_heading_is_never_left_out() {
    let app = "## Always\n\n- Use npm.\n";
    let project = "## Never\n\n- Use npm.\n";
    assert_eq!(
        render(app, project, Repeated::LeaveOut),
        render(app, project, Repeated::Keep),
        "leaving the line out would turn Never use npm into Always use npm"
    );
    assert_eq!(repeated_lines(app, project), 0);

    let nested_app = "## Always\n\n### Tools\n\n- Use npm.\n";
    let nested = "## Never\n\n### Tools\n\n- Use npm.\n";
    assert_eq!(
        repeated_lines(nested_app, nested),
        0,
        "the same inner heading under another outer heading is another section"
    );
    let underlined_app = "Always\n======\n\n- Use npm.\n";
    let underlined = "Never\n=====\n\n- Use npm.\n";
    assert_eq!(
        render(underlined_app, underlined, Repeated::LeaveOut),
        render(underlined_app, underlined, Repeated::Keep),
        "an underlined heading names a section as a # heading does"
    );
    assert_eq!(
        repeated_lines("- Use npm.\n", "## Never\n\n- Use npm.\n"),
        0,
        "a line before any heading is not the line under a heading"
    );
    assert_eq!(
        repeated_lines(
            "## Rules\n\n- Use npm.\n",
            "## Rules\n\n- Use npm.\n- Run make.\n"
        ),
        1,
        "under the same heading the repeated line is still left out"
    );
}

#[test]
fn a_line_at_another_indent_is_another_line() {
    let app = "- Use npm.\n";
    let project = "- Tools we never use:\n  - Use npm.\n";
    assert_eq!(
        render(app, project, Repeated::LeaveOut),
        render(app, project, Repeated::Keep),
        "a sub-bullet belongs to its parent line"
    );
    assert_eq!(repeated_lines(app, project), 0);
    assert_eq!(repeated_lines("\t- Use npm.\n", "- Use npm.\n"), 0);
    assert_eq!(
        repeated_lines("- Use npm.  \n", "- Use npm.\n- Use npm.\t\n"),
        2,
        "only spaces at a line's end are ignored"
    );
}

#[test]
fn a_code_block_ends_only_on_its_own_fence() {
    for text in [
        "~~~\n```\n- Use TDD.\n```\n~~~\n",
        "````\n```\n- Use TDD.\n```\n````\n",
        "```\n    ```\n- Use TDD.\n```\n",
        "```text\n~~~\n- Use TDD.\n",
        "~~~\n~~\n- Use TDD.\n~~~~\n",
    ] {
        for app in ["- Use TDD.\n", text] {
            assert_eq!(
                repeated_lines(app, text),
                0,
                "a line inside the block stays: {text:?} beside {app:?}"
            );
            assert_eq!(
                render(app, text, Repeated::LeaveOut),
                render(app, text, Repeated::Keep),
                "{text:?} beside {app:?}"
            );
        }
    }
    assert_eq!(
        repeated_lines("    ```\n- Use TDD.\n", "    ```\n- Use TDD.\n"),
        1,
        "a line indented by four spaces opens no block, so the repeat after it is left out"
    );
    assert_eq!(
        repeated_lines(
            "```\ncode\n```\n- Use TDD.\n",
            "```\ncode\n```\n- Use TDD.\n"
        ),
        1,
        "after the block closes a repeat is left out again"
    );
    assert_eq!(
        repeated_lines(
            "Run ```make``` first.\n- Use TDD.\n",
            "Run ```make``` first.\n- Use TDD.\n- Own rule.\n"
        ),
        1,
        "backticks inside a sentence open no block"
    );
}

#[test]
fn a_heading_goes_only_when_every_line_under_it_was_left_out() {
    let app = "## Rules\n\n- Use TDD.\n\n## Build\n\n- make\n";
    let project = "## Rules\n\n- Use TDD.\n\n## Build\n\n- make\n- make test\n\n## Notes\n";
    let joined = render(app, project, Repeated::LeaveOut);
    assert!(
        joined.ends_with("## Project instructions\n\n## Build\n\n- make test\n\n## Notes\n"),
        "{joined}"
    );
    assert_eq!(repeated_lines(app, project), 2);
    let underlined = render(
        "Rules\n-----\n\n- Use TDD.\n",
        "Rules\n-----\n\n- Use TDD.\n\n## Build\n\n- make\n",
        Repeated::LeaveOut,
    );
    assert!(
        underlined.ends_with("## Project instructions\n\n## Build\n\n- make\n"),
        "an underlined heading goes with its underline: {underlined}"
    );
}

#[test]
fn the_same_rule_under_another_lead_in_or_parent_item_is_never_left_out() {
    for (app, project) in [
        ("Always:\n- Use npm.\n", "Never:\n- Use npm.\n"),
        (
            "Always do these:\n\n- Use npm.\n",
            "Never do these:\n\n- Use npm.\n",
        ),
        ("- Always:\n  - Use npm.\n", "- Never:\n  - Use npm.\n"),
        (
            "- Always:\n - Tools:\n  - Use npm.\n",
            "- Never:\n - Tools:\n  - Use npm.\n",
        ),
        ("1. Always:\n   - Use npm.\n", "1. Never:\n   - Use npm.\n"),
        (
            "Always run the tests\nin CI.\n",
            "Never run the tests\nin CI.\n",
        ),
        ("- Use npm.\n", "Never:\n- Use npm.\n"),
    ] {
        assert_eq!(repeated_lines(app, project), 0, "{project:?}");
        assert_eq!(
            render(app, project, Repeated::LeaveOut),
            render(app, project, Repeated::Keep),
            "leaving the line out would drop the project's own rule: {project:?}"
        );
    }
    let joined = render(
        "Rules:\n- Use npm.\n- Use TDD.\n",
        "Rules:\n- Use TDD.\n- Run make.\n- Use npm.\n",
        Repeated::LeaveOut,
    );
    assert!(
        joined.ends_with("## Project instructions\n\nRules:\n- Run make.\n"),
        "the items of one list repeat in any order under the same lead-in: {joined}"
    );
}

#[test]
fn a_line_stays_when_a_kept_line_hangs_on_it() {
    let joined = render(
        "Never use:\n- npm\n",
        "Never use:\n- npm\n- yarn\n",
        Repeated::LeaveOut,
    );
    assert!(
        joined.ends_with("## Project instructions\n\nNever use:\n- yarn\n"),
        "the kept item keeps its lead-in: {joined}"
    );
    for (app, project) in [
        ("- Use npm.\n", "- Use npm.\n  Only in CI.\n"),
        ("- Use npm.\n", "- Use npm.\n  - Only in CI.\n"),
        ("Run this:\n", "Run this:\n\n    make test\n"),
        ("Run this:\n", "Run this:\n\n```\nmake test\n```\n"),
        ("Tools:\n", "Tools:\n\n| Tool | Use |\n"),
        ("Use tabs.\n", "Use tabs.\n\nBut never in tables.\n"),
    ] {
        assert_eq!(repeated_lines(app, project), 0, "{project:?}");
        assert_eq!(
            render(app, project, Repeated::LeaveOut),
            render(app, project, Repeated::Keep),
            "{project:?}"
        );
    }
    let app = "## Never use\n\n- npm\n";
    let project = "## Never use\n\n- npm\n\n### Except\n\n- in CI\n";
    let nested = render(app, project, Repeated::LeaveOut);
    assert!(
        nested.ends_with("## Project instructions\n\n## Never use\n\n### Except\n\n- in CI\n"),
        "a heading stays while a heading under it keeps a line: {nested}"
    );
    assert_eq!(repeated_lines(app, project), 1);
    let emptied = render(
        "## Tools\n\n### Build\n\n- make\n",
        "## Tools\n\n### Build\n\n- make\n\n### Notes\n\n## Own\n\n- Own rule.\n",
        Repeated::LeaveOut,
    );
    assert!(
        emptied.ends_with("## Project instructions\n\n## Own\n\n- Own rule.\n"),
        "a heading with nothing kept under it goes with the headings under it: {emptied}"
    );
}

#[test]
fn an_indented_code_block_and_untouched_blank_lines_stay_as_written() {
    let app = "## Build\n\n- Use TDD.\n\n    make test\n";
    let project =
        "## Build\n\n- Use TDD.\n- Own rule.\n\n    make test\n\n\n    make check\n\n\nNext.\n";
    let joined = render(app, project, Repeated::LeaveOut);
    assert!(
        joined.ends_with(
            "## Project instructions\n\n## Build\n\n- Own rule.\n\n    make test\n\n\n    make check\n\n\nNext.\n"
        ),
        "{joined}"
    );
    assert_eq!(repeated_lines(app, project), 1);
    assert_eq!(repeated_lines("\tmake\n", "\tmake\n"), 0);
    assert_eq!(repeated_lines("    - Use npm.\n", "    - Use npm.\n"), 0);
    let many = "- Use TDD.\n".repeat(TEXT_BYTES_MAX / 11);
    assert_eq!(repeated_lines("- Use TDD.\n", &many), TEXT_BYTES_MAX / 11);
    let chained = "Use TDD.\n".repeat(TEXT_BYTES_MAX / 9);
    assert_eq!(repeated_lines(&chained, &chained), TEXT_BYTES_MAX / 9);
}

#[test]
fn a_launch_follows_the_projects_repeated_lines_choice_and_never_dedupes_project_only() {
    let (_directory, root) = folder();
    let mut agent = project(&root);
    save_app(&agent.data, "- Use TDD.\n- No em dash.\n").unwrap();
    save_project(
        agent.folder.as_ref().unwrap(),
        "- Use TDD.\n- Run make check.\n",
    )
    .unwrap();
    let left_out = text(&agent).unwrap().text;
    assert_eq!(left_out.matches("- Use TDD.").count(), 1);
    assert!(left_out.ends_with("## Project instructions\n\n- Run make check.\n"));
    agent.repeated = Repeated::Keep;
    assert_eq!(text(&agent).unwrap().text.matches("- Use TDD.").count(), 2);
    agent.repeated = Repeated::LeaveOut;
    agent.mode = crate::instruction_files::Mode::ProjectOnly;
    assert_eq!(
        text(&agent).unwrap().text,
        "# Instructions from CrazyAgent\n\n## Project instructions\n\n- Use TDD.\n- Run make check.\n"
    );
}

#[cfg(unix)]
fn home(root: &Path, name: &str) -> PathBuf {
    let home = root.join(name);
    std::os::unix::fs::DirBuilderExt::mode(&mut fs::DirBuilder::new(), 0o700)
        .create(&home)
        .unwrap();
    home
}

#[cfg(unix)]
#[test]
fn both_texts_are_written_in_order_into_each_harness_file() {
    use std::os::unix::fs::PermissionsExt;
    let (_directory, root) = folder();
    let project = project(&root);
    save_app(&project.data, "App rule.\n").unwrap();
    save_project(project.folder.as_deref().unwrap(), "Project rule.\n").unwrap();
    let rendered = text(&project).unwrap().text;
    let app = rendered.find("App rule.").unwrap();
    let own = rendered.find("Project rule.").unwrap();
    assert!(app < own, "project text goes last");

    let claude = home(&root, "claude");
    prepare(&claude, Harness::Claude, &project, &given(&rendered)).unwrap();
    assert_eq!(
        fs::read_to_string(claude.join("CLAUDE.md")).unwrap(),
        rendered
    );
    assert!(!claude.join("AGENTS.md").exists());
    assert_eq!(
        fs::metadata(claude.join("CLAUDE.md"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let codex = home(&root, "codex");
    prepare(&codex, Harness::Codex, &project, &given(&rendered)).unwrap();
    assert_eq!(
        fs::read_to_string(codex.join("AGENTS.md")).unwrap(),
        rendered
    );
    assert!(!codex.join("CLAUDE.md").exists());
    assert_eq!(Harness::from_id("claude-code"), Some(Harness::Claude));
    assert_eq!(Harness::from_id("codex"), Some(Harness::Codex));
    assert_eq!(Harness::from_id("opencode"), None);
}

#[cfg(unix)]
#[test]
fn a_cleared_page_removes_the_stale_file_and_a_planted_entry_is_replaced_not_followed() {
    let (_directory, root) = folder();
    let project = project(&root);
    let codex = home(&root, "codex");
    prepare(&codex, Harness::Codex, &project, &given("# old\n")).unwrap();
    prepare(&codex, Harness::Codex, &project, &given("")).unwrap();
    assert!(!codex.join("AGENTS.md").exists());

    let outside = root.join("outside.md");
    fs::write(&outside, "owner file").unwrap();
    let claude = home(&root, "claude");
    std::os::unix::fs::symlink(&outside, claude.join("CLAUDE.md")).unwrap();
    prepare(&claude, Harness::Claude, &project, &given("# new\n")).unwrap();
    assert_eq!(fs::read_to_string(&outside).unwrap(), "owner file");
    assert!(
        !fs::symlink_metadata(claude.join("CLAUDE.md"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_to_string(claude.join("CLAUDE.md")).unwrap(),
        "# new\n"
    );

    fs::remove_file(claude.join("CLAUDE.md")).unwrap();
    fs::create_dir_all(claude.join("CLAUDE.md/inner")).unwrap();
    prepare(&claude, Harness::Claude, &project, &given("# again\n")).unwrap();
    assert_eq!(
        fs::read_to_string(claude.join("CLAUDE.md")).unwrap(),
        "# again\n"
    );
    assert_eq!(
        prepare(
            Path::new("relative"),
            Harness::Claude,
            &project,
            &given("x")
        )
        .unwrap_err()
        .0,
        "agent_home_invalid"
    );
}

#[cfg(unix)]
#[test]
fn a_relaunch_replaces_a_planted_skills_record_in_place_without_writing_through_it() {
    let (_directory, root) = folder();
    let data = project(&root).data;
    let outside = root.join("outside.txt");
    fs::write(&outside, "owner file").unwrap();
    let codex = home(&root, "codex");
    record_skills(&data, &codex, &[]).unwrap();
    let (records, name) = record_place(&data, SKILLS_FOLDER, &codex).unwrap();
    let record = records.join(name);
    fs::remove_file(&record).unwrap();
    std::os::unix::fs::symlink(&outside, &record).unwrap();
    record_skills(&data, &codex, &["tdd"]).unwrap();
    assert_eq!(fs::read_to_string(&outside).unwrap(), "owner file");
    let held = fs::symlink_metadata(&record).unwrap();
    assert!(held.is_file() && !held.file_type().is_symlink());
    assert_eq!(fs::read_to_string(&record).unwrap(), "tdd\n");

    fs::remove_file(&record).unwrap();
    fs::create_dir_all(record.join("inner")).unwrap();
    record_skills(&data, &codex, &[]).unwrap();
    assert_eq!(fs::read_to_string(&record).unwrap(), "");
    record_skills(&data, &codex, &["code-review"]).unwrap();
    assert_eq!(
        given_skills(&data, &codex).unwrap(),
        ["code-review"],
        "the last record replaces the one before it"
    );
    assert!(
        fs::read_dir(&codex).unwrap().next().is_none(),
        "nothing is recorded in the agent's home"
    );
}

#[cfg(unix)]
#[test]
fn what_an_agent_received_is_read_back_from_its_own_home() {
    use std::os::unix::fs::PermissionsExt;
    let (_directory, root) = folder();
    let project = project(&root);
    assert_eq!(
        received(&root.join("never-launched"), Harness::Claude).unwrap(),
        None
    );
    let claude = home(&root, "claude");
    assert_eq!(
        received(&claude, Harness::Claude).unwrap(),
        Some(Received::default())
    );

    prepare(&claude, Harness::Claude, &project, &given("# given\n")).unwrap();
    record_skills(&project.data, &claude, &["tdd", "release-notes"]).unwrap();
    assert_eq!(
        received(&claude, Harness::Claude).unwrap(),
        Some(Received {
            text: "# given\n".into(),
        })
    );
    assert_eq!(
        given_skills(&project.data, &claude).unwrap(),
        ["tdd", "release-notes"]
    );
    assert_eq!(
        received(&claude, Harness::Codex).unwrap().unwrap().text,
        "",
        "each harness reads only its own file"
    );
    let (records, name) = record_place(&project.data, SKILLS_FOLDER, &claude).unwrap();
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&records.join(name)), 0o600);
    assert_eq!(mode(&records), 0o700);

    record_skills(&project.data, &claude, &[]).unwrap();
    assert!(
        given_skills(&project.data, &claude).unwrap().is_empty(),
        "a launch without skills clears the last list"
    );
}

#[cfg(unix)]
#[test]
fn the_received_view_refuses_links_and_entries_the_launch_did_not_write() {
    let (_directory, root) = folder();
    let secret = root.join("owner-secret");
    fs::write(&secret, "OWNER-SECRET-91").unwrap();
    let claude = home(&root, "claude");
    let file = claude.join("CLAUDE.md");
    let refused = |home: &Path| received(home, Harness::Claude).unwrap_err().0;

    std::os::unix::fs::symlink(&secret, &file).unwrap();
    assert_eq!(refused(&claude), "agent_instructions_unsafe");
    fs::remove_file(&file).unwrap();
    fs::hard_link(&secret, &file).unwrap();
    assert_eq!(
        refused(&claude),
        "agent_instructions_unsafe",
        "a hard link to another file is not the launch's file"
    );
    fs::remove_file(&file).unwrap();
    fs::create_dir(&file).unwrap();
    assert_eq!(refused(&claude), "agent_instructions_unsafe");
    fs::remove_dir(&file).unwrap();

    let made = crate::spawn_gate::status(std::process::Command::new("/usr/bin/mkfifo").arg(&file))
        .unwrap();
    assert!(made.success());
    let (sender, outcome) = std::sync::mpsc::channel();
    let home = claude.clone();
    std::thread::spawn(move || {
        let _ = sender.send(received(&home, Harness::Claude).map_err(|problem| problem.0));
    });
    assert_eq!(
        outcome
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("a pipe in the home blocked the read"),
        Err("agent_instructions_unsafe".into())
    );
    fs::remove_file(&file).unwrap();

    let data = project(&root).data;
    record_skills(&data, &claude, &[]).unwrap();
    let (records, name) = record_place(&data, SKILLS_FOLDER, &claude).unwrap();
    fs::remove_file(records.join(&name)).unwrap();
    std::os::unix::fs::symlink(&secret, records.join(&name)).unwrap();
    assert_eq!(
        given_skills(&data, &claude).unwrap_err().0,
        "agent_instructions_unsafe"
    );
    fs::remove_file(records.join(&name)).unwrap();

    let linked = root.join("linked-home");
    std::os::unix::fs::symlink(&claude, &linked).unwrap();
    assert_eq!(refused(&linked), "agent_instructions_unsafe");
    assert_eq!(fs::read_to_string(&secret).unwrap(), "OWNER-SECRET-91");
}

#[cfg(unix)]
#[test]
fn the_received_view_holds_its_bounds() {
    let (_directory, root) = folder();
    let codex = home(&root, "codex");
    fs::write(codex.join("AGENTS.md"), "x".repeat(PROCESS_BYTES_MAX)).unwrap();
    assert_eq!(
        received(&codex, Harness::Codex)
            .unwrap()
            .unwrap()
            .text
            .len(),
        PROCESS_BYTES_MAX
    );
    fs::write(codex.join("AGENTS.md"), "x".repeat(PROCESS_BYTES_MAX + 1)).unwrap();
    assert_eq!(
        received(&codex, Harness::Codex).unwrap_err().0,
        "agent_instructions_too_large"
    );
    fs::write(codex.join("AGENTS.md"), b"bad \xff byte").unwrap();
    assert_eq!(
        received(&codex, Harness::Codex).unwrap_err().0,
        "agent_instructions_invalid"
    );
    fs::write(codex.join("AGENTS.md"), "fine\n").unwrap();

    let names: Vec<String> = (0..SKILLS_RECORD_MAX)
        .map(|index| format!("skill-{index}"))
        .collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let data = project(&root).data;
    record_skills(&data, &codex, &names).unwrap();
    assert_eq!(
        given_skills(&data, &codex).unwrap().len(),
        SKILLS_RECORD_MAX
    );
    let (records, name) = record_place(&data, SKILLS_FOLDER, &codex).unwrap();
    let record = records.join(name);
    fs::write(&record, format!("{}\nextra\n", names.join("\n"))).unwrap();
    assert_eq!(
        given_skills(&data, &codex).unwrap_err().0,
        "agent_instructions_too_large"
    );
    for planted in ["Upper\n", "with space\n", "-dash\n", "\n"] {
        fs::write(&record, planted).unwrap();
        assert_eq!(
            given_skills(&data, &codex).unwrap_err().0,
            "agent_instructions_invalid",
            "{planted:?}"
        );
    }
}

/// A FIFO that replaces the instruction file between any check and the open never makes
/// the read wait for a writer: every read ends with the text or `process_text_unsafe`.
#[cfg(unix)]
#[test]
fn a_fifo_swapped_in_for_the_instruction_file_never_blocks_the_read() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    let (_directory, root) = folder();
    let project = project(&root);
    let folder = project.folder.clone().unwrap();
    let state = folder.join(".crazyagent");
    let path = state.join("instructions.md");
    let plain = root.join("plain.md");
    let fifo = root.join("fifo.md");
    let swapped = state.join("swapped.md");
    fs::write(&plain, "Use TDD.\n").unwrap();
    let made = crate::spawn_gate::status(std::process::Command::new("/usr/bin/mkfifo").arg(&fifo))
        .unwrap();
    assert!(made.success());
    fs::hard_link(&plain, &path).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let swapper = std::thread::spawn({
        let (stop, path) = (stop.clone(), path.clone());
        move || {
            let mut swaps = 0_u64;
            while !stop.load(Ordering::SeqCst) {
                for source in [&fifo, &plain] {
                    fs::hard_link(source, &swapped).unwrap();
                    fs::rename(&swapped, &path).unwrap();
                    swaps += 1;
                }
            }
            swaps
        }
    });
    let (sent, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut outcomes = Vec::new();
        while Instant::now() < deadline {
            outcomes.push(read_project(&folder).map_err(|problem| problem.0));
        }
        let _ = sent.send(outcomes);
    });
    let outcomes = received.recv_timeout(Duration::from_secs(20));
    stop.store(true, Ordering::SeqCst);
    let swaps = swapper.join().unwrap();
    let outcomes = outcomes.expect("a read waited on the FIFO swapped in for the instruction file");
    assert!(swaps > 0 && !outcomes.is_empty());
    for outcome in &outcomes {
        assert!(
            matches!(
                outcome.as_deref().map_err(String::as_str),
                Ok("Use TDD.\n") | Err("process_text_unsafe")
            ),
            "{outcome:?}"
        );
    }
}

#[test]
fn the_default_process_leaves_commits_to_the_landing_rules_of_each_access_level() {
    for told in ["Commit with", "never commit", "Leave your edits as files"] {
        assert!(!DEFAULT_PROCESS.contains(told), "{told}");
    }
    for rule in [
        "Commit or leave your edits as the \"How your work lands\" section CrazyAgent gives you says; it follows the project's agent access.",
        "Never create, switch, rename, delete or push a branch, and never write inside the project folder's `.git` or rewrite history.",
        "The owner accepts your work in the project's Changes view.",
        "never retry it outside the sandbox",
    ] {
        assert!(DEFAULT_PROCESS.contains(rule), "{rule}");
    }
}

#[cfg(unix)]
#[test]
fn owner_text_up_to_the_cap_is_given_inline_and_more_is_refused_by_its_larger_part() {
    let (_directory, root) = folder();
    let project = project(&root);
    let app = format!("{}\n", "a".repeat(40 * 1024));
    save_app(&project.data, &app).unwrap();
    let given = text(&project).unwrap();
    assert!(given.text.contains(app.trim()));
    assert!(given.text.len() <= GIVEN_BYTES_MAX);

    let claude = home(&root, "claude");
    let stale = claude.join("crazyagent-context");
    fs::create_dir(&stale).unwrap();
    fs::write(stale.join("app-instructions.md"), &app).unwrap();
    prepare(&claude, Harness::Claude, &project, &given).unwrap();
    assert_eq!(
        fs::read_to_string(claude.join("CLAUDE.md")).unwrap(),
        given.text
    );
    assert!(
        !stale.exists(),
        "an earlier launch's context folder never outlives an inline launch"
    );

    let folder = project.folder.clone().unwrap();
    save_project(&folder, &"p".repeat(30 * 1024)).unwrap();
    assert_eq!(
        text(&project).unwrap_err().0,
        "agent_instructions_too_large_app"
    );
    save_project(&folder, &"p".repeat(60 * 1024)).unwrap();
    assert_eq!(
        text(&project).unwrap_err().0,
        "agent_instructions_too_large_project"
    );
}

/// A launch leaves no code an agent planted in its own home for the shell its harness
/// starts next (SA vid 12): the zsh, bash and sh startup files, the compiled zsh forms and
/// the folders whose scripts Claude Code and Codex source ahead of each shell command are
/// removed, a link among them without what it points to, while its harness state stays.
#[cfg(unix)]
#[test]
fn a_launch_removes_shell_startup_files_and_shell_prelude_folders_from_the_agent_home() {
    let (_directory, root) = folder();
    let project = project(&root);
    let given = text(&project).unwrap();
    let outside = root.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("kept.sh"), "echo kept\n").unwrap();
    let startup = [
        ".zshenv",
        ".zprofile",
        ".zshrc",
        ".zlogin",
        ".zlogout",
        ".zshenv.zwc",
        ".zprofile.zwc",
        ".zshrc.zwc",
        ".zlogin.zwc",
        ".zlogout.zwc",
        ".bashrc",
        ".bash_profile",
        ".bash_login",
        ".bash_logout",
        ".profile",
    ];
    let kept = [".claude.json", "projects/p/s.jsonl"];
    for (name, harness) in [("claude", Harness::Claude), ("codex", Harness::Codex)] {
        let home = home(&root, name);
        for file in startup.iter().chain(kept.iter()) {
            let path = home.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "echo planted\n").unwrap();
        }
        let session = home.join("session-env").join("0b6f1c2e-5d4a");
        fs::create_dir_all(&session).unwrap();
        fs::write(session.join("sessionstart-hook-0.sh"), "echo planted\n").unwrap();
        fs::create_dir(home.join("shell-snapshots")).unwrap();
        fs::write(
            home.join("shell-snapshots").join("snapshot-zsh-1.sh"),
            "echo planted\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside, home.join("shell_snapshots")).unwrap();

        prepare(&home, harness, &project, &given).unwrap();

        let left: Vec<&str> = startup
            .iter()
            .copied()
            .chain(["session-env", "shell-snapshots", "shell_snapshots"])
            .filter(|entry| fs::symlink_metadata(home.join(entry)).is_ok())
            .collect();
        assert!(
            left.is_empty(),
            "a {name} launch kept planted shell code in the agent home: {left:?}"
        );
        for file in kept {
            assert_eq!(
                fs::read_to_string(home.join(file)).unwrap(),
                "echo planted\n",
                "{file}"
            );
        }
        assert_eq!(
            fs::read_to_string(outside.join("kept.sh")).unwrap(),
            "echo kept\n"
        );
    }
}

/// A launch keeps in an agent home only what the engine and its harness use across runs,
/// and nothing a tool reads as config that runs code (RV-DS241-137): git's, cargo's,
/// npm's, yarn's, ssh's and Python's config and user sites, Node's home module folder,
/// cargo's own subcommands and a cargo cache the agent made itself (DEC-173), the packages
/// npx installed, the harness's plugins, rules and memories and every other entry go, a
/// link among them without what it names, while each
/// harness's sign-in, transcripts, team and task state, the per-agent keychain, the
/// launch's own files, the cargo caches linked from the owner's and the shared caches stay.
#[cfg(unix)]
#[test]
fn a_launch_keeps_only_harness_state_and_caches_in_the_agent_home() {
    let (_directory, root) = folder();
    let project = project(&root);
    let given = text(&project).unwrap();
    let outside = root.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("config.toml"), "kept outside\n").unwrap();
    let planted = [
        ".gitconfig",
        ".config/git/config",
        ".cargo/config",
        ".cargo/config.toml",
        ".cargo/bin/cargo-clippy",
        ".cargo/env",
        ".cargo/git/db/x",
        ".npmrc",
        ".yarnrc",
        ".yarnrc.yml",
        ".ssh/config",
        ".node_modules/left-pad/index.js",
        ".local/lib/python3.12/site-packages/run.pth",
        "Library/Python/3.9/lib/python/site-packages/run.pth",
        "Library/Application Support/pip/pip.conf",
        "Library/Preferences/com.example.plist",
        ".npm/_npx/0b6f/node_modules/run/index.js",
        "AGENTS.override.md",
        "settings.json",
        "rules/default.rules",
        "plugins/installed_plugins.json",
        ".claude/settings.json",
        "memories/note.md",
        ".zsh_history",
        "note",
    ];
    let kept = [
        ".claude.json",
        ".credentials.json",
        "auth.json",
        ".crazyagent-key",
        "crazyagent.lock",
        "crazyagent/bin/getconf",
        "crazyagent-relay/plugin.json",
        "projects/p/s.jsonl",
        "sessions/2026/11/15/rollout.jsonl",
        "teams/t/config.json",
        ".claude/teams/t/config.json",
        ".claude/tasks/t/1.json",
        "todos/t.json",
        "tasks/t/1.json",
        "plans/p.md",
        "skills/s/SKILL.md",
        "config.toml",
        "Library/Keychains/login.keychain-db",
        "Library/Preferences/com.apple.security.plist",
        ".cache/uv/x",
        ".npm/_cacache/index-v5/x",
    ];
    for (name, harness) in [("claude", Harness::Claude), ("codex", Harness::Codex)] {
        let home = home(&root, name);
        for file in planted.iter().chain(kept.iter()) {
            let path = home.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "planted\n").unwrap();
        }
        std::os::unix::fs::symlink(&outside, home.join(".cargo/registry")).unwrap();
        std::os::unix::fs::symlink(&outside, home.join(".config/cargo")).unwrap();
        std::os::unix::fs::symlink(outside.join("config.toml"), home.join("history.jsonl"))
            .unwrap();

        prepare(&home, harness, &project, &given).unwrap();

        let left: Vec<&str> = planted
            .iter()
            .copied()
            .chain([
                ".config",
                ".ssh",
                ".local",
                "history.jsonl",
                "Library/Python",
            ])
            .filter(|entry| fs::symlink_metadata(home.join(entry)).is_ok())
            .collect();
        assert!(
            left.is_empty(),
            "a {name} launch kept code-running config in the agent home: {left:?}"
        );
        for file in kept {
            assert_eq!(
                fs::read_to_string(home.join(file)).unwrap(),
                "planted\n",
                "{name}: {file}"
            );
        }
        assert_eq!(
            fs::read_link(home.join(".cargo/registry")).unwrap(),
            outside
        );
        assert_eq!(
            fs::read_to_string(outside.join("config.toml")).unwrap(),
            "kept outside\n"
        );
    }
}

/// A launch never keeps a link in the place of what it keeps, nor follows one (RV-DS241-137):
/// a `projects` folder, a `.claude.json` file and a `Library` folder that are links go
/// without what they name, so the next run neither reads its transcripts nor finds its
/// keychain through a place it chose, while cargo's `registry`, which the engine links to
/// the owner's read-only crates, stays a link.
#[cfg(unix)]
#[test]
fn a_launch_removes_a_link_in_the_place_of_a_kept_entry_without_what_it_names() {
    let (_directory, root) = folder();
    let project = project(&root);
    let given = text(&project).unwrap();
    let outside = root.join("outside");
    fs::create_dir_all(outside.join("Keychains")).unwrap();
    fs::write(outside.join("Keychains/login.keychain-db"), "outside\n").unwrap();
    fs::write(outside.join("claude.json"), "{}\n").unwrap();
    let home = home(&root, "claude");
    std::os::unix::fs::symlink(&outside, home.join("projects")).unwrap();
    std::os::unix::fs::symlink(&outside, home.join("Library")).unwrap();
    std::os::unix::fs::symlink(outside.join("claude.json"), home.join(".claude.json")).unwrap();
    fs::create_dir(home.join(".cargo")).unwrap();
    std::os::unix::fs::symlink(&outside, home.join(".cargo/registry")).unwrap();

    prepare(&home, Harness::Claude, &project, &given).unwrap();

    for entry in ["projects", "Library", ".claude.json"] {
        assert!(
            fs::symlink_metadata(home.join(entry)).is_err(),
            "{entry} kept as a link"
        );
    }
    assert_eq!(
        fs::read_link(home.join(".cargo/registry")).unwrap(),
        outside
    );
    assert_eq!(
        fs::read_to_string(outside.join("Keychains/login.keychain-db")).unwrap(),
        "outside\n"
    );
    assert_eq!(
        fs::read_to_string(outside.join("claude.json")).unwrap(),
        "{}\n"
    );
}

/// A folder a launch keeps only part of, which a process an agent left swaps for a link to
/// a folder outside the home while the reset walks it, never leads the reset outside
/// (RV-DS241-137, sec F1 ds137-lock-linux): `.npm` swapped after the reset found it and
/// before it lists it, `Library` swapped while the reset is in it and about to walk
/// `Library/Preferences`, and `.npm` swapped while the reset is about to remove `.npm/_npx`.
/// Every file in the folder the link names stays and the link goes as a link. It runs on
/// plain folders, without the macOS profile's pin, so it holds where nothing pins them.
#[cfg(unix)]
#[test]
fn a_kept_folder_swapped_for_a_link_mid_reset_never_leads_the_reset_outside() {
    let legs = [
        (".npm", ".npm", ".npm/junk"),
        ("Library/Preferences", "Library", "Library/Preferences/junk"),
        (".npm/_npx", ".npm", ".npm/_npx/0"),
    ];
    let owned = [
        "owner.txt",
        "junk/owner.txt",
        "Preferences/owner.plist",
        "_npx/owner.js",
    ];
    for (step, swapped, planted) in legs {
        let (_directory, root) = folder();
        let project = project(&root);
        let given = text(&project).unwrap();
        let home = home(&root, "claude");
        fs::create_dir_all(home.join(planted)).unwrap();
        let outside = root.join("outside");
        for file in owned {
            let path = outside.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "the owner's\n").unwrap();
        }
        let (link, moved, named) = (home.join(swapped), root.join("moved"), outside.clone());
        let swap: Box<dyn FnOnce()> = Box::new(move || {
            fs::rename(&link, &moved).unwrap();
            std::os::unix::fs::symlink(&named, &link).unwrap();
        });
        BEFORE_HOME_STEP.with(|hook| *hook.borrow_mut() = Some((home.join(step), swap)));

        let prepared = prepare(&home, Harness::Claude, &project, &given);

        let waiting = BEFORE_HOME_STEP.with(|hook| hook.borrow_mut().take());
        assert!(waiting.is_none(), "{step}: the swap never ran");
        let lost: Vec<&str> = owned
            .into_iter()
            .filter(|file| {
                fs::read_to_string(outside.join(file)).ok().as_deref() != Some("the owner's\n")
            })
            .collect();
        assert!(
            lost.is_empty(),
            "{step}: the reset removed {lost:?} outside the home"
        );
        assert_eq!(prepared, Ok(()), "{step}");
        assert!(
            fs::symlink_metadata(home.join(swapped)).is_err(),
            "{step}: {swapped} kept as a link"
        );
    }
}

/// A link an agent left in the place of a folder a launch keeps only part of goes as a
/// link, at the home and one folder below it, and the reset never walks through it, so
/// every file in the folder outside the home it names stays (RV-DS241-137, sec F1
/// ds137-lock-linux). Passes at the base too; it guards the handle walk.
#[cfg(unix)]
#[test]
fn a_link_in_the_place_of_a_kept_folder_goes_at_any_depth_without_what_it_names() {
    let (_directory, root) = folder();
    let project = project(&root);
    let given = text(&project).unwrap();
    let outside = root.join("outside");
    fs::create_dir_all(outside.join("sub")).unwrap();
    fs::write(outside.join("owner.plist"), "the owner's\n").unwrap();
    fs::write(outside.join("sub/owner.txt"), "the owner's\n").unwrap();
    let home = home(&root, "claude");
    fs::create_dir(home.join("Library")).unwrap();
    let links = [".npm", "Library/Preferences"];
    for link in links {
        std::os::unix::fs::symlink(&outside, home.join(link)).unwrap();
    }

    prepare(&home, Harness::Claude, &project, &given).unwrap();

    for link in links {
        assert!(
            fs::symlink_metadata(home.join(link)).is_err(),
            "{link} kept as a link"
        );
    }
    for file in ["owner.plist", "sub/owner.txt"] {
        assert_eq!(
            fs::read_to_string(outside.join(file)).ok().as_deref(),
            Some("the owner's\n"),
            "{file}"
        );
    }
}

/// Cargo's `registry` and `git` caches stay in an agent home only as the links the engine
/// makes to the owner's read-only crates (DEC-173): a cache folder an agent made itself,
/// which a later run's cargo would build from without checking it again, goes at each
/// launch with all it holds, also when no link takes its place, while a link stays a link
/// and what it names stays as it was.
#[cfg(unix)]
#[test]
fn a_cargo_cache_an_agent_made_goes_at_each_launch_and_only_a_link_stays() {
    let (_directory, root) = folder();
    let project = project(&root);
    let given = text(&project).unwrap();
    let crates = root.join("owner-crates");
    fs::create_dir_all(crates.join("src")).unwrap();
    fs::write(crates.join("src/build.rs"), "fn main() {}\n").unwrap();
    for (name, linked) in [("made", false), ("linked", true)] {
        let home = home(&root, name);
        let cargo = home.join(".cargo");
        for cache in ["registry", "git"] {
            if linked && cache == "registry" {
                fs::create_dir_all(&cargo).unwrap();
                std::os::unix::fs::symlink(&crates, cargo.join(cache)).unwrap();
                continue;
            }
            let planted = cargo.join(cache).join("src/build.rs");
            fs::create_dir_all(planted.parent().unwrap()).unwrap();
            fs::write(&planted, "fn main() { planted() }\n").unwrap();
        }

        prepare(&home, Harness::Codex, &project, &given).unwrap();

        let left: Vec<&str> = ["registry", "git"]
            .into_iter()
            .filter(|cache| !(linked && *cache == "registry"))
            .filter(|cache| fs::symlink_metadata(cargo.join(cache)).is_ok())
            .collect();
        assert!(
            left.is_empty(),
            "{name}: cargo caches the agent made stayed: {left:?}"
        );
        if linked {
            assert_eq!(fs::read_link(cargo.join("registry")).unwrap(), crates);
        }
    }
    assert_eq!(
        fs::read_to_string(crates.join("src/build.rs")).unwrap(),
        "fn main() {}\n"
    );
}

/// A flag an agent set in its own home never keeps its next launch from starting
/// (RV-DS241-138): an immutable hook file in an immutable Claude Code session folder, an
/// append-only startup file, an immutable cargo config in an immutable `.cargo` and an
/// immutable home itself all give way, so the launch removes what it removes and writes
/// what it writes, while the harness state it keeps stays.
#[cfg(target_os = "macos")]
#[test]
fn a_flag_an_agent_set_in_its_home_never_blocks_its_next_launch() {
    use nix::sys::stat::FileFlag;
    let (_directory, root) = folder();
    let project = project(&root);
    let given = text(&project).unwrap();
    let home = home(&root, "claude");
    let session = home.join("session-env").join("0b6f1c2e-5d4a");
    let hook = session.join("sessionstart-hook-0.sh");
    fs::create_dir_all(&session).unwrap();
    fs::write(&hook, "echo planted\n").unwrap();
    let startup = home.join(".zshrc");
    fs::write(&startup, "echo planted\n").unwrap();
    let cargo = home.join(".cargo");
    fs::create_dir(&cargo).unwrap();
    let config = cargo.join("config.toml");
    fs::write(&config, "[build]\nrustc-wrapper = \"/private/tmp/run\"\n").unwrap();
    let claude = home.join(".claude.json");
    fs::write(&claude, "{}\n").unwrap();
    let pinned = [
        (claude.as_path(), FileFlag::UF_IMMUTABLE),
        (hook.as_path(), FileFlag::UF_IMMUTABLE),
        (session.as_path(), FileFlag::UF_IMMUTABLE),
        (startup.as_path(), FileFlag::UF_APPEND),
        (config.as_path(), FileFlag::UF_IMMUTABLE),
        (cargo.as_path(), FileFlag::UF_IMMUTABLE),
        (home.as_path(), FileFlag::UF_IMMUTABLE),
    ];
    for (path, flag) in pinned {
        nix::unistd::chflags(path, flag).unwrap();
    }

    let prepared = prepare(&home, Harness::Claude, &project, &given);

    let kept_flags = fs::symlink_metadata(&claude).map(|metadata| {
        FileFlag::from_bits_retain(std::os::macos::fs::MetadataExt::st_flags(&metadata))
    });
    let left: Vec<&Path> = [
        hook.as_path(),
        session.as_path(),
        startup.as_path(),
        config.as_path(),
    ]
    .into_iter()
    .filter(|path| fs::symlink_metadata(path).is_ok())
    .collect();
    for (path, _) in pinned {
        let _ = nix::unistd::chflags(path, FileFlag::empty());
    }
    assert_eq!(prepared, Ok(()));
    assert!(left.is_empty(), "a flag kept {left:?} from the launch");
    assert_eq!(fs::read_to_string(&claude).unwrap(), "{}\n");
    assert!(
        kept_flags
            .as_ref()
            .is_ok_and(|flags| !flags.intersects(FileFlag::UF_IMMUTABLE)),
        "a kept file stayed pinned: {kept_flags:?}"
    );
}

/// A flag on an entry its owner cannot open never keeps the next launch from starting
/// (RV-DS241-137 code r1 F2): an immutable file at mode 000, and an immutable folder at mode
/// 000 holding one too, both go, though neither opens for reading.
#[cfg(target_os = "macos")]
#[test]
fn a_flag_on_an_entry_at_mode_000_never_blocks_the_next_launch() {
    use nix::sys::stat::FileFlag;
    use std::os::unix::fs::PermissionsExt;
    let (_directory, root) = folder();
    let project = project(&root);
    let given = text(&project).unwrap();
    let home = home(&root, "claude");
    let locked = home.join("locked");
    fs::write(&locked, "planted\n").unwrap();
    let vault = home.join("vault");
    fs::create_dir(&vault).unwrap();
    let inner = vault.join("inner");
    fs::write(&inner, "planted\n").unwrap();
    let pinned = [locked.as_path(), inner.as_path(), vault.as_path()];
    for path in pinned {
        fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
        nix::unistd::chflags(path, FileFlag::UF_IMMUTABLE).unwrap();
    }

    let prepared = prepare(&home, Harness::Claude, &project, &given);

    let left: Vec<&Path> = [locked.as_path(), vault.as_path()]
        .into_iter()
        .filter(|path| fs::symlink_metadata(path).is_ok())
        .collect();
    for path in pinned.into_iter().rev() {
        let _ = nix::unistd::chflags(path, FileFlag::empty());
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    }
    assert_eq!(prepared, Ok(()));
    assert!(
        left.is_empty(),
        "a flag at mode 000 kept {left:?} from the launch"
    );
}

/// An ACL entry an agent set in its own home never keeps its next launch from starting
/// (RV-DS241-137 sec r1 F2): a deny entry binds the owner too, yet a file whose ACL denies
/// its removal, a folder whose ACL denies removing it and what it holds, and a kept
/// `.claude.json` whose ACL denies writing it all give way, so the launch removes what it
/// removes and the kept file holds no ACL entry after it.
#[cfg(target_os = "macos")]
#[test]
fn an_acl_an_agent_set_in_its_home_never_blocks_its_next_launch() {
    use std::process::Command;
    let (_directory, root) = folder();
    let project = project(&root);
    let given = text(&project).unwrap();
    let home = home(&root, "claude");
    let note = home.join("note");
    fs::write(&note, "planted\n").unwrap();
    let stuck = home.join("stuck");
    fs::create_dir(&stuck).unwrap();
    let inner = stuck.join("inner");
    fs::write(&inner, "planted\n").unwrap();
    let claude = home.join(".claude.json");
    fs::write(&claude, "{}\n").unwrap();
    let chmod = |arguments: &[&str]| {
        let done =
            crate::spawn_gate::collected(Command::new("/bin/chmod").args(arguments)).unwrap();
        assert!(done.status.success(), "{arguments:?}: {done:?}");
    };
    let path = |path: &Path| path.to_str().unwrap().to_owned();
    chmod(&["+a", "everyone deny delete", &path(&note)]);
    chmod(&["+a", "everyone deny delete", &path(&inner)]);
    chmod(&["+a", "everyone deny delete,delete_child", &path(&stuck)]);
    chmod(&["+a", "everyone deny write,delete", &path(&claude)]);

    let prepared = prepare(&home, Harness::Claude, &project, &given);

    let listed =
        crate::spawn_gate::collected(Command::new("/bin/ls").arg("-led").arg(&claude)).unwrap();
    let kept_acl = String::from_utf8_lossy(&listed.stdout).into_owned();
    let left: Vec<&Path> = [note.as_path(), stuck.as_path()]
        .into_iter()
        .filter(|path| fs::symlink_metadata(path).is_ok())
        .collect();
    let _ = crate::spawn_gate::collected(Command::new("/bin/chmod").args(["-R", "-N"]).arg(&home));
    assert_eq!(prepared, Ok(()));
    assert!(left.is_empty(), "an ACL kept {left:?} from the launch");
    assert_eq!(fs::read_to_string(&claude).unwrap(), "{}\n");
    assert!(
        listed.status.success() && !kept_acl.contains("deny"),
        "a kept file kept its ACL: {kept_acl}"
    );
}

/// Adds one ACL entry to a test path with `/bin/chmod +a` and requires it to hold. Inputs:
/// the entry and the path.
#[cfg(target_os = "macos")]
fn acl(entry: &str, path: &Path) {
    let done = crate::spawn_gate::collected(
        std::process::Command::new("/bin/chmod")
            .args(["+a", entry])
            .arg(path),
    )
    .unwrap();
    assert!(done.status.success(), "{entry} {path:?}: {done:?}");
}

/// Whether a test path's ACL holds a deny entry, as `/bin/ls -led` lists it. Input: the
/// path. Output: the answer.
#[cfg(target_os = "macos")]
fn denies(path: &Path) -> bool {
    let listed =
        crate::spawn_gate::collected(std::process::Command::new("/bin/ls").arg("-led").arg(path))
            .unwrap();
    listed.status.success() && String::from_utf8_lossy(&listed.stdout).contains("deny")
}

/// Clears a test path's ACL and user flags when dropped, so its temporary folder can be
/// deleted whatever the test asserted. Holds the path.
#[cfg(target_os = "macos")]
struct Released(PathBuf);

#[cfg(target_os = "macos")]
impl Drop for Released {
    fn drop(&mut self) {
        let _ = nix::unistd::chflags(self.0.as_path(), nix::sys::stat::FileFlag::empty());
        let _ = crate::spawn_gate::collected(
            std::process::Command::new("/bin/chmod")
                .arg("-N")
                .arg(&self.0),
        );
    }
}

/// A launch that clears a home folder an agent left without write never changes the ACL of
/// a file a link in that folder names (RV-DS241-137 sec r2 F1): the folder and the link go,
/// and the file outside the home keeps its entry, since a tree's ACLs go only on its
/// one-link entries that are not links and on links themselves, as a launch context's
/// release clears them.
#[cfg(target_os = "macos")]
#[test]
fn a_link_in_a_home_folder_the_launch_opens_up_never_carries_an_acl_clear_outside() {
    use std::os::unix::fs::PermissionsExt;
    let (_directory, root) = folder();
    let project = project(&root);
    let given = text(&project).unwrap();
    let home = home(&root, "claude");
    let outside = root.join("outside.txt");
    fs::write(&outside, "the owner's\n").unwrap();
    let _outside = Released(outside.clone());
    acl("everyone deny write", &outside);
    let shut = home.join("d");
    fs::create_dir(&shut).unwrap();
    std::os::unix::fs::symlink(&outside, shut.join("x")).unwrap();
    fs::set_permissions(&shut, fs::Permissions::from_mode(0o500)).unwrap();

    let prepared = prepare(&home, Harness::Claude, &project, &given);

    let _ = fs::set_permissions(&shut, fs::Permissions::from_mode(0o700));
    assert_eq!(prepared, Ok(()));
    assert!(
        fs::symlink_metadata(&shut).is_err(),
        "the shut folder stayed"
    );
    assert!(denies(&outside), "a file outside the home lost its ACL");
}

/// A kept name an agent made a second link to a file outside its home never changes that
/// file at the next launch (RV-DS241-137 sec r2 F1, code r2 F2, V231-ACL): `.claude.json`
/// linked to a file whose ACL denies writing goes as a plain unlink and the file keeps its
/// entry, and `.credentials.json` linked to a file pinned immutable, which no unlink takes,
/// refuses the launch while the file keeps its flag.
#[cfg(target_os = "macos")]
#[test]
fn a_kept_name_linked_to_a_file_outside_the_home_never_changes_that_file() {
    use nix::sys::stat::FileFlag;
    use std::os::macos::fs::MetadataExt as _;
    let (_directory, root) = folder();
    let project = project(&root);
    let given = text(&project).unwrap();
    let denied = root.join("denied.json");
    fs::write(&denied, "{}\n").unwrap();
    let _denied = Released(denied.clone());
    acl("everyone deny write", &denied);
    let claude = home(&root, "claude");
    fs::hard_link(&denied, claude.join(".claude.json")).unwrap();
    let pinned = root.join("pinned.json");
    fs::write(&pinned, "{}\n").unwrap();
    let _pinned = Released(pinned.clone());
    let codex = home(&root, "codex");
    fs::hard_link(&pinned, codex.join(".credentials.json")).unwrap();
    nix::unistd::chflags(pinned.as_path(), FileFlag::UF_IMMUTABLE).unwrap();

    let linked_acl = prepare(&claude, Harness::Claude, &project, &given);
    let linked_flag = prepare(&codex, Harness::Codex, &project, &given);

    assert_eq!(linked_acl, Ok(()));
    assert!(denies(&denied), "a file outside the home lost its ACL");
    assert_eq!(
        fs::metadata(&denied).unwrap().nlink(),
        1,
        "the second link stayed"
    );
    assert_eq!(
        linked_flag,
        Err(AgentHomeError("agent_home_unwritable".into()))
    );
    let flags = fs::symlink_metadata(&pinned).unwrap().st_flags();
    assert_ne!(
        flags & FileFlag::UF_IMMUTABLE.bits(),
        0,
        "a file outside the home lost its flag"
    );
}
/// On Linux the reset keeps a home's launch lock that an agent gave a second name in a kept
/// folder, the same file the launch holds (RV-DS241-137 r3 code F2): Landlock and bubblewrap
/// keep a confined agent from linking a file from outside its write roots, so only macOS,
/// whose profile pins the lock, refuses a second link; removing the lock here would let a
/// second launch into the home.
#[cfg(target_os = "linux")]
#[test]
fn a_linux_reset_keeps_the_held_lock_whatever_its_links() {
    let (_directory, root) = folder();
    let project = project(&root);
    let given = text(&project).unwrap();
    let claude = home(&root, "claude");
    let lock = claude.join(crate::launch_context::LOCK);
    fs::write(&lock, "").unwrap();
    fs::create_dir(claude.join(".cache")).unwrap();
    fs::hard_link(&lock, claude.join(".cache").join("x")).unwrap();
    let inode = fs::symlink_metadata(&lock).unwrap().ino();

    let reset = prepare(&claude, Harness::Claude, &project, &given);

    assert_eq!(reset, Ok(()));
    assert_eq!(
        fs::symlink_metadata(&lock).map(|found| found.ino()).ok(),
        Some(inode),
        "the held lock stays"
    );
}

/// A home folder whose ACLs deny listing two folders deep still goes at the next launch
/// (RV-DS241-137 code r2 F3): each folder's ACL is cleared by its path before the walk reads
/// it, so a tree a Full run or an earlier version's run left that way never refuses every
/// later launch. Passes at the base too (scratch probe, DONE r3); it guards the new tree clear.
#[cfg(target_os = "macos")]
#[test]
fn a_home_folder_whose_acls_deny_listing_two_deep_still_goes() {
    let (_directory, root) = folder();
    let project = project(&root);
    let given = text(&project).unwrap();
    let home = home(&root, "claude");
    let outer = home.join("outer");
    let inner = outer.join("inner");
    let file = inner.join("file");
    fs::create_dir_all(&inner).unwrap();
    fs::write(&file, "planted\n").unwrap();
    let _file = Released(file.clone());
    let _inner = Released(inner.clone());
    let _outer = Released(outer.clone());
    acl("everyone deny delete", &file);
    acl("everyone deny list,search", &inner);
    acl("everyone deny list,search", &outer);

    let prepared = prepare(&home, Harness::Claude, &project, &given);

    assert_eq!(prepared, Ok(()));
    assert!(
        fs::symlink_metadata(&outer).is_err(),
        "a tree whose ACLs deny listing stayed"
    );
}

/// A home an earlier run left with an ACL that denies listing it, or listing a folder the
/// launch keeps only part of, still gets its reset (RV-DS241-137 code r2 F3): a listing that
/// stops clears that folder's ACL by its path and lists it once more, so a Full run or an
/// earlier version's run never refuses every later launch.
#[cfg(target_os = "macos")]
#[test]
fn a_home_or_kept_folder_whose_acl_denies_listing_never_refuses_the_launch() {
    let (_directory, root) = folder();
    let project = project(&root);
    let given = text(&project).unwrap();
    let home = home(&root, "claude");
    let npm = home.join(".npm");
    fs::create_dir_all(npm.join("_npx/0")).unwrap();
    fs::write(home.join("note"), "planted\n").unwrap();
    let _npm = Released(npm.clone());
    let _home = Released(home.clone());
    acl("everyone deny list", &npm);
    acl("everyone deny list", &home);

    let prepared = prepare(&home, Harness::Claude, &project, &given);

    assert_eq!(prepared, Ok(()));
    assert!(
        fs::symlink_metadata(home.join("note")).is_err(),
        "a planted file stayed"
    );
    assert!(
        fs::symlink_metadata(npm.join("_npx")).is_err(),
        "npx's packages stayed"
    );
}

/// A file a launch removes whose ACL denies reading and deleting it still goes
/// (RV-DS241-137 code r2 F3): once the plain unlink stops, its ACL is cleared by its path,
/// which needs no read, where clearing it on the entry itself opens it and fails.
#[cfg(target_os = "macos")]
#[test]
fn a_file_whose_acl_denies_reading_and_deleting_it_still_goes() {
    let (_directory, root) = folder();
    let project = project(&root);
    let given = text(&project).unwrap();
    let home = home(&root, "claude");
    let planted = home.join("note");
    fs::write(&planted, "planted\n").unwrap();
    let _planted = Released(planted.clone());
    acl("everyone deny read,delete", &planted);

    let prepared = prepare(&home, Harness::Claude, &project, &given);

    assert_eq!(prepared, Ok(()));
    assert!(
        fs::symlink_metadata(&planted).is_err(),
        "the denied file stayed"
    );
}

/// A home an agent filled with more entries than one listing of it takes still launches,
/// and every entry it planted goes while what the harness keeps stays (RV-DS241-137), so a
/// run that makes a hundred thousand files in its home never refuses every later launch.
#[cfg(unix)]
#[test]
fn a_launch_clears_a_home_an_agent_filled_with_entries() {
    let (_directory, root) = folder();
    let project = project(&root);
    let given = text(&project).unwrap();
    let home = home(&root, "claude");
    fs::write(home.join(".claude.json"), "{}\n").unwrap();
    let planted = 100_001;
    for index in 0..planted {
        File::create(home.join(format!("n-{index}"))).unwrap();
    }

    prepare(&home, Harness::Claude, &project, &given).unwrap();

    let left: Vec<String> = fs::read_dir(&home)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, [".claude.json"]);
}

/// What an agent's own run leaves in its home runs nothing in a git, Python, cargo or npm
/// it starts later, sandboxed or at Full access, which share one environment (RV-DS241-137,
/// sec r1 F1): git reads no `.gitconfig`, so its `core.fsmonitor` program never runs, Python
/// reads no user site, so a `.pth` file there runs nothing, npm reads no `.npmrc`, so its
/// `node-options` never reach a script, and a cargo config naming a `rustc-wrapper` an
/// earlier run left goes at the next launch. Without each pin the same plant runs, so each
/// leg proves its pin. Needs `/usr/bin/git` and `/usr/bin/python3` from the Command Line
/// Tools, npm and node on `PATH`, and the cargo and rustc the tests use.
#[cfg(target_os = "macos")]
#[test]
fn git_python_and_cargo_in_an_agent_run_nothing_its_home_config_names() {
    use std::ffi::{OsStr, OsString};
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    let (_directory, root) = folder();
    let project = project(&root);
    let given = text(&project).unwrap();
    let home = home(&root, "agent");
    let marker = root.join("ran.txt");
    let fsmonitor = root.join("fsmonitor.sh");
    let wrapper = root.join("wrapper.sh");
    for (script, then) in [(&fsmonitor, "exit 1"), (&wrapper, "exec \"$@\"")] {
        fs::write(
            script,
            format!(
                "#!/bin/sh\necho '{}' >> '{}'\n{then}\n",
                script.display(),
                marker.display()
            ),
        )
        .unwrap();
        fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let probe = root.join("probe");
    fs::create_dir_all(probe.join("src")).unwrap();
    fs::write(
        probe.join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    fs::write(probe.join("src/lib.rs"), "").unwrap();
    fs::create_dir(home.join(".cargo")).unwrap();
    fs::write(
        home.join(".cargo/config.toml"),
        format!("[build]\nrustc-wrapper = \"{}\"\n", wrapper.display()),
    )
    .unwrap();

    prepare(&home, Harness::Claude, &project, &given).unwrap();

    fs::write(
        home.join(".gitconfig"),
        format!("[core]\n\tfsmonitor = {}\n", fsmonitor.display()),
    )
    .unwrap();
    let site = crate::spawn_gate::collected(
        Command::new("/usr/bin/python3")
            .args(["-c", "import site; print(site.getusersitepackages())"])
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin"),
    )
    .unwrap();
    assert!(site.status.success(), "{site:?}");
    let site = PathBuf::from(String::from_utf8(site.stdout).unwrap().trim());
    assert!(site.starts_with(&home), "{}", site.display());
    fs::create_dir_all(&site).unwrap();
    fs::write(
        site.join("run.pth"),
        format!(
            "import os; open('{}', 'a').write('pth\\n')\n",
            marker.display()
        ),
    )
    .unwrap();
    let repo = root.join("repo");
    fs::create_dir(&repo).unwrap();
    fs::write(repo.join("a.txt"), "a\n").unwrap();
    for arguments in [&["init", "-q"][..], &["add", "a.txt"][..]] {
        let setup = crate::spawn_gate::collected(
            Command::new("/usr/bin/git")
                .args(arguments)
                .current_dir(&repo)
                .env_clear()
                .env("HOME", &root)
                .env("PATH", "/usr/bin:/bin")
                .env("GIT_CONFIG_NOSYSTEM", "1"),
        )
        .unwrap();
        assert!(setup.status.success(), "{setup:?}");
    }
    let path = std::env::var_os("PATH").unwrap_or_else(|| OsString::from("/usr/bin:/bin"));
    let owner = std::env::var_os("HOME").map(PathBuf::from);
    let rustup = std::env::var_os("RUSTUP_HOME")
        .or_else(|| owner.map(|owner| owner.join(".rustup").into_os_string()))
        .unwrap();
    let toolchain = std::env::var_os("RUSTUP_TOOLCHAIN");
    let npm = std::env::split_paths(&path)
        .map(|folder| folder.join("npm"))
        .find(|candidate| candidate.is_file())
        .expect("the row needs npm on PATH")
        .into_os_string();
    let package = root.join("package");
    fs::create_dir(&package).unwrap();
    fs::write(
        package.join("package.json"),
        "{\"name\":\"p\",\"version\":\"0.0.0\",\"scripts\":{\"x\":\"node -e 0\"}}\n",
    )
    .unwrap();
    let required = root.join("npmrc.js");
    fs::write(
        &required,
        format!(
            "require('fs').appendFileSync('{}', 'npmrc\\n');\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::write(
        home.join(".npmrc"),
        format!("node-options=--require={}\n", required.display()),
    )
    .unwrap();
    let cargo_home = home.join(".cargo");
    let target = root.join("target");
    let mut base: Vec<(&str, &OsStr)> = vec![
        ("HOME", home.as_os_str()),
        ("PATH", path.as_os_str()),
        ("RUSTUP_HOME", rustup.as_os_str()),
        ("CARGO_HOME", cargo_home.as_os_str()),
        ("CARGO_TARGET_DIR", target.as_os_str()),
    ];
    if let Some(toolchain) = &toolchain {
        base.push(("RUSTUP_TOOLCHAIN", toolchain.as_os_str()));
    }
    let env =
        crate::sandbox::agent_env(&base, std::iter::empty::<(&OsStr, Option<&OsStr>)>()).unwrap();
    let mut ran = Vec::new();
    for (program, arguments, folder) in [
        (
            OsString::from("/usr/bin/git"),
            &["status", "--porcelain"][..],
            &repo,
        ),
        (
            OsString::from("/usr/bin/python3"),
            &["-c", "pass"][..],
            &repo,
        ),
        (
            OsString::from(env!("CARGO")),
            &[
                "check",
                "--offline",
                "--quiet",
                "--manifest-path",
                "Cargo.toml",
            ][..],
            &probe,
        ),
        (
            npm.clone(),
            &["--no-update-notifier", "run", "x"][..],
            &package,
        ),
    ] {
        let output = crate::spawn_gate::collected(
            Command::new(&program)
                .args(arguments)
                .current_dir(folder)
                .env_clear()
                .envs(env.iter().map(|(name, value)| (name, value))),
        )
        .unwrap();
        ran.push(format!("{program:?}: {output:?}"));
        assert!(output.status.success(), "{ran:?}");
    }
    assert!(
        !marker.exists(),
        "an agent tool ran what its home config named: {:?}\n{ran:?}",
        fs::read_to_string(&marker).unwrap_or_default()
    );
    for (pin, program, arguments, folder, expected) in [
        (
            "GIT_CONFIG_GLOBAL",
            OsString::from("/usr/bin/git"),
            &["status", "--porcelain"][..],
            &repo,
            fsmonitor.display().to_string(),
        ),
        (
            "PYTHONNOUSERSITE",
            OsString::from("/usr/bin/python3"),
            &["-c", "pass"][..],
            &repo,
            "pth".to_owned(),
        ),
        (
            "NPM_CONFIG_USERCONFIG",
            npm.clone(),
            &["--no-update-notifier", "run", "x"][..],
            &package,
            "npmrc".to_owned(),
        ),
    ] {
        let output = crate::spawn_gate::collected(
            Command::new(&program)
                .args(arguments)
                .current_dir(folder)
                .env_clear()
                .envs(
                    env.iter()
                        .filter(|(name, _)| name.as_os_str() != OsStr::new(pin))
                        .map(|(name, value)| (name, value)),
                ),
        )
        .unwrap();
        let heard = fs::read_to_string(&marker).unwrap_or_default();
        assert!(
            heard.contains(&expected),
            "without {pin} the planted config ran nothing, so the row proves nothing: \
             {heard:?}\n{output:?}"
        );
    }
}
