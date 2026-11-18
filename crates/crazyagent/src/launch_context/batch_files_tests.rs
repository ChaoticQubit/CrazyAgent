use super::*;

/// V243-WIN-MCP-NPX: a launch that left servers out because each one's command is not a
/// Windows program names them in its tools section, at most four then how many more, within the
/// line's bound, and says what to tell the owner; a launch with none says nothing of them.
#[test]
fn a_launch_that_left_batch_file_servers_out_names_them_in_its_tools_section() {
    assert_eq!(batch_files_line(&[]), None);
    let names = |count: usize| {
        (0..count)
            .map(|n| format!("server-{n}"))
            .collect::<Vec<_>>()
    };
    let one = batch_files_line(&names(1)).unwrap();
    assert!(
        one.starts_with("- MCP servers off for this launch: `server-0`. Each one's command is not a Windows program (.exe or .com), which Windows may start through cmd.exe"),
        "{one}"
    );
    assert!(
        one.ends_with("set its command to the tool's .exe, or to node.exe with the tool's script as the first argument.\n"),
        "{one}"
    );
    let six = batch_files_line(&names(6)).unwrap();
    assert!(
        six.contains("`server-0`, `server-1`, `server-2`, `server-3` and 2 more."),
        "{six}"
    );
    assert!(!six.contains("server-4"), "{six}");
    let longest = vec![
        "x".repeat(2 * crate::plugins::SERVER_NAME_BYTES_MAX);
        2 * crate::plugins::SERVERS_PER_SCOPE_MAX
    ];
    assert!(batch_files_line(&longest).unwrap().len() <= BATCH_LINE_BYTES_MAX);

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let plugins = |left: Vec<String>| LaunchPlugins {
        skills: Vec::new(),
        servers: vec![command_server("context-mode"), command_server("graph")],
        context_mode_off: None,
        batch_files: left.clone(),
        commands_not_found: Vec::new(),
        left_out: left,
    };
    let line = batch_files_line(&["notes".to_owned()]).unwrap();
    let claude = private_home(&root, "claude");
    let installed = context_with(&root, plugins(vec!["notes".into()]), Vec::new())
        .install("claude-code", &claude, true)
        .unwrap();
    let prompt = fs::read_to_string(claude.join(STATE).join("instructions.md")).unwrap();
    drop(installed);
    assert!(tools_section(&prompt).contains(&line), "{prompt}");

    let codex = private_home(&root, "codex");
    let installed = context_with(&root, plugins(vec!["notes".into()]), Vec::new())
        .install("codex", &codex, true)
        .unwrap();
    let config: toml::Table = fs::read_to_string(codex.join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let developer = config["developer_instructions"]
        .as_str()
        .unwrap()
        .to_owned();
    drop(installed);
    assert!(tools_section(&developer).contains(&line), "{developer}");

    let most = private_home(&root, "most");
    let installed = context_with(&root, plugins(longest), Vec::new())
        .install("claude-code", &most, true)
        .unwrap();
    drop(installed);

    let none = private_home(&root, "none");
    let installed = context_with(&root, plugins(Vec::new()), Vec::new())
        .install("claude-code", &none, true)
        .unwrap();
    let prompt = fs::read_to_string(none.join(STATE).join("instructions.md")).unwrap();
    drop(installed);
    assert!(!prompt.contains("MCP servers off"), "{prompt}");
}

/// V243-MCP-NPX-LAUNCH-LINE (#349 r2 code F1): a launch that left servers out because each
/// one's command was not found, a missing npx.cmd among them, or the tool checks refused it
/// names them on a line of their own that says to check them in Settings, never the line that
/// calls them no Windows program; a launch with both causes carries both lines.
#[test]
fn a_launch_that_left_not_found_servers_out_names_them_on_their_own_line() {
    assert_eq!(not_found_line(&[]), None);
    let one = not_found_line(&["gone".to_owned()]).unwrap();
    assert!(
        one.starts_with("- MCP servers off for this launch: `gone`. Each one's command was not found on this computer"),
        "{one}"
    );
    assert!(one.ends_with("check these servers in Settings.\n"), "{one}");
    assert!(!one.contains("Windows program"), "{one}");
    let names: Vec<String> = (0..6).map(|n| format!("server-{n}")).collect();
    let six = not_found_line(&names).unwrap();
    assert!(
        six.contains("`server-0`, `server-1`, `server-2`, `server-3` and 2 more."),
        "{six}"
    );
    assert!(!six.contains("server-4"), "{six}");

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let plugins = LaunchPlugins {
        skills: Vec::new(),
        servers: vec![command_server("context-mode"), command_server("graph")],
        context_mode_off: None,
        batch_files: vec!["notes".into()],
        commands_not_found: vec!["gone".into()],
        left_out: vec!["notes".into(), "gone".into()],
    };
    let batch = batch_files_line(&["notes".to_owned()]).unwrap();
    let claude = private_home(&root, "claude");
    let installed = context_with(&root, plugins, Vec::new())
        .install("claude-code", &claude, true)
        .unwrap();
    let prompt = fs::read_to_string(claude.join(STATE).join("instructions.md")).unwrap();
    drop(installed);
    let section = tools_section(&prompt);
    assert!(section.contains(&batch), "{prompt}");
    assert!(section.contains(&one), "{prompt}");
    assert!(!batch.contains("`gone`"), "{batch}");
}

/// The tools section at its longest, a Claude Code launch with the context-mode server, a
/// skill name past its clip and both left-out lines each naming the most servers a launch can
/// leave out, stays within `TOOL_USE_BYTES_MAX`: the section's assert never stops a launch.
#[test]
fn the_longest_tools_section_with_both_left_out_lines_fits_its_bound() {
    let longest = vec![
        "x".repeat(2 * crate::plugins::SERVER_NAME_BYTES_MAX);
        2 * crate::plugins::SERVERS_PER_SCOPE_MAX
    ];
    let servers = vec![command_server(CONTEXT_MODE_SERVER)];
    let skill = "s".repeat(2 * NAME_BYTES_MAX);
    let section = tool_use_section(true, &servers, None, &longest, &longest, &[], Some(&skill));
    assert!(section.contains("is not a Windows program"), "{section}");
    assert!(
        section.contains("was not found on this computer"),
        "{section}"
    );
    assert!(section.contains(&skill[..NAME_BYTES_MAX]), "{section}");
    assert!(section.len() <= TOOL_USE_BYTES_MAX, "{}", section.len());
}
