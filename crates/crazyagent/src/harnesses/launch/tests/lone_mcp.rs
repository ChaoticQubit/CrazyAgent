use super::*;

/// A context-less launch of Codex and of Claude Code with tools on keeps the empty MCP list
/// it is built with, Codex's `mcp_servers={}` and Claude Code's empty `--mcp-config`, since
/// the lock it runs under wrote no context.
#[test]
fn a_context_less_launch_with_tools_keeps_its_empty_mcp_list() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::Builder::new()
        .prefix("ca-lone-mcp-")
        .tempdir_in(at_path("/tmp", std::fs::canonicalize("/tmp"))?)?;
    let base = directory.path().canonicalize()?;
    let profile = base.join("project.sb");
    at_path(
        &profile,
        std::fs::write(&profile, "(version 1)\n(allow default)\n"),
    )?;
    for (harness, empty) in [
        ("codex", "mcp_servers={}"),
        ("claude-code", "{\"mcpServers\":{}}"),
    ] {
        let home = base.join(harness).join("account-1");
        let work = base.join(format!("{harness}-work"));
        for folder in [&home, &work] {
            at_path(folder, std::fs::create_dir_all(folder))?;
        }
        for folder in [base.clone(), base.join(harness), home.clone()] {
            at_path(
                &folder,
                std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)),
            )?;
        }
        let request = LaunchRequest {
            harness: harness.into(),
            executable: PathBuf::from("/usr/bin/true"),
            working_dir: work,
            account: AccountBinding::Subscription { home },
            model: None,
            tools: true,
            boundary: LaunchBoundary::Profile(profile.clone()),
            git_identity: None,
            limits: LaunchLimits::default(),
        };
        let built = super::super::lone_command(&request);
        assert!(built.is_ok(), "{harness}: {:?}", built.as_ref().err());
        let (command, _lock) = built.unwrap();
        assert!(
            command.get_args().any(|argument| argument == empty),
            "{harness}"
        );
    }
    Ok(())
}
