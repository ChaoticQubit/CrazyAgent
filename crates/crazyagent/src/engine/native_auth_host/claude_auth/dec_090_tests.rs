use super::{ClaudeAuthCommand, auth_command, verified_status};
use crate::team::runtime::managed_agent::{ManagedAgent, claude};

#[test]
fn the_claude_custodian_status_never_loads_what_a_tools_on_agent_wrote() {
    let claude = claude();
    let agent = ManagedAgent::new("claude-code", &claude);
    let marker = agent.base.join("dec-090-helper-ran");
    let helper = agent.working_dir.join("dec-090-helper.sh");
    let settings = serde_json::json!({ "apiKeyHelper": helper }).to_string();
    assert!(!settings.contains('\''), "{settings}");
    let profile = format!("'{}'", agent.profile_home.display());
    let plant = format!(
        "printf '#!/bin/sh\\necho ran > {marker}\\necho planted-helper-key\\n' > '{helper}' && chmod 700 '{helper}' && echo \"helper written\"\n\
         for folder in {profile} {profile}/.claude \"$HOME\" \"$HOME/.claude\" \"$CLAUDE_CONFIG_DIR\"; do\n\
         mkdir -p \"$folder\" 2>/dev/null\n\
         for name in settings.json settings.local.json; do\n\
         (printf '%s' '{settings}' > \"$folder/$name\") 2>/dev/null && echo \"planted $folder/$name\"\n\
         done\n\
         done\n",
        marker = marker.display(),
        helper = helper.display(),
    );
    let planted = agent.shell(&plant);
    let planted = String::from_utf8_lossy(&planted.stdout).into_owned();
    assert!(planted.starts_with("helper written\n"), "{planted}");
    let status = crate::spawn_gate::output(
        &mut auth_command(&claude, &agent.profile_home, ClaudeAuthCommand::Status).unwrap(),
    )
    .unwrap();
    let stdout = String::from_utf8_lossy(&status.stdout).into_owned();
    let verified = verified_status(&status.stdout, &agent.profile_home, status.status.code());
    let mut problems = Vec::new();
    if !matches!(verified, Ok(None)) {
        problems.push(format!(
            "the custodian did not see a signed-out profile: {verified:?}"
        ));
    }
    if stdout.contains("apiKeyHelper") || stdout.contains("api_key_helper") {
        problems.push("the custodian's status used the planted apiKeyHelper".into());
    }
    if marker.exists() {
        problems.push("the planted apiKeyHelper ran".into());
    }
    assert!(
        problems.is_empty(),
        "the Claude custodian on {} loaded what a tools-on agent wrote:\n{}\nstatus: {stdout}\nagent planted:\n{planted}",
        agent.profile_home.display(),
        problems.join("\n")
    );
}
