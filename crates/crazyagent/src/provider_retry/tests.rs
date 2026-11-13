use super::*;

fn engine() -> (tempfile::TempDir, Connection) {
    let folder = tempfile::tempdir().unwrap();
    let connection = Connection::open(folder.path().join("engine.db")).unwrap();
    initialize(&connection).unwrap();
    (folder, connection)
}

#[test]
fn a_fresh_engine_retries_briefly() {
    let (_folder, connection) = engine();
    assert_eq!(load(&connection).unwrap(), ProviderRetry::RetryBriefly);
    assert_eq!(ProviderRetry::default(), ProviderRetry::RetryBriefly);
}

#[test]
fn each_option_is_saved_and_read_back_by_a_read_only_reader() {
    let (folder, connection) = engine();
    for choice in [
        ProviderRetry::StopAtOnce,
        ProviderRetry::HarnessDefault,
        ProviderRetry::RetryBriefly,
    ] {
        assert_eq!(save(&connection, choice).unwrap(), Outcome::Applied);
        assert_eq!(load(&connection).unwrap(), choice);
        let reader = Connection::open_with_flags(
            folder.path().join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(load(&reader).unwrap(), choice);
    }
    let stored: i64 = connection
        .query_row("SELECT count(*) FROM provider_retry_settings", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(stored, 1);
}

/// A stored choice this engine does not know is refused with its code, never read as a
/// default, so no run guesses its retry limit.
#[test]
fn a_stored_choice_this_engine_does_not_know_is_refused() {
    let folder = tempfile::tempdir().unwrap();
    let connection = Connection::open(folder.path().join("engine.db")).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE provider_retry_settings (singleton INTEGER PRIMARY KEY, choice TEXT NOT NULL);
             INSERT INTO provider_retry_settings VALUES(1,'retry_forever');",
        )
        .unwrap();
    initialize(&connection).unwrap();
    assert_eq!(
        load(&connection).unwrap_err().0,
        "provider_retry_invalid".to_owned()
    );
}

/// Codex (DEC-163 P1, P3): Stop at once turns the endless connect wait off and sets both
/// retry counts to 0 on a custom provider; Retry briefly only turns the endless wait off;
/// Harness default sets nothing. The built-in `openai` provider only ever takes the feature
/// setting, since Codex refuses to start on any `model_providers.openai` key.
#[test]
fn codex_gets_the_wait_bound_and_its_counts_only_where_its_provider_takes_them() {
    let custom = CodexProvider::Custom("crazyagent_endpoint");
    assert_eq!(
        ProviderRetry::StopAtOnce.codex_settings(custom),
        vec![
            CODEX_UNBOUNDED_OFF.to_owned(),
            "model_providers.crazyagent_endpoint.request_max_retries=0".to_owned(),
            "model_providers.crazyagent_endpoint.stream_max_retries=0".to_owned(),
        ]
    );
    assert_eq!(
        ProviderRetry::RetryBriefly.codex_settings(custom),
        vec![CODEX_UNBOUNDED_OFF.to_owned()]
    );
    assert_eq!(
        ProviderRetry::HarnessDefault.codex_settings(custom),
        Vec::<String>::new()
    );
    for choice in [ProviderRetry::StopAtOnce, ProviderRetry::RetryBriefly] {
        assert_eq!(
            choice.codex_settings(CodexProvider::BuiltIn),
            vec![CODEX_UNBOUNDED_OFF.to_owned()]
        );
    }
    assert_eq!(
        ProviderRetry::HarnessDefault.codex_settings(CodexProvider::BuiltIn),
        Vec::<String>::new()
    );
    for choice in ProviderRetry::ALL {
        for provider in [
            custom,
            CodexProvider::Custom("crazyagent"),
            CodexProvider::BuiltIn,
        ] {
            assert!(
                choice
                    .codex_settings(provider)
                    .iter()
                    .all(|setting| !setting.starts_with("model_providers.openai.")),
                "{choice:?} {provider:?}"
            );
        }
    }
}

/// Claude Code (DEC-163 P2, P3): only Stop at once sets its retry cap, to 0; the other
/// options leave its own retries.
#[test]
fn claude_code_gets_its_retry_cap_only_under_stop_at_once() {
    assert_eq!(
        ProviderRetry::StopAtOnce.claude_env(),
        Some((CLAUDE_MAX_RETRIES, "0"))
    );
    assert_eq!(ProviderRetry::RetryBriefly.claude_env(), None);
    assert_eq!(ProviderRetry::HarnessDefault.claude_env(), None);
    assert_eq!(CLAUDE_MAX_RETRIES, "CLAUDE_CODE_MAX_RETRIES");
}

/// RCLI's requests go through the engine's model gateway: one try under Stop at once and
/// Harness default (its behaviour today), two retries under Retry briefly.
#[test]
fn rcli_gets_its_gateway_retry_count_per_option() {
    assert_eq!(ProviderRetry::StopAtOnce.rcli_retries(), 0);
    assert_eq!(
        ProviderRetry::RetryBriefly.rcli_retries(),
        RCLI_BRIEF_RETRIES
    );
    assert_eq!(ProviderRetry::HarnessDefault.rcli_retries(), 0);
    assert_eq!(RCLI_BRIEF_RETRIES, 2);
}

/// DEC-141 parity: every harness has a line for every option, naming the harness, and the
/// owner's view lists them all in the setting's order with the chosen option.
#[test]
fn every_harness_has_a_line_for_every_option_in_the_owner_view() {
    for choice in ProviderRetry::ALL {
        for harness in HarnessKind::ALL {
            let line = choice.harness_line(harness);
            let name = match harness {
                HarnessKind::ClaudeCode => "Claude Code",
                HarnessKind::Codex => "Codex",
                HarnessKind::Rcli => "RCLI",
            };
            assert!(
                line.starts_with(&format!("{name}: ")),
                "{choice:?} {harness:?} {line}"
            );
            assert!(line.ends_with('.') && !line.contains('\u{2014}'), "{line}");
        }
    }
    let shown = view(ProviderRetry::StopAtOnce);
    assert_eq!(shown.choice, ProviderRetry::StopAtOnce);
    let order: Vec<(ProviderRetry, HarnessKind)> = shown
        .notes
        .iter()
        .map(|note| (note.option, note.harness))
        .collect();
    let expected: Vec<(ProviderRetry, HarnessKind)> = ProviderRetry::ALL
        .into_iter()
        .flat_map(|choice| {
            HarnessKind::ALL
                .into_iter()
                .map(move |harness| (choice, harness))
        })
        .collect();
    assert_eq!(order, expected);
    assert!(
        shown
            .notes
            .iter()
            .all(|note| note.line == note.option.harness_line(note.harness))
    );
    assert_eq!(
        serde_json::to_value(ProviderRetry::ALL).unwrap(),
        serde_json::json!(["stop_at_once", "retry_briefly", "harness_default"])
    );
}
