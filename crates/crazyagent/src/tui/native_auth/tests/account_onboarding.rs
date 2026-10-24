use super::*;
use crate::config::{ModelAccess, ModelAuthentication, ModelEndpoint, ProviderQuotaPolicy};
use std::collections::BTreeSet;

async fn empty_accounts(case: &Case) -> Ui {
    std::fs::write(&case.global, "timeout_ms = 1000\n").unwrap();
    case.restart().await;
    let ui = Ui::new(case).await;
    assert_eq!(catalog(case), ModelAccess::default());
    assert_no_auth(case);
    ui
}

fn catalog(case: &Case) -> ModelAccess {
    crate::config::resolve_config(&case.global, &case.project)
        .unwrap()
        .settings
        .model_access
        .unwrap_or_default()
}

fn commits(case: &Case) -> i64 {
    case.observer()
        .query_row(
            "SELECT count(*) FROM owner_operations WHERE origin='native_owner'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn assert_no_auth(case: &Case) {
    let count: i64 = case
        .observer()
        .query_row("SELECT count(*) FROM native_auth_operations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        count, 0,
        "onboarding must not request native authentication"
    );
}

fn type_text(ui: &mut Ui, value: &str) {
    for character in value.chars() {
        ui.key(KeyCode::Char(character));
    }
}

fn assert_no_routing_form(ui: &Ui) {
    let body = views::body(&ui.app);
    for field in [
        "Account reference",
        "Connection ID",
        "Harness",
        "Protocol",
        "Credential/profile reference",
    ] {
        assert!(!body.contains(field), "normal onboarding exposed {field}");
    }
}

fn choose_local(ui: &mut Ui, label: &str) {
    ui.key(KeyCode::Char('a'));
    let providers = views::body(&ui.app);
    assert!(providers.contains("Sign in to a provider"), "{providers}");
    assert!(providers.contains("Codex"));
    assert!(providers.contains("Claude"));
    assert!(providers.contains("API key or compatible endpoint"));
    assert!(providers.contains("Local endpoint"));
    assert!(providers.contains("Advanced"));
    for _ in 0..3 {
        ui.key(KeyCode::Down);
    }
    assert!(views::body(&ui.app).contains("> Local endpoint"));
    ui.key(KeyCode::Enter);
    assert!(views::body(&ui.app).contains("Account label"));
    assert_no_routing_form(ui);
    type_text(ui, label);
    ui.key(KeyCode::Enter);
    assert!(views::body(&ui.app).contains("Endpoint URL"));
    assert_no_routing_form(ui);
}

async fn add_local(ui: &mut Ui, endpoint: &str, model: &str) {
    type_text(ui, endpoint);
    ui.key(KeyCode::Enter);
    assert!(views::body(&ui.app).contains("Model"));
    assert_no_routing_form(ui);
    type_text(ui, model);
    ui.key(KeyCode::Enter);
    ui.key(KeyCode::Enter);
    ui.drain().await;
    let profile = views::body(&ui.app);
    assert!(
        profile.contains("Press Enter to finish local endpoint setup"),
        "{profile}"
    );
    ui.key(KeyCode::Enter);
    ui.drain().await;
    let configured = views::body(&ui.app);
    assert!(
        configured.contains("ConfiguredOnly | generation 1"),
        "{configured}"
    );
    ui.key(KeyCode::Esc);
    let finished = views::body(&ui.app);
    assert!(finished.contains("Private setup closed"), "{finished}");
}

fn assert_local(case: &Case, saved: &ModelAccess, endpoint: &str, model: &str) {
    let connection = saved
        .connections
        .iter()
        .find(|connection| connection.model.id == model)
        .unwrap();
    assert!(saved.accounts.contains(&connection.account_ref));
    assert_eq!(connection.harness.as_str(), "rcli");
    assert!(!connection.protocol.as_str().is_empty());
    assert_eq!(
        connection.endpoint,
        ModelEndpoint::Http {
            url: endpoint.into()
        }
    );
    assert_eq!(connection.auth, ModelAuthentication::None {});
    assert_ne!(connection.id, connection.account_ref);
    let pools: Vec<_> = saved
        .pools
        .iter()
        .filter(|pool| pool.members == vec![connection.id.clone()])
        .collect();
    assert_eq!(pools.len(), 1);
    assert_ne!(pools[0].id, connection.id);
    assert_ne!(pools[0].id, connection.account_ref);
    assert!(!pools[0].fallback_allowed);
    assert_no_auth(case);
}

async fn page(case: &Case, collection: ModelAccessCollection) -> Vec<ModelAccessRecord> {
    let mut owner = case.controller().await;
    let actual = owner
        .read(OwnerRead::ModelAccessPage {
            collection,
            cursor: None,
            limit: 16,
        })
        .await
        .unwrap();
    let OwnerData::ModelAccessPage {
        collection: actual_collection,
        items,
        writable,
        next,
        ..
    } = actual.data
    else {
        panic!("actual owner page required");
    };
    assert_eq!(actual_collection, collection);
    assert!(writable);
    assert!(next.is_none());
    items
}

#[tokio::test]
async fn local_account_uses_meaningful_fields_and_committed_owner_records_without_auth() {
    run(|case| async move {
        let mut ui = empty_accounts(&case).await;
        let original = std::fs::read(&case.global).unwrap();
        let before = commits(&case);
        choose_local(&mut ui, "Cancelled local account");
        ui.key(KeyCode::Esc);
        assert_eq!(std::fs::read(&case.global).unwrap(), original);
        assert_eq!(commits(&case), before);
        assert_no_auth(&case);
        choose_local(&mut ui, "Local account 1033");
        assert_eq!(catalog(&case), ModelAccess::default());
        add_local(&mut ui, "http://127.0.0.1:9/v1", "local-model-1033").await;
        let saved = catalog(&case);
        assert_eq!(saved.accounts.len(), 1);
        assert_eq!(saved.connections.len(), 1);
        assert_eq!(saved.pools.len(), 1);
        assert_eq!(commits(&case), before + 3);
        assert_local(&case, &saved, "http://127.0.0.1:9/v1", "local-model-1033");
        let (basis, observed) = case.selected(saved.connections[0].id.as_str()).await;
        assert!(basis.writable);
        assert_eq!(observed, saved.connections[0]);
        let body = views::body(&ui.app);
        assert!(!body.contains("Codex browser sign-in"));
        assert!(!body.contains("AuthenticatedObserved"));
        ui.key(KeyCode::Char('3'));
        ui.drain().await;
        let pools = views::body(&ui.app);
        assert!(pools.contains(saved.pools[0].id.as_str()), "{pools}");
        assert_eq!(catalog(&case), saved);
        assert_no_auth(&case);
    })
    .await;
}

async fn create_pool(ui: &mut Ui, connections: &[ModelAccessRecord]) {
    ui.key(KeyCode::Char('3'));
    ui.drain().await;
    ui.key(KeyCode::Char('n'));
    assert!(views::body(&ui.app).contains("Pool label (optional)"));
    type_text(ui, "Shared local pool 1033");
    ui.key(KeyCode::Enter);
    ui.drain().await;
    let chooser = views::body(&ui.app);
    assert!(chooser.contains("Choose connections"));
    for connection in connections {
        assert!(chooser.contains(connection.id().as_str()));
    }
    ui.key(KeyCode::Char(' '));
    ui.key(KeyCode::Down);
    ui.key(KeyCode::Char(' '));
    ui.key(KeyCode::Enter);
    ui.key(KeyCode::Enter);
    ui.drain().await;
}

async fn edit_pool(ui: &mut Ui, case: &Case, id: &CredentialRef) {
    let refresh = key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &ui.app,
    )
    .unwrap();
    ui.app.reduce(refresh).unwrap();
    ui.drain().await;
    let pools = page(case, ModelAccessCollection::Pools).await;
    let index = pools.iter().position(|record| record.id() == id).unwrap();
    for _ in 0..index {
        ui.key(KeyCode::Down);
    }
    ui.key(KeyCode::Char('e'));
    ui.drain().await;
    assert!(views::body(&ui.app).contains("Choose connections"));
    ui.key(KeyCode::Char(' '));
    ui.key(KeyCode::Enter);
    ui.drain().await;
}

#[tokio::test]
async fn second_local_account_and_pool_membership_keep_actual_connection_and_pool_identities() {
    run(|case| async move {
        let mut ui = empty_accounts(&case).await;
        let before = commits(&case);
        choose_local(&mut ui, "First local account");
        add_local(&mut ui, "http://127.0.0.1:9/v1", "first-local-1033").await;
        let first = catalog(&case);
        ui.key(KeyCode::Char('1'));
        ui.drain().await;
        choose_local(&mut ui, "Second local account");
        add_local(&mut ui, "http://127.0.0.1:10/v1", "second-local-1033").await;
        let second = catalog(&case);
        assert_eq!(second.accounts.len(), 2);
        assert_eq!(second.connections.len(), 2);
        assert_eq!(second.pools.len(), 2);
        assert!(second.accounts.contains(&first.accounts[0]));
        assert!(second.connections.contains(&first.connections[0]));
        assert!(second.pools.contains(&first.pools[0]));
        assert_eq!(commits(&case), before + 6);
        let connections = page(&case, ModelAccessCollection::Connections).await;
        assert_eq!(connections.len(), 2);
        create_pool(&mut ui, &connections).await;
        let third = catalog(&case);
        assert_eq!(third.pools.len(), 3);
        let shared = third
            .pools
            .iter()
            .find(|pool| !second.pools.contains(pool))
            .unwrap();
        let members: BTreeSet<_> = connections
            .iter()
            .map(|record| record.id().clone())
            .collect();
        assert_eq!(
            shared.members.iter().cloned().collect::<BTreeSet<_>>(),
            members
        );
        assert!(!shared.fallback_allowed);
        assert_eq!(commits(&case), before + 7);
        edit_pool(&mut ui, &case, &shared.id).await;
        let final_catalog = catalog(&case);
        let changed = final_catalog
            .pools
            .iter()
            .find(|pool| pool.id == shared.id)
            .unwrap();
        assert_eq!(changed.members, vec![connections[1].id().clone()]);
        assert_eq!(final_catalog.accounts, second.accounts);
        assert_eq!(final_catalog.connections, second.connections);
        assert!(
            second
                .pools
                .iter()
                .all(|pool| final_catalog.pools.contains(pool))
        );
        assert_eq!(commits(&case), before + 8);
        let observed = page(&case, ModelAccessCollection::Pools).await;
        assert!(observed.contains(&ModelAccessRecord::Pool {
            pool: changed.clone()
        }));
        assert_no_auth(&case);
    })
    .await;
}

#[tokio::test]
async fn pool_setup_chooses_the_provider_quota_policy_and_edit_keeps_it() {
    run(|case| async move {
        let mut ui = empty_accounts(&case).await;
        choose_local(&mut ui, "Quota local account");
        add_local(&mut ui, "http://127.0.0.1:9/v1", "quota-local-1033").await;
        let before = catalog(&case);
        let connections = page(&case, ModelAccessCollection::Connections).await;
        assert_eq!(connections.len(), 1);
        ui.key(KeyCode::Char('3'));
        ui.drain().await;
        ui.key(KeyCode::Char('n'));
        type_text(&mut ui, "Strict quota pool");
        ui.key(KeyCode::Enter);
        ui.drain().await;
        let chooser = views::body(&ui.app);
        assert!(
            chooser.contains("Provider quota policy: bounded_attempt"),
            "{chooser}"
        );
        assert!(
            chooser.contains("Strict blocks unknown provider quota"),
            "{chooser}"
        );
        ui.key(KeyCode::Char(' '));
        ui.key(KeyCode::Char('p'));
        let strict = views::body(&ui.app);
        assert!(strict.contains("Provider quota policy: strict"), "{strict}");
        ui.key(KeyCode::Enter);
        ui.drain().await;
        let saved = catalog(&case);
        assert_eq!(saved.pools.len(), before.pools.len() + 1);
        let created = saved
            .pools
            .iter()
            .find(|pool| !before.pools.contains(pool))
            .unwrap()
            .clone();
        assert_eq!(created.provider_quota_policy, ProviderQuotaPolicy::Strict);
        assert_eq!(created.members, vec![connections[0].id().clone()]);
        let refresh = key_event(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &ui.app,
        )
        .unwrap();
        ui.app.reduce(refresh).unwrap();
        ui.drain().await;
        let pools = page(&case, ModelAccessCollection::Pools).await;
        let index = pools
            .iter()
            .position(|record| record.id() == &created.id)
            .unwrap();
        for _ in 0..index {
            ui.key(KeyCode::Down);
        }
        ui.key(KeyCode::Char('e'));
        ui.drain().await;
        let editor = views::body(&ui.app);
        assert!(editor.contains("Provider quota policy: strict"), "{editor}");
        ui.key(KeyCode::Char('p'));
        ui.key(KeyCode::Enter);
        ui.drain().await;
        let edited = catalog(&case);
        let changed = edited
            .pools
            .iter()
            .find(|pool| pool.id == created.id)
            .unwrap();
        assert_eq!(
            changed.provider_quota_policy,
            ProviderQuotaPolicy::BoundedAttempt
        );
        assert_eq!(changed.members, created.members);
        assert_eq!(changed.name, created.name);
        assert_eq!(edited.connections, saved.connections);
        assert_no_auth(&case);
    })
    .await;
}
