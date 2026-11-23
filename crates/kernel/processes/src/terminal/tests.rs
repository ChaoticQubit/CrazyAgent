use super::*;

#[test]
fn terminal_names_are_short_lowercase_words_with_inner_dashes() {
    let long = "a".repeat(AGENT_ID_BYTES_MAX + 1);
    for good in ["team-1", "research-42", "a", "x9"] {
        assert_eq!(AgentId::new(good).unwrap().as_str(), good);
    }
    for bad in [
        "",
        "-team",
        "team-",
        "Team-1",
        "team_1",
        "team.1",
        "team:1",
        "team 1",
        "_crazyagent",
        long.as_str(),
    ] {
        assert_eq!(
            AgentId::new(bad).unwrap_err().0,
            "terminal_agent_invalid",
            "{bad}"
        );
    }
}

#[test]
fn a_team_agent_terminal_is_named_after_its_team_agent_id() {
    let id = AgentId::team(7).unwrap();
    assert_eq!(id.as_str(), "team-7");
    assert_eq!(id.team_agent(), Some(7));
    assert_eq!(AgentId::team(0).unwrap_err().0, "terminal_agent_invalid");
    assert_eq!(AgentId::new("team-07").unwrap().team_agent(), None);
    assert_eq!(AgentId::new("research-7").unwrap().team_agent(), None);
}
