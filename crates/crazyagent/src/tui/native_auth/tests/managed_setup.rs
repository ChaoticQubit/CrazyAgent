use super::*;
use crate::config::{ModelAccess, ModelAuthentication, ModelEndpoint};
use std::collections::BTreeSet;

async fn empty_settings(case: &Case) -> Ui {
    std::fs::write(&case.global, "timeout_ms = 1000\n").unwrap();
    case.restart().await;
    let ui = Ui::new(case).await;
    assert_eq!(saved_catalog(case), ModelAccess::default());
    assert_no_auth_start(case);
    ui
}

fn saved_catalog(case: &Case) -> ModelAccess {
    crate::config::resolve_config(&case.global, &case.project)
        .unwrap()
        .settings
        .model_access
        .unwrap_or_default()
}

fn owner_commits(case: &Case) -> i64 {
    case.observer()
        .query_row(
            "SELECT count(*) FROM owner_operations WHERE origin='native_owner'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn assert_no_auth_start(case: &Case) {
    let count: i64 = case
        .observer()
        .query_row("SELECT count(*) FROM native_auth_operations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        count, 0,
        "setup and inspection must not start native authentication"
    );
}

fn assert_managed_fields(screen: &str) {
    for field in [
        "Account reference",
        "Connection ID",
        "Harness",
        "Protocol",
        "Credential/profile reference",
    ] {
        assert!(!screen.contains(field), "managed setup exposed {field}");
    }
    assert!(!screen.contains("Claude official sign-in"));
    assert!(!screen.contains("AuthenticatedObserved"));
}

fn choose_codex(ui: &mut Ui, label: &str) {
    ui.key(KeyCode::Char('a'));
    let provider = views::body(&ui.app);
    assert!(provider.contains("Sign in to a provider"), "{provider}");
    assert!(provider.contains("Codex"), "{provider}");
    assert!(provider.contains("Advanced"), "{provider}");
    assert_managed_fields(&provider);
    ui.key(KeyCode::Enter);
    let label_form = views::body(&ui.app);
    assert!(label_form.contains("Account label"), "{label_form}");
    assert!(
        label_form.to_ascii_lowercase().contains("optional"),
        "{label_form}"
    );
    assert_managed_fields(&label_form);
    for character in label.chars() {
        ui.key(KeyCode::Char(character));
    }
}

fn assert_generated_catalog(catalog: &ModelAccess, count: usize) {
    assert_eq!(catalog.accounts.len(), count);
    assert_eq!(catalog.connections.len(), count);
    assert_eq!(catalog.pools.len(), count);
    let mut identifiers = BTreeSet::new();
    for connection in &catalog.connections {
        assert_eq!(connection.harness.as_str(), "codex");
        assert_eq!(connection.protocol.as_str(), "native");
        assert!(matches!(connection.endpoint, ModelEndpoint::Native {}));
        assert!(!connection.provider.as_str().is_empty());
        assert!(!connection.model.id.is_empty());
        assert!(!connection.model.author.as_str().is_empty());
        assert!(catalog.accounts.contains(&connection.account_ref));
        let ModelAuthentication::NativeManaged { profile_ref } = &connection.auth else {
            panic!("managed setup must persist an actual managed native connection");
        };
        assert!(identifiers.insert(connection.id.clone()));
        assert!(identifiers.insert(connection.account_ref.clone()));
        assert!(identifiers.insert(profile_ref.clone()));
        assert_ne!(connection.id.as_str(), "Work account 1021");
        assert_ne!(connection.account_ref.as_str(), "Work account 1021");
        let pools: Vec<_> = catalog
            .pools
            .iter()
            .filter(|pool| pool.members.contains(&connection.id))
            .collect();
        assert_eq!(pools.len(), 1);
        assert_eq!(pools[0].members, vec![connection.id.clone()]);
        assert!(!pools[0].fallback_allowed);
        assert!(identifiers.insert(pools[0].id.clone()));
    }
    assert_eq!(identifiers.len(), count * 4);
}

#[tokio::test]
async fn managed_setup_codex_label_commits_generated_settings_before_auth_method_choice() {
    run(|case| async move {
        let mut ui = empty_settings(&case).await;
        let commits = owner_commits(&case);
        choose_codex(&mut ui, "Work account 1021");
        assert!(views::body(&ui.app).contains("Work account 1021"));
        assert_eq!(saved_catalog(&case), ModelAccess::default());
        ui.key(KeyCode::Enter);
        ui.key(KeyCode::Enter);
        ui.drain().await;
        let saved = saved_catalog(&case);
        assert_generated_catalog(&saved, 1);
        assert_eq!(owner_commits(&case), commits + 3);
        let (basis, actual) = case.selected(saved.connections[0].id.as_str()).await;
        assert!(basis.writable);
        assert_eq!(actual, saved.connections[0]);
        let auth = views::body(&ui.app);
        assert!(auth.contains("Codex browser sign-in"), "{auth}");
        assert!(auth.contains("Codex device-code sign-in"), "{auth}");
        assert!(!auth.contains("Claude official sign-in"));
        assert_no_auth_start(&case);
        ui.key(KeyCode::Esc);
        ui.key(KeyCode::Char('3'));
        ui.drain().await;
        let pools = views::body(&ui.app);
        assert!(pools.contains(saved.pools[0].id.as_str()), "{pools}");
        ui.key(KeyCode::Char('1'));
        ui.drain().await;
        let accounts = views::body(&ui.app);
        assert!(accounts.contains(saved.accounts[0].as_str()), "{accounts}");
        assert_eq!(saved_catalog(&case), saved);
        assert_no_auth_start(&case);
    })
    .await;
}

#[tokio::test]
async fn managed_setup_cancel_is_nonmutating_and_second_unlabelled_account_is_distinct() {
    run(|case| async move {
        let mut ui = empty_settings(&case).await;
        let original = std::fs::read(&case.global).unwrap();
        let commits = owner_commits(&case);
        ui.key(KeyCode::Char('a'));
        assert!(views::body(&ui.app).contains("Sign in to a provider"));
        ui.key(KeyCode::Esc);
        assert_eq!(std::fs::read(&case.global).unwrap(), original);
        assert_eq!(owner_commits(&case), commits);
        assert_no_auth_start(&case);
        choose_codex(&mut ui, "");
        ui.key(KeyCode::Enter);
        ui.drain().await;
        let first = saved_catalog(&case);
        assert_generated_catalog(&first, 1);
        assert_no_auth_start(&case);
        ui.key(KeyCode::Esc);
        ui.key(KeyCode::Char('1'));
        ui.drain().await;
        choose_codex(&mut ui, "");
        ui.key(KeyCode::Enter);
        ui.drain().await;
        let second = saved_catalog(&case);
        assert_generated_catalog(&second, 2);
        assert!(second.accounts.contains(&first.accounts[0]));
        assert!(second.connections.contains(&first.connections[0]));
        assert!(second.pools.contains(&first.pools[0]));
        assert_eq!(owner_commits(&case), commits + 6);
        assert!(views::body(&ui.app).contains("Codex browser sign-in"));
        assert_no_auth_start(&case);
    })
    .await;
}
