use super::*;
use crate::engine::owner::AccountSecret;
use crate::github::GitHubMethod;

#[tokio::test]
async fn github_selector_reports_signed_out_and_refuses_work_before_sign_in() {
    run_case(false, |context| async move {
        let (client, _) =
            OwnerClient::discover(&context.state, OwnerClientKind::Tui, Duration::from_secs(2))
                .await
                .unwrap();
        let status = client.github_status().await.unwrap();
        assert!(!status.signed_in);
        assert_eq!(status.login, None);
        assert_eq!(status.method, GitHubMethod::None);
        assert!(!status.waiting);
        assert_eq!(status.problem, None);
        assert_eq!(
            client.github_repos(1).await.unwrap_err().0,
            "github_not_signed_in"
        );
        assert_eq!(
            client
                .clone_repo("octo-cat/tool".into())
                .await
                .unwrap_err()
                .0,
            "projects_home_not_set"
        );
        let token = AccountSecret::new(b"not a token".to_vec()).unwrap();
        assert_eq!(
            client.github_token(token).await.unwrap_err().0,
            "github_token_invalid"
        );
        assert!(!client.github_sign_out().await.unwrap().signed_in);
        assert!(!context.state.join("github.json").exists());
    })
    .await;
}
