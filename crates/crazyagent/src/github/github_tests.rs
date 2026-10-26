use super::test_server::{Answer, Seen, Server, base64};
use super::*;
use crate::credentials::test_keychain::TestKeychain;
use crate::credentials::{Keychain, reference};
use std::os::unix::fs::OpenOptionsExt;
use std::process::Command;
use std::time::{Duration, Instant};

struct Scratch {
    root: tempfile::TempDir,
    namespace: String,
    store: Arc<Keychain>,
    _keychain: TestKeychain,
}

impl Scratch {
    fn new() -> Self {
        let mut bytes = [0_u8; 6];
        getrandom::fill(&mut bytes).unwrap();
        let suffix: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let namespace = format!("dev.crazyagent.test-{suffix}");
        let mut keychain = TestKeychain::temporary();
        keychain.serve(&namespace);
        Self {
            root: tempfile::tempdir().unwrap(),
            store: Arc::new(Keychain::new(&namespace).unwrap()),
            namespace,
            _keychain: keychain,
        }
    }

    fn account(&self, server: &Server) -> GitHub {
        GitHub::new(
            &self.root.path().canonicalize().unwrap(),
            self.namespace.clone(),
            Arc::clone(&self.store) as Arc<dyn Store>,
            Endpoints::loopback(server.port),
        )
    }

    fn state(&self) -> Option<String> {
        std::fs::read_to_string(self.root.path().join("github.json")).ok()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let item = reference(&format!("{}.github", self.namespace), "token").unwrap();
        let _ = self.store.delete(&item);
    }
}

fn token() -> String {
    let mut bytes = [0_u8; 18];
    getrandom::fill(&mut bytes).unwrap();
    let tail: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("gho_test{tail}")
}

fn secret(text: &str) -> SecretString {
    SecretString::new(text.as_bytes().to_vec()).unwrap()
}

fn bearer(seen: &Seen, token: &str) -> bool {
    seen.header("authorization") == Some(format!("Bearer {token}").as_str())
}

fn user(seen: &Seen, token: &str) -> Answer {
    if bearer(seen, token) {
        Answer::json(200, serde_json::json!({ "login": "octo-cat" }))
    } else {
        Answer::json(401, serde_json::json!({ "message": "Bad credentials" }))
    }
}

fn wait_for(account: &GitHub, done: impl Fn(&GitHubStatus) -> bool) -> GitHubStatus {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = account.status();
        if done(&status) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "status never settled: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
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

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

fn tree_holds(path: &Path, needle: &str) -> bool {
    std::fs::read_dir(path).unwrap().any(|entry| {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            tree_holds(&entry.path(), needle)
        } else {
            kind.is_file() && contains(&std::fs::read(entry.path()).unwrap(), needle)
        }
    })
}

#[test]
fn curl_command_keeps_url_headers_token_and_form_out_of_argv_and_env() {
    let secret_text = token();
    let token = secret(&secret_text);
    let request = http::Request {
        url: url::Url::parse("https://api.github.com/user?probe=1").unwrap(),
        accept: "application/json",
        token: Some(&token),
        form: Some(vec![("client_id", "Ov23-public"), ("scope", SCOPES)]),
        allow_http: false,
    };
    let (command, config) = request.command().unwrap();
    let argv: Vec<String> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let env: Vec<String> = command
        .get_envs()
        .map(|(name, value)| format!("{name:?}={value:?}"))
        .collect();
    for shown in argv.iter().chain(env.iter()) {
        assert!(!shown.contains(&secret_text));
        assert!(!shown.contains("api.github.com"));
        assert!(!shown.contains("Ov23-public"));
    }
    assert!(argv.contains(&"=https".to_owned()));
    assert!(argv.contains(&"--max-redirs".to_owned()));
    assert_eq!(env, vec![r#""PATH"=Some("/usr/bin:/bin")"#.to_owned()]);
    assert!(contains(
        &config,
        &format!("Authorization: Bearer {secret_text}")
    ));
    assert!(contains(&config, "client_id=Ov23-public"));
    assert!(contains(&config, "scope=repo+read%3Auser"));
    let plain = http::Request {
        url: url::Url::parse("http://api.github.com/user").unwrap(),
        accept: "application/json",
        token: None,
        form: None,
        allow_http: true,
    };
    assert_eq!(plain.command().err().unwrap().0, "github_request_invalid");
}

#[test]
fn device_sign_in_honors_pending_and_slow_down_then_keeps_only_a_reference() {
    let scratch = Scratch::new();
    let secret_text = token();
    let granted = secret_text.clone();
    let counter = Arc::new(Mutex::new(0_u32));
    let server = Server::start(move |seen, _| match seen.path.as_str() {
        "/login/device/code" => {
            assert_eq!(seen.method, "POST");
            let form = seen.form();
            assert_eq!(form.get("client_id").map(String::as_str), Some(CLIENT_ID));
            assert_eq!(form.get("scope").map(String::as_str), Some(SCOPES));
            let host = seen.header("host").unwrap().to_owned();
            Answer::json(
                200,
                serde_json::json!({
                    "device_code": "device-test-code",
                    "user_code": "WDJB-MJHT",
                    "verification_uri": format!("http://{host}/login/device"),
                    "expires_in": 900,
                    "interval": 1,
                }),
            )
        }
        "/login/oauth/access_token" => {
            let form = seen.form();
            assert_eq!(
                form.get("device_code").map(String::as_str),
                Some("device-test-code")
            );
            assert_eq!(
                form.get("grant_type").map(String::as_str),
                Some("urn:ietf:params:oauth:grant-type:device_code")
            );
            let mut count = counter.lock().unwrap();
            *count += 1;
            match *count {
                1 => Answer::json(200, serde_json::json!({ "error": "authorization_pending" })),
                2 => Answer::json(
                    200,
                    serde_json::json!({ "error": "slow_down", "interval": 3 }),
                ),
                _ => Answer::json(
                    200,
                    serde_json::json!({
                        "access_token": granted,
                        "token_type": "bearer",
                        "scope": "repo,read:user",
                    }),
                ),
            }
        }
        "/user" => user(seen, &granted),
        _ => Answer::json(404, serde_json::json!({})),
    });
    let account = scratch.account(&server);
    let code = account.sign_in().unwrap();
    assert_eq!(code.user_code, "WDJB-MJHT");
    assert_eq!(
        code.verification_uri,
        format!("http://127.0.0.1:{}/login/device", server.port)
    );
    assert_eq!(code.expires_s, 900);
    assert!(account.status().waiting);
    let status = wait_for(&account, |status| status.signed_in);
    assert_eq!(status.login.as_deref(), Some("octo-cat"));
    assert_eq!(status.method, GitHubMethod::Device);
    assert!(!status.waiting);
    assert_eq!(status.problem, None);
    let polls: Vec<Seen> = server
        .seen()
        .into_iter()
        .filter(|seen| seen.path == "/login/oauth/access_token")
        .collect();
    assert_eq!(polls.len(), 3);
    assert!(polls[2].at.duration_since(polls[1].at) >= Duration::from_secs(3));
    let state = scratch.state().unwrap();
    assert!(!state.contains(&secret_text));
    assert!(state.contains(&format!("{}.github:token", scratch.namespace)));
    let saved = state::load(&account.root).unwrap().unwrap();
    assert_eq!(
        scratch.store.get(&saved.credential).unwrap().expose(),
        secret_text
    );
    assert!(!account.sign_out().unwrap().signed_in);
    assert_eq!(
        scratch.store.get(&saved.credential).unwrap_err().0,
        "credential_missing"
    );
    assert_eq!(scratch.state(), None);
}

#[test]
fn denied_device_sign_in_reports_a_plain_problem_and_saves_nothing() {
    let scratch = Scratch::new();
    let server = Server::start(|seen, _| match seen.path.as_str() {
        "/login/device/code" => {
            let port = seen.header("host").unwrap().to_owned();
            Answer::json(
                200,
                serde_json::json!({
                    "device_code": "device-test-code",
                    "user_code": "ABCD-EFGH",
                    "verification_uri": format!("http://{port}/login/device"),
                    "expires_in": 900,
                    "interval": 1,
                }),
            )
        }
        "/login/oauth/access_token" => {
            Answer::json(200, serde_json::json!({ "error": "access_denied" }))
        }
        _ => Answer::json(404, serde_json::json!({})),
    });
    let account = scratch.account(&server);
    account.sign_in().unwrap();
    let status = wait_for(&account, |status| !status.waiting);
    assert!(!status.signed_in);
    assert_eq!(
        status.problem.as_deref(),
        Some("GitHub sign-in was denied.")
    );
    assert_eq!(scratch.state(), None);
}

#[test]
fn verification_page_on_another_host_is_refused() {
    let scratch = Scratch::new();
    let server = Server::start(|_, _| {
        Answer::json(
            200,
            serde_json::json!({
                "device_code": "device-test-code",
                "user_code": "ABCD-EFGH",
                "verification_uri": "https://github.com.example.invalid/login/device",
                "expires_in": 900,
                "interval": 1,
            }),
        )
    });
    let account = scratch.account(&server);
    assert_eq!(account.sign_in().unwrap_err().0, "github_reply_invalid");
    assert!(!account.status().waiting);
}

#[test]
fn empty_client_id_override_is_reported_as_not_configured() {
    let scratch = Scratch::new();
    let server = Server::start(|_, _| Answer::json(500, serde_json::json!({})));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(scratch.root.path().join("github.toml"))
        .unwrap();
    std::io::Write::write_all(&mut file, b"client_id = \"\"\n").unwrap();
    let account = scratch.account(&server);
    assert_eq!(
        account.sign_in().unwrap_err().0,
        "github_client_id_not_configured"
    );
    assert_eq!(
        account.status().problem.as_deref(),
        Some("GitHub sign-in is not set up yet: client id not configured.")
    );
    assert!(server.seen().is_empty());
}

#[test]
fn pasted_token_counts_only_once_github_accepts_it() {
    let scratch = Scratch::new();
    let accepted = token();
    let expected = accepted.clone();
    let server = Server::start(move |seen, _| user(seen, &expected));
    let account = scratch.account(&server);
    assert_eq!(
        account.use_token(secret(&token())).unwrap_err().0,
        "github_token_refused"
    );
    assert_eq!(scratch.state(), None);
    assert!(!account.status().signed_in);
    let status = account.use_token(secret(&accepted)).unwrap();
    assert!(status.signed_in);
    assert_eq!(status.method, GitHubMethod::Token);
    assert_eq!(account.status().login.as_deref(), Some("octo-cat"));
    assert!(!scratch.state().unwrap().contains(&accepted));
    for seen in server.seen() {
        assert_eq!(seen.header("x-github-api-version"), Some("2022-11-28"));
        assert_eq!(seen.header("user-agent"), Some("CrazyAgent"));
    }
}

#[test]
fn signed_in_gh_cli_token_is_reused_through_the_real_gh() {
    let scratch = Scratch::new();
    let cli_token = token();
    let expected = cli_token.clone();
    let server = Server::start(move |seen, _| user(seen, &expected));
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("gh");
    std::fs::create_dir(&config).unwrap();
    let gh = api::find_cli().expect("the GitHub CLI is installed for this test");
    let account = scratch.account(&server);
    let empty = account
        .use_cli_at(
            gh,
            &[
                ("HOME", home.path().as_os_str()),
                ("GH_CONFIG_DIR", config.as_os_str()),
            ],
        )
        .unwrap_err();
    assert_eq!(empty.0, "github_cli_not_signed_in");
    let status = account
        .use_cli_at(
            gh,
            &[
                ("HOME", home.path().as_os_str()),
                ("GH_CONFIG_DIR", config.as_os_str()),
                ("GH_TOKEN", OsStr::new(&cli_token)),
            ],
        )
        .unwrap();
    assert!(status.signed_in);
    assert_eq!(status.method, GitHubMethod::Cli);
    assert!(!scratch.state().unwrap().contains(&cli_token));
}

#[test]
fn repos_page_uses_the_stored_token_and_offers_the_next_page_when_full() {
    let scratch = Scratch::new();
    let secret_text = token();
    let expected = secret_text.clone();
    let server = Server::start(move |seen, _| match seen.path.as_str() {
        "/user" => user(seen, &expected),
        "/user/repos" if bearer(seen, &expected) => {
            let query = seen.query_pairs();
            let page: u32 = query.get("page").unwrap().parse().unwrap();
            let count = if page == 1 { 100 } else { 3 };
            let repos: Vec<serde_json::Value> = (0..count)
                .map(|index| {
                    serde_json::json!({
                        "full_name": format!("octo-cat/repo-{page}-{index}"),
                        "private": index % 2 == 0,
                        "default_branch": "main",
                        "size": 1024 + index,
                        "owner": { "login": "octo-cat" },
                    })
                })
                .collect();
            Answer::json(200, serde_json::Value::Array(repos))
        }
        _ => Answer::json(401, serde_json::json!({})),
    });
    let account = scratch.account(&server);
    assert_eq!(account.repos(1).unwrap_err().0, "github_not_signed_in");
    account.use_token(secret(&secret_text)).unwrap();
    let first = account.repos(1).unwrap();
    assert_eq!(first.repos.len(), 100);
    assert_eq!(first.next, Some(2));
    assert_eq!(first.repos[0].full_name, "octo-cat/repo-1-0");
    assert!(first.repos[0].private);
    assert_eq!(first.repos[1].size_kb, 1025);
    let second = account.repos(2).unwrap();
    assert_eq!(second.repos.len(), 3);
    assert_eq!(second.next, None);
    assert_eq!(account.repos(0).unwrap_err().0, "github_repo_page_invalid");
    let listed = server
        .seen()
        .into_iter()
        .find(|seen| seen.path == "/user/repos")
        .unwrap();
    let query = listed.query_pairs();
    assert_eq!(query.get("per_page").map(String::as_str), Some("100"));
    assert_eq!(query.get("sort").map(String::as_str), Some("pushed"));
}

fn served_repository(full_name: &str, secret_text: &str) -> (tempfile::TempDir, Server) {
    let source = tempfile::tempdir().unwrap();
    let work = source.path().join("work");
    std::fs::create_dir(&work).unwrap();
    git(&["init", "--quiet", "--initial-branch=main"], &work);
    std::fs::write(work.join("README.md"), "hello\n").unwrap();
    git(&["add", "README.md"], &work);
    git(&["commit", "--quiet", "-m", "first"], &work);
    let bare = source.path().join(format!("{full_name}.git"));
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    git(
        &[
            "clone",
            "--quiet",
            "--bare",
            work.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
        source.path(),
    );
    git(&["update-server-info"], &bare);
    let served = source.path().to_path_buf();
    let basic = format!(
        "Basic {}",
        base64(format!("x-access-token:{secret_text}").as_bytes())
    );
    let expected = secret_text.to_owned();
    let server = Server::start(move |seen, _| {
        if seen.path == "/user" {
            return user(seen, &expected);
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
    (source, server)
}

#[test]
fn clone_asks_the_store_helper_and_leaves_no_token_in_the_project() {
    let scratch = Scratch::new();
    let secret_text = token();
    let (_source, server) = served_repository("octo-cat/tool", &secret_text);
    let home = tempfile::tempdir().unwrap();
    let home = home.path().canonicalize().unwrap();
    let account = scratch.account(&server);
    assert_eq!(
        account.clone_repo(&home, "octo-cat/tool").unwrap_err().0,
        "github_not_signed_in"
    );
    account.use_token(secret(&secret_text)).unwrap();
    assert_eq!(
        account.clone_repo(&home, "../tool").unwrap_err().0,
        "github_repo_name_invalid"
    );
    let receipt = account.clone_repo(&home, "octo-cat/tool").unwrap();
    assert_eq!(receipt.full_name, "octo-cat/tool");
    let project = home.join("tool");
    assert_eq!(receipt.path, project.to_string_lossy());
    assert_eq!(
        std::fs::read_to_string(project.join("README.md")).unwrap(),
        "hello\n"
    );
    assert!(!tree_holds(&project, &secret_text));
    let config = std::fs::read_to_string(project.join(".git/config")).unwrap();
    assert!(config.contains(&format!(
        "url = http://127.0.0.1:{}/octo-cat/tool.git",
        server.port
    )));
    assert!(!config.contains("x-access-token"));
    assert!(!project.join(".git/hooks").exists());
    assert_eq!(
        account.clone_repo(&home, "octo-cat/tool").unwrap_err().0,
        "clone_destination_exists"
    );
}

#[test]
fn clone_of_a_repository_named_after_credentials_is_an_ordinary_project() {
    let scratch = Scratch::new();
    let secret_text = token();
    let full_name = "credential-labs/git-credential-tool";
    let (_source, server) = served_repository(full_name, &secret_text);
    let home = tempfile::tempdir().unwrap();
    let home = home.path().canonicalize().unwrap();
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let receipt = account.clone_repo(&home, full_name).unwrap();
    let project = home.join("git-credential-tool");
    assert_eq!(receipt.path, project.to_string_lossy());
    assert_eq!(
        std::fs::read_to_string(project.join("README.md")).unwrap(),
        "hello\n"
    );
    assert!(!tree_holds(&project, &secret_text));
    let config = std::fs::read_to_string(project.join(".git/config")).unwrap();
    assert!(config.contains(&format!(
        "url = http://127.0.0.1:{}/{full_name}.git",
        server.port
    )));
    let entries: Vec<_> = std::fs::read_dir(&home)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries, [std::ffi::OsString::from("git-credential-tool")]);
}

#[test]
fn clone_config_check_refuses_credential_keys_and_a_foreign_origin_by_name() {
    let folder = tempfile::tempdir().unwrap();
    let config = folder.path().join("config");
    let remote =
        url::Url::parse("http://127.0.0.1:9/credential-labs/git-credential-tool.git").unwrap();
    let origin = format!(
        "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {remote}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n[branch \"credential\"]\n\tremote = origin\n\tmerge = refs/heads/credential\n"
    );
    std::fs::write(&config, &origin).unwrap();
    let git = Path::new("/usr/bin/git");
    let home = folder.path().join("home");
    std::fs::create_dir(&home).unwrap();
    clone::verify_config(git, &home, &config, &remote).unwrap();
    for refused in [
        format!("{origin}[credential]\n\thelper = store\n"),
        format!("{origin}[Credential \"https://github.com\"]\n\tHelper = store\n"),
        format!("{origin}[credential]\n\tuseHttpPath\n"),
        origin.replace("git-credential-tool", "other"),
        format!("{origin}[remote \"origin\"]\n\turl = {remote}\n"),
        "[core]\n\tbare = false\n".to_owned(),
        "[core\n".to_owned(),
        format!("{origin}{}", "\n".repeat(65 << 10)),
    ] {
        std::fs::write(&config, &refused).unwrap();
        assert_eq!(
            clone::verify_config(git, &home, &config, &remote)
                .unwrap_err()
                .0,
            "github_clone_failed",
            "{refused:.120}"
        );
    }
    let real = folder.path().join("real");
    std::fs::write(&real, &origin).unwrap();
    std::fs::remove_file(&config).unwrap();
    std::os::unix::fs::symlink(&real, &config).unwrap();
    assert_eq!(
        clone::verify_config(git, &home, &config, &remote)
            .unwrap_err()
            .0,
        "github_clone_failed"
    );
}

#[test]
fn clone_command_holds_no_secret_and_allows_only_its_own_scheme() {
    let scratch = Scratch::new();
    let item = reference(&format!("{}.github", scratch.namespace), "token").unwrap();
    let helper = scratch
        .store
        .git_credential_helper(&item, "x-access-token")
        .unwrap();
    let home = tempfile::tempdir().unwrap();
    let (command, remote) = clone::clone_command(
        &Endpoints::github(),
        &helper,
        Path::new("/usr/bin/git"),
        home.path(),
        &home.path().join("tool"),
        "octo-cat/tool",
    )
    .unwrap();
    assert_eq!(remote.as_str(), "https://github.com/octo-cat/tool.git");
    let argv: Vec<String> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(argv.contains(&"protocol.allow=never".to_owned()));
    assert!(argv.contains(&"protocol.https.allow=always".to_owned()));
    assert!(argv.contains(&"core.hooksPath=/dev/null".to_owned()));
    assert!(argv.contains(&"--no-recurse-submodules".to_owned()));
    assert!(!argv.iter().any(|arg| arg.contains("gho_")));
    let names: Vec<String> = command
        .get_envs()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    assert!(!names.iter().any(|name| name.contains("TOKEN")));
    for name in [
        "owner",
        "owner/name/extra",
        "-x/y",
        "a/.git",
        "a/b.git",
        "a/b c",
    ] {
        assert!(clone::full_name(name).is_err(), "{name} was accepted");
    }
}

#[test]
fn namespaces_differ_per_engine_root_and_stay_short() {
    let first = namespace(Path::new("/Users/someone/.crazyagent"));
    let second = namespace(Path::new("/Users/someone/other-engine"));
    assert_ne!(first, second);
    assert!(first.starts_with("dev.crazyagent.e"));
    assert_eq!(first.len(), "dev.crazyagent.e".len() + 12);
    assert!(crate::credentials::checked_name(&format!("{first}.github")).is_ok());
}
