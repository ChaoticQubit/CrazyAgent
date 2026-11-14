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
        "process_text_unsafe"
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
    let stale = claude.join(STALE_CONTEXT_FOLDER);
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
