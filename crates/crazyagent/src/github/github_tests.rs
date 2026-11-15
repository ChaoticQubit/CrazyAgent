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
        headers: Vec::new(),
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
    assert!(contains(
        &config,
        "scope=repo+read%3Auser+read%3Aorg+workflow"
    ));
    let plain = http::Request {
        url: url::Url::parse("http://api.github.com/user").unwrap(),
        accept: "application/json",
        token: None,
        form: None,
        headers: Vec::new(),
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

/// How long a test waits for GitHub work to reach the point where it holds it.
const PAUSE_WAIT: Duration = Duration::from_secs(30);

/// A GitHub stand-in that grants one device sign-in at its first poll. Input: the token
/// it grants. Output: the running stand-in.
fn granting_server(granted: String) -> Server {
    Server::start(move |seen, _| match seen.path.as_str() {
        "/login/device/code" => {
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
        "/login/oauth/access_token" => Answer::json(
            200,
            serde_json::json!({
                "access_token": granted,
                "token_type": "bearer",
                "scope": "repo,read:user",
            }),
        ),
        "/user" => user(seen, &granted),
        _ => Answer::json(404, serde_json::json!({})),
    })
}

#[test]
fn a_device_sign_in_that_saved_its_token_never_reads_as_still_waiting() {
    let scratch = Scratch::new();
    let server = granting_server(token());
    let account = scratch.account(&server);
    let (saved, go_on) = pause_at(&account.root, Pause::AfterKeep);
    account.sign_in().unwrap();
    saved
        .recv_timeout(PAUSE_WAIT)
        .expect("the device sign-in never reached its save");
    let status = account.status();
    go_on.send(()).unwrap();
    assert!(status.signed_in, "{status:?}");
    assert_eq!(status.method, GitHubMethod::Device);
    assert!(
        !status.waiting,
        "a device sign-in that saved its token still reads as waiting: {status:?}"
    );
    assert_eq!(status.problem, None);
    let status = wait_for(&account, |status| status.signed_in && !status.waiting);
    assert_eq!(status.problem, None);
    assert!(!account.sign_out().unwrap().signed_in);
}

#[test]
fn a_status_read_holds_off_every_save_until_it_has_read_the_saved_sign_in() {
    let scratch = Scratch::new();
    let server = Server::start(|_, _| Answer::json(404, serde_json::json!({})));
    let account = scratch.account(&server);
    let (read, finish) = pause_at(&account.root, Pause::InStatus);
    let reader = account.clone();
    let reading = std::thread::spawn(move || reader.status());
    read.recv_timeout(PAUSE_WAIT)
        .expect("the status read never reached the waiting flag");
    let saves_could_land = PENDING.try_lock().is_ok();
    finish.send(()).unwrap();
    let status = reading.join().unwrap();
    assert!(
        !saves_could_land,
        "a save could land between the status read of the waiting flag and of the saved sign-in"
    );
    assert!(!status.signed_in && !status.waiting, "{status:?}");
}

/// How long a test watches for a save that must never come: a sign-in thread that saves
/// does so within milliseconds of GitHub's answer.
const LATE_SAVE_WINDOW: Duration = Duration::from_secs(3);

#[test]
fn a_device_token_github_grants_after_sign_out_is_never_saved() {
    let scratch = Scratch::new();
    let (asked, engine_asked) = std::sync::mpsc::channel::<()>();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let asked = Mutex::new(asked);
    let released = Mutex::new(released);
    let answered = token();
    let server = Server::start(move |seen, _| match seen.path.as_str() {
        "/login/device/code" => {
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
        "/login/oauth/access_token" => Answer::json(
            200,
            serde_json::json!({
                "access_token": answered,
                "token_type": "bearer",
                "scope": "repo,read:user",
            }),
        ),
        "/user" => {
            asked.lock().unwrap().send(()).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(30))
                .unwrap();
            user(seen, &answered)
        }
        _ => Answer::json(404, serde_json::json!({})),
    });
    let account = scratch.account(&server);
    account.sign_in().unwrap();
    engine_asked
        .recv_timeout(Duration::from_secs(30))
        .expect("GitHub granted the code and the engine asked whose token it is");
    assert!(!account.sign_out().unwrap().signed_in);
    release.send(()).unwrap();

    let deadline = Instant::now() + LATE_SAVE_WINDOW;
    while Instant::now() < deadline {
        assert_eq!(
            scratch.state(),
            None,
            "a token GitHub granted after sign-out must not be saved"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let item = reference(&format!("{}.github", scratch.namespace), "token").unwrap();
    assert_eq!(
        scratch.store.get(&item).unwrap_err().0,
        "credential_missing",
        "nor may it reach the credential store"
    );
    let status = account.status();
    assert!(!status.signed_in && !status.waiting, "{status:?}");
}

const SIGN_OUT_RACE_ROUNDS: usize = 12;

/// Stress check, not proof: rounds of a sign-out racing a pasted token must always leave a
/// stored token with its saved sign-in, or neither. It holds on every interleaving of the
/// locked sign-out; code without the lock fails it only when the timing lines up, since no
/// pause point inside sign-out exists to force the losing order with real parts.
#[test]
fn a_token_pasted_during_sign_out_is_kept_with_its_record_or_removed_with_it() {
    let scratch = Scratch::new();
    let before = token();
    let pasted = token();
    let accepted = [before.clone(), pasted.clone()];
    let server = Server::start(move |seen, _| {
        if accepted.iter().any(|token| bearer(seen, token)) {
            Answer::json(200, serde_json::json!({ "login": "octo-cat" }))
        } else {
            Answer::json(401, serde_json::json!({ "message": "Bad credentials" }))
        }
    });
    let account = scratch.account(&server);
    let item = reference(&format!("{}.github", scratch.namespace), "token").unwrap();
    for round in 0..SIGN_OUT_RACE_ROUNDS {
        assert!(account.use_token(secret(&before)).unwrap().signed_in);
        let start = Arc::new(std::sync::Barrier::new(2));
        let paste = {
            let (account, start, pasted) = (account.clone(), Arc::clone(&start), pasted.clone());
            std::thread::spawn(move || {
                start.wait();
                account.use_token(secret(&pasted))
            })
        };
        start.wait();
        assert!(!account.sign_out().unwrap().signed_in);
        assert!(paste.join().unwrap().unwrap().signed_in);
        assert_eq!(
            scratch.state().is_some(),
            scratch.store.get(&item).is_ok(),
            "round {round}: a token in the credential store must have its saved sign-in, and a \
             forgotten sign-in must leave no token behind"
        );
        assert!(!account.sign_out().unwrap().signed_in);
        assert_eq!(scratch.state(), None);
        assert_eq!(
            scratch.store.get(&item).unwrap_err().0,
            "credential_missing"
        );
    }
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

#[test]
fn a_dot_named_repository_is_listed_and_never_stops_the_rest_of_the_page() {
    let scratch = Scratch::new();
    let secret_text = token();
    let expected = secret_text.clone();
    let long = format!("octo-cat/{}\u{202e}", "x".repeat(300));
    let served_long = long.clone();
    let server = Server::start(move |seen, _| match seen.path.as_str() {
        "/user" => user(seen, &expected),
        "/user/repos" if bearer(seen, &expected) => {
            let repos: Vec<serde_json::Value> = (0..100)
                .map(|index| {
                    let full_name = match index {
                        3 => "octo-cat/.github".to_owned(),
                        5 => "octo-cat/-dash".to_owned(),
                        9 => served_long.clone(),
                        _ => format!("octo-cat/repo-{index}"),
                    };
                    let branch = if index == 7 { "ma\u{7}in\nx" } else { "main" };
                    serde_json::json!({
                        "full_name": full_name,
                        "private": false,
                        "default_branch": branch,
                        "size": 1,
                    })
                })
                .collect();
            Answer::json(200, serde_json::Value::Array(repos))
        }
        _ => Answer::json(401, serde_json::json!({})),
    });
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let page = account.repos(1).unwrap();
    assert_eq!(page.repos.len(), 100);
    assert_eq!(page.next, Some(2));
    assert_eq!(page.repos[3].full_name, "octo-cat/.github");
    assert_eq!(page.repos[5].full_name, "octo-cat/-dash");
    assert_eq!(page.repos[7].default_branch, "ma\\u0007in\\u000ax");
    assert_eq!(page.repos[8].full_name, "octo-cat/repo-8");
    let shown = &page.repos[9].full_name;
    assert!(
        shown.starts_with("octo-cat/xxx") && shown.len() <= 201,
        "{shown}"
    );
    assert!(!shown.contains('\u{202e}'), "{shown}");
    assert_ne!(shown, &long);
    for (index, repo) in page.repos.iter().enumerate() {
        let refused = matches!(index, 3 | 5 | 9).then_some("github_repo_name_unsupported");
        assert_eq!(repo.refused.as_deref(), refused, "{index}");
    }
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        account
            .clone_repo(home.path(), "octo-cat/.github")
            .unwrap_err()
            .0,
        "github_repo_name_invalid"
    );
}

#[test]
fn a_reply_carries_the_scopes_github_sent_even_with_a_full_size_body() {
    let server = Server::start(|seen, _| {
        let mut answer = Answer::bytes(200, vec![b'x'; 4 << 20]);
        let scopes = match seen.path.as_str() {
            "/classic" => Some("repo, read:org, workflow"),
            "/odd" => Some("%header{x-oauth-scopes}"),
            _ => None,
        };
        if let Some(scopes) = scopes {
            answer
                .headers
                .push(("X-OAuth-Scopes".into(), scopes.into()));
        }
        answer
    });
    let send = |path: &str| {
        http::Request {
            url: url::Url::parse(&format!("http://127.0.0.1:{}{path}", server.port)).unwrap(),
            accept: "application/json",
            token: None,
            form: None,
            headers: Vec::new(),
            allow_http: true,
        }
        .send()
        .unwrap()
    };
    let classic = send("/classic");
    assert_eq!(classic.status, 200);
    assert_eq!(classic.body.len(), 4 << 20);
    assert_eq!(
        classic.scopes,
        Some(vec![
            "repo".to_owned(),
            "read:org".to_owned(),
            "workflow".to_owned()
        ])
    );
    let fine_grained = send("/fine");
    assert_eq!(fine_grained.body.len(), 4 << 20);
    assert_eq!(fine_grained.scopes, None);
    assert_eq!(send("/odd").scopes, None);
}

fn scoped(mut answer: Answer, scopes: Option<&str>) -> Answer {
    if let Some(scopes) = scopes {
        answer
            .headers
            .push(("X-OAuth-Scopes".into(), scopes.into()));
    }
    answer
}

fn owners_server(secret_text: &str, scopes: Option<&'static str>, memberships: u16) -> Server {
    let expected = secret_text.to_owned();
    Server::start(move |seen, _| match seen.path.as_str() {
        "/user" => scoped(user(seen, &expected), scopes),
        "/user/memberships/orgs" if bearer(seen, &expected) && memberships == 200 => Answer::json(
            200,
            serde_json::json!([
                { "state": "active", "role": "admin", "organization": { "login": "acme-labs" } },
                { "state": "active", "role": "member", "organization": { "login": "beta-org" } },
                { "state": "active", "role": "billing_manager", "organization": { "login": "billing-only" } },
                { "state": "pending", "role": "admin", "organization": { "login": "not-yet" } },
            ]),
        ),
        "/user/memberships/orgs" => Answer::json(
            memberships,
            serde_json::json!({
                "message": "Forbidden"
            }),
        ),
        _ => Answer::json(404, serde_json::json!({ "message": "Not Found" })),
    })
}

#[test]
fn owners_list_the_personal_account_first_then_the_organizations_the_owner_is_a_member_of() {
    let scratch = Scratch::new();
    let secret_text = token();
    let server = owners_server(&secret_text, Some("repo, read:org, workflow"), 200);
    let account = scratch.account(&server);
    assert_eq!(account.owners().unwrap_err().0, "github_not_signed_in");
    account.use_token(secret(&secret_text)).unwrap();
    let owners = account.owners().unwrap();
    assert_eq!(owners.problem, None);
    let listed: Vec<(&str, bool)> = owners
        .owners
        .iter()
        .map(|owner| (owner.login.as_str(), owner.personal))
        .collect();
    assert_eq!(
        listed,
        [
            ("octo-cat", true),
            ("acme-labs", false),
            ("beta-org", false)
        ]
    );
    let asked = server
        .seen()
        .into_iter()
        .find(|seen| seen.path == "/user/memberships/orgs")
        .unwrap();
    let query = asked.query_pairs();
    assert_eq!(query.get("state").map(String::as_str), Some("active"));
    assert_eq!(query.get("per_page").map(String::as_str), Some("100"));
}

#[test]
fn owners_without_read_org_still_offer_the_personal_account_and_say_why() {
    let reauth = "gh auth refresh -s read:org";
    for (scopes, memberships, asks, sentence) in [
        (Some("repo, read:user"), 200, false, Some(reauth)),
        (None, 403, true, Some(reauth)),
        (Some("repo, admin:org"), 200, true, None),
        (
            Some("repo, read:org"),
            403,
            true,
            Some("refused to list your organizations"),
        ),
    ] {
        let scratch = Scratch::new();
        let secret_text = token();
        let server = owners_server(&secret_text, scopes, memberships);
        let account = scratch.account(&server);
        account.use_token(secret(&secret_text)).unwrap();
        let owners = account.owners().unwrap();
        let asked = server
            .seen()
            .iter()
            .any(|seen| seen.path == "/user/memberships/orgs");
        assert_eq!(asked, asks, "{scopes:?}");
        let Some(sentence) = sentence else {
            assert_eq!(owners.owners.len(), 3, "{scopes:?}");
            assert_eq!(owners.problem, None, "{scopes:?}");
            continue;
        };
        assert_eq!(
            owners.owners,
            [GitHubOwner {
                login: "octo-cat".into(),
                personal: true,
            }],
            "{scopes:?}"
        );
        let problem = owners.problem.unwrap();
        assert!(problem.contains(sentence), "{scopes:?}: {problem}");
        assert_eq!(
            problem.contains("read:org"),
            sentence == reauth,
            "{scopes:?}: {problem}"
        );
    }
}

#[test]
fn a_repository_name_is_free_on_a_404_taken_otherwise_and_checked_before_any_request() {
    let scratch = Scratch::new();
    let secret_text = token();
    let expected = secret_text.clone();
    let server = Server::start(move |seen, _| match seen.path.as_str() {
        "/user" => user(seen, &expected),
        "/repos/octo-cat/tool" if bearer(seen, &expected) => {
            Answer::json(200, serde_json::json!({ "full_name": "octo-cat/tool" }))
        }
        "/repos/acme-labs/moved" if bearer(seen, &expected) => {
            Answer::json(301, serde_json::json!({ "message": "Moved Permanently" }))
        }
        _ => Answer::json(404, serde_json::json!({ "message": "Not Found" })),
    });
    let account = scratch.account(&server);
    assert_eq!(
        account.name_free("octo-cat", "tool").unwrap_err().0,
        "github_not_signed_in"
    );
    account.use_token(secret(&secret_text)).unwrap();
    assert!(!account.name_free("octo-cat", "tool").unwrap());
    assert!(!account.name_free("acme-labs", "moved").unwrap());
    assert!(account.name_free("octo-cat", "new-tool").unwrap());
    let before = server.seen().len();
    let long = "x".repeat(101);
    for (owner, name) in [
        ("octo-cat", ""),
        ("octo-cat", "a b"),
        ("octo-cat", ".."),
        ("octo-cat", "tool.git"),
        ("octo-cat", long.as_str()),
        ("", "tool"),
        ("octo/cat", "tool"),
        ("octo-cat", "../tool"),
    ] {
        assert_eq!(
            account.name_free(owner, name).unwrap_err().0,
            "github_repo_name_invalid",
            "{owner}/{name}"
        );
    }
    assert_eq!(server.seen().len(), before);
}

#[test]
fn create_makes_a_private_squash_only_repository_under_the_chosen_account() {
    let scratch = Scratch::new();
    let secret_text = token();
    let expected = secret_text.clone();
    let server = Server::start(move |seen, _| {
        let body: serde_json::Value = serde_json::from_slice(&seen.body).unwrap_or_default();
        let name = body["name"].as_str().unwrap_or_default().to_owned();
        let private = if name == "exposed" {
            serde_json::json!(false)
        } else {
            body["private"].clone()
        };
        let made = |owner: &str, node: &str| {
            let full_name = if name == "renamed" {
                format!("{owner}/other")
            } else {
                format!("{owner}/{name}")
            };
            Answer::json(
                201,
                serde_json::json!({ "full_name": full_name, "node_id": node, "private": private }),
            )
        };
        match (seen.method.as_str(), seen.path.as_str()) {
            (_, "/user") => user(seen, &expected),
            ("POST", "/user/repos") if bearer(seen, &expected) && name == "taken" => Answer::json(
                422,
                serde_json::json!({
                    "message": "Repository creation failed.",
                    "errors": [{
                        "resource": "Repository",
                        "code": "custom",
                        "field": "name",
                        "message": "name already exists on this account"
                    }]
                }),
            ),
            ("POST", "/user/repos") if bearer(seen, &expected) => made("octo-cat", "R_kgDOtest01"),
            ("POST", "/orgs/acme-labs/repos") if bearer(seen, &expected) => {
                made("acme-labs", "R_kgDOtest02")
            }
            ("POST", "/orgs/beta-org/repos") => Answer::json(
                403,
                serde_json::json!({
                    "message": "You need admin access to the organization before adding a repository to it."
                }),
            ),
            _ => Answer::json(404, serde_json::json!({ "message": "Not Found" })),
        }
    });
    let account = scratch.account(&server);
    let repo = |owner: &str, name: &str, private: bool| GitHubNewRepo {
        owner: owner.into(),
        name: name.into(),
        description: "A tool that sorts things.".into(),
        private,
    };
    assert_eq!(
        account
            .create_repo(&repo("octo-cat", "tool", true))
            .unwrap_err()
            .0,
        "github_not_signed_in"
    );
    account.use_token(secret(&secret_text)).unwrap();
    let created = account
        .create_repo(&repo("octo-cat", "tool", true))
        .unwrap();
    assert_eq!(
        created,
        GitHubCreated {
            full_name: "octo-cat/tool".into(),
            node_id: "R_kgDOtest01".into(),
            private: true,
        }
    );
    let sent = server
        .seen()
        .into_iter()
        .find(|seen| seen.path == "/user/repos")
        .unwrap();
    assert_eq!(sent.header("content-type"), Some("application/json"));
    let body: serde_json::Value = serde_json::from_slice(&sent.body).unwrap();
    assert_eq!(
        body,
        serde_json::json!({
            "name": "tool",
            "description": "A tool that sorts things.",
            "private": true,
            "has_issues": true,
            "has_wiki": false,
            "has_projects": false,
            "allow_squash_merge": true,
            "allow_merge_commit": false,
            "allow_rebase_merge": false,
            "delete_branch_on_merge": true,
            "allow_auto_merge": false,
            "auto_init": false,
        })
    );
    let public = account
        .create_repo(&repo("acme-labs", "site", false))
        .unwrap();
    assert_eq!(public.full_name, "acme-labs/site");
    assert!(!public.private);
    let org_sent = server
        .seen()
        .into_iter()
        .find(|seen| seen.path == "/orgs/acme-labs/repos")
        .unwrap();
    let org_body: serde_json::Value = serde_json::from_slice(&org_sent.body).unwrap();
    assert_eq!(org_body["private"], serde_json::json!(false));
    for (owner, name, code) in [
        ("octo-cat", "taken", "github_repo_name_taken"),
        ("beta-org", "tool", "github_repo_create_refused"),
        ("octo-cat", "renamed", "github_reply_invalid"),
        ("octo-cat", "exposed", "github_repo_visibility_mismatch"),
    ] {
        assert_eq!(
            account.create_repo(&repo(owner, name, true)).unwrap_err().0,
            code,
            "{owner}/{name}"
        );
    }
    let before = server.seen().len();
    for (bad, code) in [
        (
            GitHubNewRepo {
                description: "x".repeat(351),
                ..repo("octo-cat", "long", true)
            },
            "github_repo_description_invalid",
        ),
        (
            GitHubNewRepo {
                description: "line\nbreak".into(),
                ..repo("octo-cat", "lines", true)
            },
            "github_repo_description_invalid",
        ),
        (repo("octo-cat", "a b", true), "github_repo_name_invalid"),
    ] {
        assert_eq!(account.create_repo(&bad).unwrap_err().0, code);
    }
    assert_eq!(server.seen().len(), before);
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
