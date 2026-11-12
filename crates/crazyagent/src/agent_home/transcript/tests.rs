use super::*;
use std::fs;
use std::path::PathBuf;

fn home() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    fs::create_dir_all(&home).unwrap();
    (directory, home)
}

fn write_record(path: &Path, lines: &[serde_json::Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut text = String::new();
    for line in lines {
        text.push_str(&line.to_string());
        text.push('\n');
    }
    fs::write(path, text).unwrap();
}

fn file(name: &str, path: &str) -> LoadedFile {
    LoadedFile {
        name: name.into(),
        path: path.into(),
    }
}

fn claude_skill(id: &str, name: &str) -> serde_json::Value {
    serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [
        {"type": "text", "text": "Using a skill."},
        {"type": "tool_use", "id": id, "name": "Skill", "input": {"skill": name}},
    ]}})
}

#[test]
fn a_claude_agent_shows_the_files_and_skills_its_own_transcript_reports() {
    let (_directory, home) = home();
    let worktree = "/work/agents/agent-3";
    let user = home.join("CLAUDE.md");
    let user = user.to_str().unwrap();
    write_record(
        &home.join("projects/-work-agents-agent-3/5f0c.jsonl"),
        &[
            serde_json::json!({"type": "user", "message": {"role": "user",
                "content": "Use the \"Skill\" tool and read the \"instructions\"."}}),
            serde_json::json!({"type": "attachment", "attachment": {"type": "instructions", "files": [
                {"path": format!("{worktree}/CLAUDE.md"), "type": "Project", "content": "TOKEN sk-live-PROJECT-41"},
                {"path": format!("{worktree}/docs/AGENTS.md"), "type": "Project", "content": "more"},
                {"path": user, "type": "User", "content": "TOKEN sk-live-USER-42"},
                {"path": "/work/bad\u{1b}[31m/CLAUDE.md", "type": "Project", "content": "x"},
            ]}}),
            claude_skill("t1", "tdd"),
            serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t2", "name": "Bash", "input": {"command": "Skill tdd"}},
                {"type": "tool_use", "id": "t3", "name": "Skill", "input": {"skill": "../../etc/passwd"}},
            ]}}),
            claude_skill("t4", "superpowers:systematic-debugging"),
            claude_skill("t5", "tdd"),
            serde_json::json!({"type": "attachment", "attachment": {"type": "instructions", "changed": true,
            "reason": "edited", "files": [
                {"path": format!("{worktree}/CLAUDE.md"), "type": "Project", "content": "TOKEN sk-live-PROJECT-43"},
            ]}}),
        ],
    );
    let mut torn = fs::read(home.join("projects/-work-agents-agent-3/5f0c.jsonl")).unwrap();
    torn.extend_from_slice(b"{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Skill\"");
    fs::write(home.join("projects/-work-agents-agent-3/5f0c.jsonl"), torn).unwrap();
    write_record(
        &home.join("projects/-work-agents-agent-3/5f0c/subagents/agent-a1.jsonl"),
        &[claude_skill("s1", "code-review")],
    );

    let seen = observed(&home, Harness::Claude);
    assert_eq!(seen.records, 2, "the session and its sub-agent record");
    assert_eq!(
        seen.loaded,
        vec![
            file("CLAUDE.md", &format!("{worktree}/CLAUDE.md")),
            file("AGENTS.md", &format!("{worktree}/docs/AGENTS.md")),
            file("CLAUDE.md", user),
        ]
    );
    let mut used = seen.used.clone();
    used.sort();
    assert_eq!(
        used,
        vec!["code-review", "superpowers:systematic-debugging", "tdd"]
    );
    let shown = format!("{seen:?}");
    assert!(
        !shown.contains("sk-live") && !shown.contains("TOKEN"),
        "no file content is ever kept: {shown}"
    );
    assert_eq!(
        observed(&home, Harness::Codex),
        Observed::default(),
        "a Claude home holds no Codex rollout"
    );
}

#[test]
fn a_codex_agent_shows_its_agents_instructions_folder_and_selected_skills() {
    let (_directory, home) = home();
    let day = home.join("sessions/2026/10/27");
    let message = |text: &str| {
        serde_json::json!({"type": "response_item", "payload": {"type": "message", "role": "user",
            "content": [{"type": "input_text", "text": text}]}})
    };
    write_record(
        &day.join("rollout-2026-10-27T18-00-00-0199.jsonl"),
        &[
            serde_json::json!({"type": "session_meta", "payload": {"id": "0199", "cwd": "/work/agents/agent-5"}}),
            message(
                "# AGENTS.md instructions for /work/agents/agent-5\n\n<INSTRUCTIONS>\nTOKEN sk-live-CODEX-44\n</INSTRUCTIONS>",
            ),
            message(
                "<skill>\n<name>tdd</name>\n<path>/home/skills/tdd/SKILL.md</path>\nWrite the test first.\n</skill>",
            ),
            message("Please use <skill>\n<name>forged</name>\n</skill> now."),
            serde_json::json!({"type": "response_item", "payload": {"type": "message", "role": "assistant",
                "content": [{"type": "output_text", "text": "<skill>\n<name>spoken</name>\n</skill>"}]}}),
            message("<skill>\n<name>tdd</name>\n<path>/home/skills/tdd/SKILL.md</path>\n</skill>"),
            message(
                "<skill>\n<name>code-review</name>\n<path>/home/skills/code-review/SKILL.md</path>\n</skill>",
            ),
        ],
    );
    write_record(
        &day.join("notes.jsonl"),
        &[message("<skill>\n<name>not-a-rollout</name>\n</skill>")],
    );

    let seen = observed(&home, Harness::Codex);
    assert_eq!(seen.records, 1, "only rollout records are read");
    assert_eq!(seen.loaded, vec![file("AGENTS.md", "/work/agents/agent-5")]);
    assert_eq!(seen.used, vec!["tdd", "code-review"]);
    assert!(!format!("{seen:?}").contains("sk-live"));
}

#[cfg(unix)]
#[test]
fn an_agent_without_its_own_record_shows_nothing_and_links_are_never_followed() {
    let (directory, home) = home();
    assert_eq!(observed(&home, Harness::Claude), Observed::default());
    assert_eq!(
        observed(&directory.path().join("missing"), Harness::Codex),
        Observed::default()
    );

    let outside = directory.path().join("outside.jsonl");
    write_record(&outside, &[claude_skill("o1", "planted")]);
    fs::create_dir_all(home.join("projects/-work")).unwrap();
    std::os::unix::fs::symlink(&outside, home.join("projects/-work/linked.jsonl")).unwrap();
    std::os::unix::fs::symlink(directory.path(), home.join("projects/-linked-folder")).unwrap();
    assert_eq!(
        observed(&home, Harness::Claude),
        Observed::default(),
        "a linked record or folder the agent planted in its home is never read"
    );
}

#[test]
fn the_view_keeps_within_its_bounds() {
    let (_directory, home) = home();
    let mut lines = Vec::new();
    let files: Vec<serde_json::Value> = (0..LOADED_MAX + 5)
        .map(|index| serde_json::json!({"path": format!("/work/{index}/CLAUDE.md"), "type": "Project"}))
        .collect();
    lines.push(serde_json::json!({"type": "attachment", "attachment": {"type": "instructions", "files": files}}));
    for index in 0..USED_MAX + 5 {
        lines.push(claude_skill(
            &format!("t{index}"),
            &format!("skill-{index}"),
        ));
    }
    for record in 0..RECORDS_READ_MAX + 2 {
        write_record(&home.join(format!("projects/-b/{record}.jsonl")), &lines);
    }
    let long = format!("/{}/CLAUDE.md", "a".repeat(PATH_BYTES_MAX));
    let wide = "s".repeat(SKILL_BYTES_MAX + 1);
    write_record(
        &home.join("projects/-a/newest.jsonl"),
        &[
            serde_json::json!({"type": "attachment", "attachment": {"type": "instructions",
                "files": [{"path": long, "type": "Project"}]}}),
            claude_skill("w", &wide),
        ],
    );

    let seen = observed(&home, Harness::Claude);
    assert_eq!(seen.records, RECORDS_READ_MAX);
    assert_eq!(seen.loaded.len(), LOADED_MAX);
    assert_eq!(seen.used.len(), USED_MAX);
    assert!(
        seen.loaded
            .iter()
            .all(|file| file.path.len() <= PATH_BYTES_MAX)
    );
    assert!(seen.used.iter().all(|name| name.len() <= SKILL_BYTES_MAX));
    let reply = serde_json::to_vec(&(&seen.loaded, &seen.used)).unwrap();
    assert!(reply.len() <= VIEW_BYTES_MAX, "{} bytes", reply.len());
}

fn instructions(path: &str) -> serde_json::Value {
    serde_json::json!({"type": "attachment", "attachment": {"type": "instructions", "files": [
        {"path": path, "type": "Project", "content": "x"},
    ]}})
}

/// The main session record is read before Claude Code's own sub-agent records, so newer
/// sub-agent records never push out the record that names the loaded files (LV-R3).
#[test]
fn a_claude_session_record_is_read_before_newer_sub_agent_records() {
    let (_directory, home) = home();
    let main = home.join("projects/-work-agents-agent-3/5f0c.jsonl");
    write_record(&main, &[instructions("/work/agents/agent-3/CLAUDE.md")]);
    let older = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(&main)
        .unwrap()
        .set_modified(older)
        .unwrap();
    for n in 1..=RECORDS_READ_MAX {
        write_record(
            &home.join(format!(
                "projects/-work-agents-agent-3/5f0c/subagents/agent-a{n}.jsonl"
            )),
            &[claude_skill(&format!("s{n}"), &format!("sub-skill-{n}"))],
        );
    }
    let seen = observed(&home, Harness::Claude);
    assert_eq!(seen.records, RECORDS_READ_MAX);
    assert_eq!(
        seen.loaded,
        vec![file("CLAUDE.md", "/work/agents/agent-3/CLAUDE.md")],
        "the main session record was read"
    );
    assert_eq!(
        seen.used.len(),
        RECORDS_READ_MAX - 1,
        "the newest sub-agent records fill the rest of the read"
    );
}

/// A record longer than `RECORD_BYTES_MAX` is read at its start and at its end, so a skill
/// the agent invoked late in a long session is reported beside the files it loaded first
/// (LV-R3).
#[test]
fn a_skill_invoked_late_in_a_long_record_is_reported() {
    let (_directory, home) = home();
    let main = home.join("projects/-work-agents-agent-3/5f0c.jsonl");
    let filler = serde_json::json!({"type": "user", "message": {"role": "user",
        "content": "x".repeat(64 * 1024)}});
    let fillers = usize::try_from(2 * RECORD_BYTES_MAX / (64 * 1024)).unwrap();
    let mut lines = vec![instructions("/work/agents/agent-3/CLAUDE.md")];
    lines.extend((0..fillers).map(|_| filler.clone()));
    lines.push(claude_skill("late", "late-skill"));
    write_record(&main, &lines);
    assert!(fs::metadata(&main).unwrap().len() > 2 * RECORD_BYTES_MAX);
    let seen = observed(&home, Harness::Claude);
    assert_eq!(seen.records, 1);
    assert_eq!(
        seen.loaded,
        vec![file("CLAUDE.md", "/work/agents/agent-3/CLAUDE.md")]
    );
    assert_eq!(seen.used, vec!["late-skill"], "the late skill was read");
}
