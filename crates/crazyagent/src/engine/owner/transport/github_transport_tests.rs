use super::*;
use crate::engine::owner::AccountSecret;
use crate::github::GitHubMethod;

#[test]
fn a_publish_start_has_time_for_the_agent_that_drafts_its_gitignore() {
    let start = crate::engine::owner::transport::github::Action::PublishStart {
        project: crate::protocol::ProjectId::new("alpha").unwrap(),
        plan: crate::github::GitHubPublishPlan {
            owner: "octo-cat".into(),
            name: "tool".into(),
            description: String::new(),
            private: true,
            docs: false,
        },
    };
    assert!(start.time() > crate::instruction_cleanup::WALL_TIME + Duration::from_secs(120));
}

#[tokio::test]
async fn github_selector_reports_signed_out_and_refuses_work_before_sign_in() {
    run_case(false, |context| async move {
        #[cfg(target_os = "macos")]
        let _keychain = {
            let mut keychain = crate::credentials::test_keychain::TestKeychain::temporary();
            keychain.serve(&crate::github::namespace(&context.state));
            keychain
        };
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
            client.github_owners().await.unwrap_err().0,
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

#[cfg(target_os = "macos")]
mod slow_remote {
    use super::*;
    use crate::client_controller::OwnerController;
    use crate::credentials::test_keychain::TestKeychain;
    use crate::credentials::{SecretString, Store, platform_store, reference};
    use crate::engine::owner::{OwnerResponse, OwnerResult};
    use crate::github::test_server::{Answer, Server, base64};
    use crate::github::{Endpoints, GitHub, namespace, use_loopback};
    use crate::projects_home::FoundState;
    use crate::protocol::CredentialRef;
    use std::process::Command;
    use std::sync::Mutex as StdMutex;
    use std::sync::mpsc;
    use std::time::Instant;

    const SLOW: Duration = Duration::from_millis(2500);

    struct Forget {
        root: PathBuf,
        store: Arc<dyn Store>,
        item: CredentialRef,
    }

    impl Drop for Forget {
        fn drop(&mut self) {
            let _ = self.store.delete(&self.item);
            use_loopback(&self.root, None);
        }
    }

    fn git(args: &[&str], directory: &Path) {
        let status = crate::spawn_gate::status(
            Command::new("/usr/bin/git")
                .args(args)
                .current_dir(directory)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", directory)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.invalid"),
        )
        .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn bare(work: &Path, bare: &Path) {
        std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
        git(
            &[
                "clone",
                "--quiet",
                "--bare",
                work.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
            work,
        );
        git(&["update-server-info"], bare);
    }

    async fn set_home(controller: &mut OwnerController, home: &Path) {
        let sent = controller
            .set_projects_home(home.to_str().unwrap().into(), 1)
            .await
            .unwrap();
        if matches!(sent, OwnerResponse::OutcomeUnknown { .. }) {
            let checked = controller.reconcile_pending().await.unwrap();
            assert!(
                matches!(
                    &checked,
                    OwnerResponse::Committed { operation } if operation.result == OwnerResult::Applied
                ),
                "{checked:?}"
            );
            return;
        }
        assert!(
            matches!(
                &sent,
                OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied
            ),
            "{sent:?}"
        );
    }

    fn names(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn a_github_clone_and_repo_list_slower_than_the_owner_wait_still_answer() {
        run_case(false, |context| async move {
            let source = tempfile::tempdir().unwrap();
            let work = source.path().join("work");
            std::fs::create_dir(&work).unwrap();
            git(&["init", "--quiet", "--initial-branch=main"], &work);
            std::fs::write(work.join("README.md"), "hello\n").unwrap();
            git(&["add", "README.md"], &work);
            git(&["commit", "--quiet", "-m", "first"], &work);
            bare(&work, &source.path().join("octo-cat").join("tool.git"));
            let broken = source.path().join("octo-cat").join("broken.git");
            bare(&work, &broken);
            std::fs::remove_dir_all(broken.join("objects")).unwrap();

            let served = source.path().to_path_buf();
            let (stalled, stall) = mpsc::channel();
            let stalled = StdMutex::new(Some(stalled));
            let server = Server::start(move |seen, _| {
                if seen.path == "/user" {
                    return Answer::json(200, serde_json::json!({ "login": "octo-cat" }));
                }
                if seen.path == "/user/memberships/orgs" {
                    return Answer::json(
                        200,
                        serde_json::json!([{
                            "state": "active",
                            "role": "admin",
                            "organization": { "login": "acme-labs" }
                        }]),
                    );
                }
                if seen.path == "/user/repos" {
                    std::thread::sleep(SLOW);
                    return Answer::json(
                        200,
                        serde_json::json!([{
                            "full_name": "octo-cat/tool",
                            "private": false,
                            "default_branch": "main",
                            "size": 1
                        }]),
                    );
                }
                let relative = seen.path.trim_start_matches('/');
                assert!(!relative.contains(".."));
                if relative.starts_with("octo-cat/tool.git/objects/")
                    && !relative.starts_with("octo-cat/tool.git/objects/info/")
                    && let Some(sender) = stalled.lock().unwrap().take()
                {
                    let _ = sender.send(());
                    std::thread::sleep(SLOW);
                }
                match std::fs::read(served.join(relative)) {
                    Ok(bytes) => Answer::bytes(200, bytes),
                    Err(_) => Answer::bytes(404, Vec::new()),
                }
            });

            let root = context.state.clone();
            let space = namespace(&root);
            let mut keychain = TestKeychain::temporary();
            keychain.serve(&space);
            let store = platform_store(&space)
                .expect("UNAVAILABLE: the macOS Keychain of the signed-in user");
            let _forget = Forget {
                root: root.clone(),
                store: Arc::clone(&store),
                item: reference(&format!("{space}.github"), "token").unwrap(),
            };
            let token = SecretString::new(b"gho_slowremote0123456789".to_vec()).unwrap();
            GitHub::new(&root, space, store, Endpoints::loopback(server.port))
                .use_token(token)
                .unwrap();
            use_loopback(&root, Some(server.port));

            let home = tempfile::Builder::new()
                .prefix("ca-gh-home-")
                .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
                .unwrap();
            std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let home = std::fs::canonicalize(home.path()).unwrap();
            let mut controller =
                OwnerController::attach(&root, OwnerClientKind::Tui, Duration::from_secs(10))
                    .await
                    .unwrap();
            set_home(&mut controller, &home).await;
            controller.detach();

            let (client, _) =
                OwnerClient::discover(&root, OwnerClientKind::Tui, Duration::from_secs(2))
                    .await
                    .unwrap();
            let watched = home.clone();
            let observe = async move {
                tokio::task::spawn_blocking(move || {
                    stall.recv_timeout(Duration::from_secs(20)).unwrap();
                    names(&watched)
                })
                .await
                .unwrap()
            };
            let (cloned, during) = tokio::join!(client.clone_repo("octo-cat/tool".into()), observe);
            let receipt = cloned.unwrap_or_else(|error| {
                panic!(
                    "the clone answered {} while the projects home held {during:?}",
                    error.0
                )
            });
            let project = home.join("tool");
            assert_eq!(receipt.full_name, "octo-cat/tool");
            assert_eq!(receipt.path, project.to_string_lossy());
            assert_eq!(
                std::fs::read_to_string(project.join("README.md")).unwrap(),
                "hello\n"
            );
            assert!(
                !during.is_empty() && during.iter().all(|name| name.starts_with('.')),
                "mid-clone the projects home held {during:?}"
            );
            assert_eq!(names(&home), ["tool"]);

            assert_eq!(
                client
                    .clone_repo("octo-cat/broken".into())
                    .await
                    .unwrap_err()
                    .0,
                "github_clone_failed"
            );
            assert_eq!(
                names(&home),
                ["tool"],
                "a failed clone leaves nothing behind"
            );
            assert_eq!(
                client
                    .clone_repo("octo-cat/tool".into())
                    .await
                    .unwrap_err()
                    .0,
                "clone_destination_exists"
            );

            let started = Instant::now();
            let page = client
                .github_repos(1)
                .await
                .unwrap_or_else(|error| panic!("the repo list answered {}", error.0));
            assert!(started.elapsed() >= SLOW);
            assert_eq!(page.repos.len(), 1);
            assert_eq!(page.repos[0].full_name, "octo-cat/tool");
            assert_eq!(page.next, None);
            let owners = client
                .github_owners()
                .await
                .unwrap_or_else(|error| panic!("the owner list answered {}", error.0));
            let logins: Vec<&str> = owners
                .owners
                .iter()
                .map(|owner| owner.login.as_str())
                .collect();
            assert_eq!(logins, ["octo-cat", "acme-labs"]);
            assert!(owners.owners[0].personal);
            assert!(!client.github_sign_out().await.unwrap().signed_in);
        })
        .await;
    }

    fn holds(folder: &Path, needle: &[u8]) -> bool {
        let Ok(entries) = std::fs::read_dir(folder) else {
            return false;
        };
        entries.flatten().any(|entry| {
            let Ok(kind) = entry.file_type() else {
                return false;
            };
            if kind.is_dir() {
                holds(&entry.path(), needle)
            } else if kind.is_file() {
                std::fs::read(entry.path())
                    .is_ok_and(|bytes| bytes.windows(needle.len()).any(|window| window == needle))
            } else {
                false
            }
        })
    }

    #[tokio::test]
    async fn an_owner_token_sign_in_lets_the_engine_clone_into_the_home_with_no_token_on_disk() {
        run_case(false, |context| async move {
            let source = tempfile::tempdir().unwrap();
            let work = source.path().join("work");
            std::fs::create_dir(&work).unwrap();
            git(&["init", "--quiet", "--initial-branch=main"], &work);
            std::fs::write(work.join("README.md"), "hello\n").unwrap();
            git(&["add", "README.md"], &work);
            git(&["commit", "--quiet", "-m", "first"], &work);
            bare(&work, &source.path().join("octo-cat").join("tool.git"));

            let token = "gho_engineclone0123456789";
            let bearer = format!("Bearer {token}");
            let basic = format!(
                "Basic {}",
                base64(format!("x-access-token:{token}").as_bytes())
            );
            let expected = basic.clone();
            let served = source.path().to_path_buf();
            let server = Server::start(move |seen, _| {
                if seen.path == "/user" {
                    return if seen.header("authorization") == Some(bearer.as_str()) {
                        Answer::json(200, serde_json::json!({ "login": "octo-cat" }))
                    } else {
                        Answer::json(401, serde_json::json!({ "message": "Bad credentials" }))
                    };
                }
                if seen.header("authorization") != Some(basic.as_str()) {
                    let mut answer = Answer::bytes(401, Vec::new());
                    answer
                        .headers
                        .push(("WWW-Authenticate".into(), "Basic realm=\"git\"".into()));
                    return answer;
                }
                let relative = seen.path.trim_start_matches('/');
                assert!(!relative.contains(".."));
                match std::fs::read(served.join(relative)) {
                    Ok(bytes) => Answer::bytes(200, bytes),
                    Err(_) => Answer::bytes(404, Vec::new()),
                }
            });

            let root = context.state.clone();
            let space = namespace(&root);
            let mut keychain = TestKeychain::temporary();
            keychain.serve(&space);
            let store = platform_store(&space)
                .expect("UNAVAILABLE: the macOS Keychain of the signed-in user");
            let kept = Forget {
                root: root.clone(),
                store: Arc::clone(&store),
                item: reference(&format!("{space}.github"), "token").unwrap(),
            };
            use_loopback(&root, Some(server.port));

            let home = tempfile::Builder::new()
                .prefix("ca-gh-home-")
                .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
                .unwrap();
            std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let home = std::fs::canonicalize(home.path()).unwrap();
            let mut controller =
                OwnerController::attach(&root, OwnerClientKind::Tui, Duration::from_secs(10))
                    .await
                    .unwrap();
            set_home(&mut controller, &home).await;
            controller.detach();

            let (client, _) =
                OwnerClient::discover(&root, OwnerClientKind::Tui, Duration::from_secs(2))
                    .await
                    .unwrap();
            let status = client
                .github_token(AccountSecret::new(token.as_bytes().to_vec()).unwrap())
                .await
                .unwrap();
            assert!(status.signed_in);
            assert_eq!(status.login.as_deref(), Some("octo-cat"));
            assert_eq!(status.method, GitHubMethod::Token);
            assert!(
                store.get(&kept.item).is_ok(),
                "the engine keeps the token in the credential store"
            );

            let receipt = client
                .clone_repo("octo-cat/tool".into())
                .await
                .unwrap_or_else(|error| panic!("the engine clone answered {}", error.0));
            let project = home.join("tool");
            assert_eq!(receipt.full_name, "octo-cat/tool");
            assert_eq!(receipt.path, project.to_string_lossy());
            assert_eq!(
                std::fs::read_to_string(project.join("README.md")).unwrap(),
                "hello\n"
            );
            let config = std::fs::read_to_string(project.join(".git").join("config")).unwrap();
            assert!(
                config.contains(&format!(
                    "url = http://127.0.0.1:{}/octo-cat/tool.git",
                    server.port
                )),
                "{config}"
            );
            for kept_text in [token, "x-access-token", "@127.0.0.1"] {
                assert!(!config.contains(kept_text), "{kept_text} in {config}");
            }
            assert!(
                !holds(&project, token.as_bytes()),
                "no file in the clone holds the token"
            );
            assert!(
                server
                    .seen()
                    .iter()
                    .any(|seen| seen.header("authorization") == Some(expected.as_str())),
                "git sent the kept token through the engine's credential helper"
            );

            let mut controller =
                OwnerController::attach(&root, OwnerClientKind::Tui, Duration::from_secs(10))
                    .await
                    .unwrap();
            let deadline = Instant::now() + Duration::from_secs(20);
            let entry = loop {
                let found = controller.found_projects().await.unwrap();
                if let Some(entry) = found.into_iter().find(|entry| entry.name == "tool") {
                    break entry;
                }
                assert!(
                    Instant::now() < deadline,
                    "the clone must show on Home as Found"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            };
            assert_eq!(entry.state, FoundState::Found);
            assert!(entry.git && entry.project_id.is_none(), "{entry:?}");
            controller.detach();

            assert!(!client.github_sign_out().await.unwrap().signed_in);
            assert_eq!(
                store.get(&kept.item).unwrap_err().0,
                "credential_missing",
                "signing out deletes the kept token"
            );
            assert!(
                !holds(&root, token.as_bytes()),
                "the engine root keeps only a reference to the token"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_project_publish_runs_over_the_owner_socket_by_its_project_id() {
        use crate::github::{GitHubPublishPlan, GitHubPublishStep};
        use crate::protocol::ProjectId;
        run_case(false, |context| async move {
            let token = "gho_enginepublish0123456789";
            let bearer = format!("Bearer {token}");
            let server = Server::start(move |seen, _| {
                if seen.header("authorization") != Some(bearer.as_str()) {
                    return Answer::json(401, serde_json::json!({ "message": "Bad credentials" }));
                }
                match (seen.method.as_str(), seen.path.as_str()) {
                    ("GET", "/user") => Answer::json(
                        200,
                        serde_json::json!({ "login": "octo-cat", "id": 583_231 }),
                    ),
                    ("POST", "/user/repos") => Answer::json(
                        403,
                        serde_json::json!({ "message": "Resource not accessible by integration" }),
                    ),
                    _ => Answer::json(404, serde_json::json!({ "message": "Not Found" })),
                }
            });
            let root = context.state.clone();
            let space = namespace(&root);
            let mut keychain = TestKeychain::temporary();
            keychain.serve(&space);
            let store = platform_store(&space)
                .expect("UNAVAILABLE: the macOS Keychain of the signed-in user");
            let _forget = Forget {
                root: root.clone(),
                store: Arc::clone(&store),
                item: reference(&format!("{space}.github"), "token").unwrap(),
            };
            GitHub::new(&root, space, store, Endpoints::loopback(server.port))
                .use_token(SecretString::new(token.as_bytes().to_vec()).unwrap())
                .unwrap();
            use_loopback(&root, Some(server.port));

            let folder = tempfile::Builder::new()
                .prefix("ca-gh-publish-")
                .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
                .unwrap();
            let project = std::fs::canonicalize(folder.path()).unwrap();
            std::fs::write(project.join("main.rs"), "fn main() {}\n").unwrap();
            std::fs::write(
                project.join("keys.txt"),
                "token ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8\n",
            )
            .unwrap();
            {
                let db = crate::landing::open(&root).unwrap();
                crate::ide::workspace::tests::add_project(&db, "alpha", &project);
            }
            let (client, _) =
                OwnerClient::discover(&root, OwnerClientKind::Tui, Duration::from_secs(2))
                    .await
                    .unwrap();
            let plan = GitHubPublishPlan {
                owner: "octo-cat".into(),
                name: "tool".into(),
                description: String::new(),
                private: true,
                docs: false,
            };
            let alpha = ProjectId::new("alpha").unwrap();
            let beta = ProjectId::new("beta").unwrap();
            assert_eq!(
                client
                    .github_publish_status(beta.clone())
                    .await
                    .unwrap_err()
                    .0,
                "github_publish_project_unknown"
            );
            assert_eq!(
                client
                    .github_publish_start(beta, plan.clone())
                    .await
                    .unwrap_err()
                    .0,
                "github_publish_project_unknown"
            );
            assert_eq!(
                client.github_publish_status(alpha.clone()).await.unwrap(),
                None
            );
            let started = client
                .github_publish_start(alpha.clone(), plan.clone())
                .await
                .unwrap();
            assert_eq!(started.project, project);
            assert!(
                matches!(
                    &started.step,
                    GitHubPublishStep::Lines {
                        draft: crate::github::GitHubIgnoreDraft::Baseline { reason },
                        ..
                    } if reason.starts_with("instructions_cleanup_")
                ),
                "{started:?}"
            );
            let lines = vec![".env*".to_owned()];
            let blocked = client
                .github_publish_approve(alpha.clone(), lines)
                .await
                .unwrap();
            assert!(
                matches!(blocked.step, GitHubPublishStep::Blocked { .. }),
                "{blocked:?}"
            );
            assert_eq!(
                client.github_publish_recheck(alpha.clone()).await.unwrap(),
                blocked
            );
            let stopped = client
                .github_publish_exclude(alpha.clone(), "keys.txt".into())
                .await
                .unwrap();
            assert!(
                matches!(stopped.step, GitHubPublishStep::Creating { .. }),
                "{stopped:?}"
            );
            assert_eq!(
                stopped.stopped.as_deref(),
                Some("github_repo_create_refused")
            );
            assert_eq!(
                client.github_publish_status(alpha.clone()).await.unwrap(),
                Some(stopped.clone())
            );
            assert_eq!(
                client.github_publish_continue(alpha.clone()).await.unwrap(),
                stopped
            );
            assert_eq!(
                client
                    .github_publish_recheck(alpha.clone())
                    .await
                    .unwrap_err()
                    .0,
                "github_publish_step_invalid"
            );
            let creates = server
                .seen()
                .iter()
                .filter(|seen| seen.method == "POST" && seen.path == "/user/repos")
                .count();
            assert_eq!(creates, 2);
            assert_eq!(
                client
                    .github_publish_discard(alpha.clone())
                    .await
                    .map_err(|error| error.0),
                Ok(None)
            );
            assert_eq!(
                client.github_publish_status(alpha.clone()).await.unwrap(),
                None
            );
            assert!(!holds(&root, token.as_bytes()));
            assert!(!holds(&project, token.as_bytes()));
        })
        .await;
    }
}
