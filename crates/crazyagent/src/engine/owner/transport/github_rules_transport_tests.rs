use super::*;
use crate::github::GitHubRule;
use crate::protocol::ProjectId;
use std::collections::BTreeSet;

#[test]
fn a_rules_read_or_switch_has_time_for_two_reads_and_a_change() {
    let project = ProjectId::new("alpha").unwrap();
    for action in [
        crate::engine::owner::transport::github::Action::Rules {
            project: project.clone(),
        },
        crate::engine::owner::transport::github::Action::RuleSet {
            project,
            rule: GitHubRule::WikiOff,
            on: false,
        },
    ] {
        assert_eq!(action.time(), Duration::from_secs(300));
    }
}

#[tokio::test]
async fn rule_defaults_and_repository_rules_run_over_the_owner_socket_by_project_id() {
    run_case(false, |context| async move {
        #[cfg(target_os = "macos")]
        let _keychain = {
            let mut keychain = crate::credentials::test_keychain::TestKeychain::temporary();
            keychain.serve(&crate::github::namespace(&context.state));
            keychain
        };
        let root = context.state.clone();
        let (client, _) =
            OwnerClient::discover(&root, OwnerClientKind::Tui, Duration::from_secs(2))
                .await
                .unwrap();
        assert_eq!(
            client.github_rule_defaults().await.map_err(|error| error.0),
            Ok(GitHubRule::defaults())
        );
        let mut chosen: BTreeSet<GitHubRule> = GitHubRule::defaults();
        chosen.insert(GitHubRule::SignedCommits);
        chosen.remove(&GitHubRule::ActionsReadOnlyToken);
        assert_eq!(
            client
                .github_rule_defaults_set(chosen.clone())
                .await
                .map_err(|error| error.0),
            Ok(chosen.clone())
        );
        assert_eq!(
            client.github_rule_defaults().await.map_err(|error| error.0),
            Ok(chosen)
        );
        let folder = tempfile::tempdir().unwrap();
        let project = std::fs::canonicalize(folder.path()).unwrap();
        {
            let db = crate::landing::open(&root).unwrap();
            crate::ide::workspace::tests::add_project(&db, "alpha", &project);
        }
        let alpha = ProjectId::new("alpha").unwrap();
        let beta = ProjectId::new("beta").unwrap();
        assert_eq!(
            client
                .github_rules(beta.clone())
                .await
                .map(|_| ())
                .map_err(|error| error.0),
            Err("github_publish_project_unknown".to_owned())
        );
        assert_eq!(
            client
                .github_rule_set(beta, GitHubRule::WikiOff, false)
                .await
                .map(|_| ())
                .map_err(|error| error.0),
            Err("github_publish_project_unknown".to_owned())
        );
        assert_eq!(
            client
                .github_rules(alpha.clone())
                .await
                .map(|_| ())
                .map_err(|error| error.0),
            Err("github_rules_not_published".to_owned())
        );
        assert_eq!(
            client
                .github_rule_set(alpha, GitHubRule::WikiOff, false)
                .await
                .map(|_| ())
                .map_err(|error| error.0),
            Err("github_rules_not_published".to_owned())
        );
    })
    .await;
}
