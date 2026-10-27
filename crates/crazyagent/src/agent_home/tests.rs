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
    }
}

#[test]
fn both_texts_are_saved_and_read_back_and_a_missing_file_is_empty() {
    let (_directory, root) = folder();
    let project = project(&root);
    let folder = project.folder.clone().unwrap();
    assert_eq!(read_app(&project.data).unwrap(), "");
    assert_eq!(read_project(&folder).unwrap(), "");
    assert_eq!(text(&project).unwrap(), "");

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
    let rendered = text(&project).unwrap();
    assert!(rendered.len() <= PROCESS_BYTES_MAX);

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
    assert_eq!(render("", " \n"), "");
    assert_eq!(
        render("Use TDD.\n", "Run make check.\n"),
        "# Instructions from CrazyAgent\n\n## App instructions\n\nUse TDD.\n\n## Project instructions\n\nRun make check.\n"
    );
    assert_eq!(
        render("", "Only project.\n"),
        "# Instructions from CrazyAgent\n\n## Project instructions\n\nOnly project.\n"
    );
}

#[cfg(unix)]
fn home(root: &Path, name: &str) -> PathBuf {
    use std::os::unix::fs::DirBuilderExt;
    let home = root.join(name);
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
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
    let rendered = text(&project).unwrap();
    let app = rendered.find("App rule.").unwrap();
    let own = rendered.find("Project rule.").unwrap();
    assert!(app < own, "project text goes last");

    let claude = home(&root, "claude");
    prepare(&claude, Harness::Claude, &project, &rendered).unwrap();
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
    prepare(&codex, Harness::Codex, &project, &rendered).unwrap();
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
    prepare(&codex, Harness::Codex, &project, "# old\n").unwrap();
    prepare(&codex, Harness::Codex, &project, "").unwrap();
    assert!(!codex.join("AGENTS.md").exists());

    let outside = root.join("outside.md");
    fs::write(&outside, "owner file").unwrap();
    let claude = home(&root, "claude");
    std::os::unix::fs::symlink(&outside, claude.join("CLAUDE.md")).unwrap();
    prepare(&claude, Harness::Claude, &project, "# new\n").unwrap();
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
    prepare(&claude, Harness::Claude, &project, "# again\n").unwrap();
    assert_eq!(
        fs::read_to_string(claude.join("CLAUDE.md")).unwrap(),
        "# again\n"
    );
    assert_eq!(
        prepare(Path::new("relative"), Harness::Claude, &project, "x")
            .unwrap_err()
            .0,
        "agent_home_invalid"
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

    prepare(&claude, Harness::Claude, &project, "# given\n").unwrap();
    record_skills(&claude, &["tdd", "release-notes"]).unwrap();
    assert_eq!(
        received(&claude, Harness::Claude).unwrap(),
        Some(Received {
            text: "# given\n".into(),
            skills: vec!["tdd".into(), "release-notes".into()],
        })
    );
    assert_eq!(
        received(&claude, Harness::Codex).unwrap().unwrap().text,
        "",
        "each harness reads only its own file"
    );
    assert_eq!(
        fs::metadata(claude.join(SKILLS_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    record_skills(&claude, &[]).unwrap();
    assert!(
        received(&claude, Harness::Claude)
            .unwrap()
            .unwrap()
            .skills
            .is_empty(),
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

    std::os::unix::fs::symlink(&secret, claude.join(SKILLS_FILE)).unwrap();
    assert_eq!(refused(&claude), "agent_instructions_unsafe");
    fs::remove_file(claude.join(SKILLS_FILE)).unwrap();

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
    record_skills(&codex, &names).unwrap();
    assert_eq!(
        received(&codex, Harness::Codex)
            .unwrap()
            .unwrap()
            .skills
            .len(),
        SKILLS_RECORD_MAX
    );
    fs::write(
        codex.join(SKILLS_FILE),
        format!("{}\nextra\n", names.join("\n")),
    )
    .unwrap();
    assert_eq!(
        received(&codex, Harness::Codex).unwrap_err().0,
        "agent_instructions_too_large"
    );
    for planted in ["Upper\n", "with space\n", "-dash\n", "\n"] {
        fs::write(codex.join(SKILLS_FILE), planted).unwrap();
        assert_eq!(
            received(&codex, Harness::Codex).unwrap_err().0,
            "agent_instructions_invalid",
            "{planted:?}"
        );
    }
}
