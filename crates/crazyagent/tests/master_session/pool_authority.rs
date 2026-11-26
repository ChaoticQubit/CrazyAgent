use super::*;

#[tokio::test]
async fn master_worker_only_or_wrong_project_pool_cannot_authorize_a_leader() {
    for catalog in [
        CATALOG.replace("roles = [\"project_leader\"]", "roles = [\"worker\"]"),
        CATALOG.replace("projects = [\"p\", \"q\"]", "projects = [\"q\"]"),
    ] {
        let f = Fixture::start(&catalog).await;
        let owner = f.owner(OwnerClientKind::Tui).await;
        f.create(&owner, "p").await;
        let exact = f
            .open_request(&owner, "denied-open", "p", selection(MasterBackend::Rcli))
            .await;
        rejected(owner.request(exact).await.unwrap());
        assert_eq!(f.session(&owner, "p").await, None);
        assert!(f.history(&owner, "p").await.entries.is_empty());
        f.stop().await;
    }
}
