use super::*;

#[test]
fn a_launch_that_left_context_mode_out_says_why_in_its_tools_section() {
    use crate::plugins::ContextModeOff;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    for (off, line) in [
        (
            ContextModeOff::SettingUp,
            "- context-mode is off for this launch: CrazyAgent is still setting it up on this computer. Use the shell instead; agents started once it is set up get it.\n",
        ),
        (
            ContextModeOff::Failed,
            "- context-mode is off for this launch: setting it up failed on this computer. Use the shell instead, and tell the owner in your report to check context-mode in Settings.\n",
        ),
        (
            ContextModeOff::Mismatch,
            "- context-mode is off for this launch: its installed files did not match the copy the default pack pins, so CrazyAgent did not run them. Use the shell instead, and tell the owner in your report.\n",
        ),
        (
            ContextModeOff::NodeTooOld,
            "- context-mode is off for this launch: it needs Node.js 22.5 or newer, and this machine's Node.js is older. Use the shell instead, and tell the owner in your report to update Node.js.\n",
        ),
        (
            ContextModeOff::NpxNotFound,
            "- context-mode is off for this launch: CrazyAgent did not find the npx it runs on this computer, so it did not start it. Use the shell instead, and tell the owner in your report to check context-mode in Settings.\n",
        ),
    ] {
        let plugins = LaunchPlugins {
            skills: Vec::new(),
            servers: vec![command_server("graph")],
            context_mode_off: Some(off),
            left_out: Vec::new(),
        };
        let home = private_home(&root, &format!("{off:?}").to_lowercase());
        let installed = context_with(&root, plugins, Vec::new())
            .install("claude-code", &home, true)
            .unwrap();
        let prompt = fs::read_to_string(home.join(STATE).join("instructions.md")).unwrap();
        drop(installed);
        let section = tools_section(&prompt);
        assert!(section.contains(line), "{off:?}: {section}");
        assert!(!section.contains("ctx_execute"), "{off:?}: {section}");
    }
}
