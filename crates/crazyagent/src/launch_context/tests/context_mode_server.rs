use super::*;

#[test]
fn the_context_mode_server_is_declared_under_its_plugin_name_for_claude_and_codex() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let plugins = || LaunchPlugins {
        skills: Vec::new(),
        servers: vec![command_server("context-mode"), command_server("graph")],
        context_mode_off: None,
        batch_files: Vec::new(),
        commands_not_found: Vec::new(),
        left_out: Vec::new(),
    };

    let claude = private_home(&root, "claude");
    let installed = context_with(&root, plugins(), Vec::new())
        .install("claude-code", &claude, true)
        .unwrap();
    let mcp: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(claude.join(STATE).join("mcp.json")).unwrap())
            .unwrap();
    let declared = mcp["mcpServers"].as_object().unwrap();
    assert!(declared.contains_key("plugin_context-mode_context-mode"));
    assert!(declared.contains_key("graph"));
    assert!(!declared.contains_key("context-mode"));
    drop(installed);

    let codex = private_home(&root, "codex");
    let installed = context_with(&root, plugins(), Vec::new())
        .install("codex", &codex, true)
        .unwrap();
    let declared = codex_servers(&codex);
    assert!(declared.contains_key("plugin_context-mode_context-mode"));
    assert!(declared.contains_key("graph"));
    assert!(!declared.contains_key("context-mode"));
    drop(installed);
}
