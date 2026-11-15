use super::manifest::SubAgents;
use super::{HarnessEntry, Search, TeamUse, catalog, sign_in_min, specs};

fn nothing_installed() -> Search {
    Search::new(Vec::new(), None, Vec::new())
}

#[test]
fn the_catalog_says_what_each_harness_can_do_in_a_team_as_it_did_before_the_manifests() {
    let entries = catalog(&nothing_installed());
    assert_eq!(entries.len(), 18);
    for (entry, spec) in entries.iter().zip(specs()) {
        assert_eq!(entry.id, spec.id);
        let team = match spec.id {
            "codex" | "claude-code" => TeamUse::Available,
            "rcli" => TeamUse::MasterOnly {
                reason: "runs only as a CrazyAgent master".into(),
            },
            _ => TeamUse::Unavailable {
                reason: "CrazyAgent cannot read its replies yet".into(),
            },
        };
        assert_eq!(entry.team, team, "{}", spec.id);
        if spec.id == "claude-code" {
            assert_eq!(entry.aliases, ["claude"]);
        } else {
            assert!(entry.aliases.is_empty(), "{}", spec.id);
        }
    }
}

#[test]
fn only_codex_and_claude_code_have_a_sign_in_minimum_and_it_is_the_one_their_adapter_names() {
    assert_eq!(
        sign_in_min("codex"),
        Some(crate::adapters::codex::SIGN_IN_MIN)
    );
    assert_eq!(
        sign_in_min("claude-code"),
        Some(crate::adapters::claude::SIGN_IN_MIN)
    );
    assert_eq!(sign_in_min("claude"), sign_in_min("claude-code"));
    for spec in specs() {
        if !matches!(spec.id, "codex" | "claude-code") {
            assert_eq!(sign_in_min(spec.id), None, "{}", spec.id);
        }
    }
    assert_eq!(sign_in_min("no-such-harness"), None);
}

#[test]
fn the_catalog_says_what_each_harness_can_be_and_which_endpoint_accounts_it_runs_on() {
    let entries = catalog(&nothing_installed());
    let row = |id: &str| entries.iter().find(|entry| entry.id == id).unwrap();
    let codex = row("codex");
    assert!(codex.terminal && codex.master && codex.research);
    assert_eq!(codex.sub_agents, SubAgents::Threads);
    assert_eq!(codex.account_protocols, ["openai*"]);
    let claude = row("claude-code");
    assert!(claude.terminal && claude.master && claude.research);
    assert_eq!(claude.sub_agents, SubAgents::Teams);
    assert_eq!(claude.account_protocols, ["anthropic-messages"]);
    let rcli = row("rcli");
    assert!(!rcli.terminal && rcli.master && !rcli.research);
    assert_eq!(rcli.sub_agents, SubAgents::App);
    assert_eq!(rcli.account_protocols, ["*"]);
    for entry in &entries {
        assert_eq!(
            entry.terminal,
            entry.team == TeamUse::Available,
            "{}",
            entry.id
        );
        if matches!(entry.id.as_str(), "codex" | "claude-code" | "rcli") {
            continue;
        }
        assert!(
            !entry.terminal && !entry.master && !entry.research,
            "{}",
            entry.id
        );
        assert_eq!(entry.sub_agents, SubAgents::App, "{}", entry.id);
        assert!(entry.account_protocols.is_empty(), "{}", entry.id);
    }
}

#[test]
fn a_catalog_row_carries_the_new_settings_in_plain_words_and_reads_back_the_same() {
    let entries = catalog(&nothing_installed());
    for entry in &entries {
        let value = serde_json::to_value(entry).unwrap();
        assert_eq!(value["terminal"], entry.terminal, "{}", entry.id);
        assert_eq!(value["master"], entry.master, "{}", entry.id);
        assert_eq!(value["research"], entry.research, "{}", entry.id);
        let back: HarnessEntry = serde_json::from_value(value).unwrap();
        assert_eq!(&back, entry);
    }
    let claude = serde_json::to_value(&entries[1]).unwrap();
    assert_eq!(entries[1].id, "claude-code");
    assert_eq!(claude["sub_agents"], "teams");
    assert_eq!(
        claude["account_protocols"],
        serde_json::json!(["anthropic-messages"])
    );
    assert_eq!(
        serde_json::to_value(&entries[0]).unwrap()["sub_agents"],
        "threads"
    );
    assert_eq!(
        serde_json::to_value(&entries[2]).unwrap()["sub_agents"],
        "app"
    );
}
