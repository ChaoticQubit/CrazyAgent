use super::{Search, TeamUse, catalog, sign_in_min, specs};

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
