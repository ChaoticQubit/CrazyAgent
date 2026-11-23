use super::*;
use kernel_common::relay_names::{
    LOCAL_PLACEHOLDER, TOKEN_HEX, TOKEN_PREFIX, is_placeholder, is_relay_token,
};

/// A drawn lineage token and a relay token are each told by their shape, the local
/// placeholder is a placeholder but no relay token, and text of another shape is neither.
#[test]
fn a_lineage_token_and_a_relay_token_are_told_by_their_shape() {
    let token = LineageToken::new().unwrap();
    assert!(LineageToken::valid(token.as_str()));
    assert!(!LineageToken::valid("ABC"));
    let relay = format!("{TOKEN_PREFIX}{}", "a".repeat(TOKEN_HEX));
    assert!(is_relay_token(&relay));
    assert!(is_placeholder(&relay));
    assert!(is_placeholder(LOCAL_PLACEHOLDER));
    assert!(!is_relay_token(LOCAL_PLACEHOLDER));
    assert!(!is_placeholder(LINEAGE_VARIABLE));
}

/// An agent's environment holds the fixed agent homes the owner's terminal leaves out, a
/// secret name or a real key never reaches either, a launch hands only a relay token or the
/// placeholder, and a Full access command needs an absolute program.
#[test]
fn an_agent_environment_refuses_secrets_and_the_owner_terminal_keeps_its_home() {
    use crate::profile::consts::AGENT_HOME_ENV;
    use kernel_common::env_names::agent_env_refused;
    use std::ffi::OsStr;
    let base = [
        ("HOME", OsStr::new("/tmp/agent-home")),
        ("TMPDIR", OsStr::new("/tmp/agent-tmp")),
    ];
    let agent = agent_process_env(&base, []).unwrap();
    let owner = owner_terminal_env(&base, []).unwrap();
    for (name, value) in AGENT_HOME_ENV {
        assert!(
            agent
                .iter()
                .any(|(known, set)| known == name && set == value)
        );
    }
    assert_eq!(agent.len(), owner.len() + AGENT_HOME_ENV.len());
    let secret = [(OsStr::new("GITHUB_TOKEN"), Some(OsStr::new("held")))];
    assert!(agent_process_env(&base, secret).is_err());
    assert!(agent_env_refused(OsStr::new("GITHUB_TOKEN")));
    assert!(!agent_variable_allowed(
        OsStr::new("ANTHROPIC_API_KEY"),
        OsStr::new("sk-real")
    ));
    assert!(Handed::new(&["OPENAI_API_KEY"], "sk-real").is_err());
    #[cfg(unix)]
    assert!(unconfined_command(std::path::Path::new("bin/agent"), &base, []).is_err());
    let routed = Routed {
        any: vec![443],
        bridged: Vec::new(),
    };
    assert_eq!(routed.clone(), routed);
}
