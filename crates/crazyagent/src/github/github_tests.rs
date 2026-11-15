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
        Answer::json(
            200,
            serde_json::json!({ "login": "octo-cat", "id": 583_231 }),
        )
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

fn paged_server(secret_text: &str, full_pages: u32, extra: usize) -> Server {
    let expected = secret_text.to_owned();
    Server::start(move |seen, _| match seen.path.as_str() {
        "/user" => scoped(user(seen, &expected), Some("repo, read:org")),
        "/user/memberships/orgs" if bearer(seen, &expected) => {
            let page: u32 = seen
                .query_pairs()
                .get("page")
                .map_or(1, |page| page.parse().unwrap());
            let count = if page <= full_pages {
                100
            } else if page == full_pages + 1 {
                extra
            } else {
                0
            };
            let listed: Vec<serde_json::Value> = (0..count)
                .map(|index| {
                    serde_json::json!({
                        "state": "active",
                        "role": "member",
                        "organization": { "login": format!("org-{page}-{index}") }
                    })
                })
                .collect();
            Answer::json(200, serde_json::Value::Array(listed))
        }
        _ => Answer::json(404, serde_json::json!({ "message": "Not Found" })),
    })
}

#[test]
fn owners_follow_the_next_page_up_to_ten_pages_and_say_when_more_were_left_out() {
    for (full_pages, extra, asked, offered, cut) in [
        (1, 3, 2, 104, false),
        (1, 0, 2, 101, false),
        (12, 0, 10, 1001, true),
    ] {
        let scratch = Scratch::new();
        let secret_text = token();
        let server = paged_server(&secret_text, full_pages, extra);
        let account = scratch.account(&server);
        account.use_token(secret(&secret_text)).unwrap();
        let owners = account.owners().unwrap();
        let pages: Vec<String> = server
            .seen()
            .iter()
            .filter(|seen| seen.path == "/user/memberships/orgs")
            .map(|seen| seen.query_pairs().get("page").cloned().unwrap_or_default())
            .collect();
        let expected: Vec<String> = (1..=asked).map(|page| page.to_string()).collect();
        assert_eq!(pages, expected, "{full_pages} {extra}");
        assert_eq!(owners.owners.len(), offered, "{full_pages} {extra}");
        assert!(owners.owners[0].personal);
        match owners.problem {
            Some(problem) => assert!(cut && problem.contains("1,000"), "{problem}"),
            None => assert!(!cut, "{full_pages} {extra}"),
        }
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

#[test]
fn the_rulesets_crazyagent_made_are_remembered_for_the_next_pass() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new();
    let secret_text = token();
    let made: Arc<Mutex<Vec<(u64, String)>>> = Arc::default();
    let rulesets = Arc::clone(&made);
    let expected = secret_text.clone();
    let list = "/repos/octo-cat/tool/rulesets";
    let server = Server::start(move |seen, index| {
        if seen.path == "/user" {
            return user(seen, &expected);
        }
        if !bearer(seen, &expected) {
            return Answer::json(401, serde_json::json!({ "message": "Bad credentials" }));
        }
        match seen.method.as_str() {
            "GET" if seen.path == list => {
                let listed: Vec<serde_json::Value> = rulesets
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(id, name)| {
                        serde_json::json!({ "id": id, "name": name, "source_type": "Repository" })
                    })
                    .collect();
                Answer::json(200, serde_json::Value::Array(listed))
            }
            "POST" if seen.path == list => {
                let body: serde_json::Value = serde_json::from_slice(&seen.body).unwrap();
                let id = 500 + index as u64;
                rulesets
                    .lock()
                    .unwrap()
                    .push((id, body["name"].as_str().unwrap().to_owned()));
                Answer::json(201, serde_json::json!({ "id": id }))
            }
            "PUT" if seen.path.starts_with(&format!("{list}/")) => {
                Answer::json(200, serde_json::json!({}))
            }
            "PATCH" if seen.path == "/repos/octo-cat/tool" => Answer::json(
                200,
                serde_json::from_slice::<serde_json::Value>(&seen.body).unwrap(),
            ),
            "PATCH" => Answer::json(202, serde_json::json!({})),
            _ => Answer::bytes(204, Vec::new()),
        }
    });
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let facts = GitHubRepoFacts {
        private: true,
        ci: GitHubCi::None,
    };
    let first = account
        .apply_rules("octo-cat/tool", &GitHubRule::defaults(), &facts)
        .unwrap();
    assert!(
        first
            .iter()
            .all(|outcome| !matches!(outcome.state, GitHubRuleState::Refused { .. })),
        "{first:?}"
    );
    let folder = scratch.root.path().join("github-repos");
    let files: Vec<PathBuf> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1);
    assert_eq!(
        std::fs::metadata(&files[0]).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(&folder).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&files[0]).unwrap()).unwrap();
    let mut recorded: Vec<u64> = record["rulesets"]
        .as_object()
        .unwrap()
        .values()
        .map(|id| id.as_u64().unwrap())
        .collect();
    recorded.sort_unstable();
    let ids: Vec<u64> = made.lock().unwrap().iter().map(|(id, _)| *id).collect();
    assert_eq!(ids.len(), 5);
    assert_eq!(recorded, ids);
    let before = server.seen().len();
    let second = account
        .apply_rules("octo-cat/tool", &GitHubRule::defaults(), &facts)
        .unwrap();
    assert_eq!(second, first);
    let again: Vec<String> = server.seen()[before..]
        .iter()
        .filter(|seen| seen.path.contains("/rulesets"))
        .map(|seen| format!("{} {}", seen.method, seen.path))
        .collect();
    assert!(
        again.iter().all(|call| !call.starts_with("POST")),
        "{again:?}"
    );
    assert_eq!(
        again.iter().filter(|call| call.starts_with("PUT")).count(),
        5
    );
    assert_eq!(made.lock().unwrap().len(), 5);
}

#[test]
fn a_repository_record_is_found_in_any_case_and_a_tampered_one_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new();
    let secret_text = token();
    let expected = secret_text.clone();
    let server = Server::start(move |seen, _| {
        if seen.path == "/user" {
            return user(seen, &expected);
        }
        match seen.method.as_str() {
            "GET" => Answer::json(
                200,
                serde_json::json!([{ "id": 7, "name": "CrazyAgent pull requests" }]),
            ),
            "PUT" => Answer::json(200, serde_json::json!({ "id": 7 })),
            _ => Answer::bytes(204, Vec::new()),
        }
    });
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let folder = scratch.root.path().join("github-repos");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let file: String = Sha256::digest(b"octo-cat/tool")
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let write = |text: &str| {
        let path = folder.join(format!("{file}.json"));
        let _ = std::fs::remove_file(&path);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        std::io::Write::write_all(&mut options.open(&path).unwrap(), text.as_bytes()).unwrap();
    };
    let facts = GitHubRepoFacts {
        private: true,
        ci: GitHubCi::None,
    };
    write(r#"{"rulesets":{"pull_request":7}}"#);
    let outcome = account
        .set_rule(
            "Octo-Cat/Tool",
            GitHubRule::PullRequest,
            &GitHubRule::defaults(),
            &facts,
        )
        .unwrap();
    assert_eq!(outcome.state, GitHubRuleState::Applied);
    assert_eq!(server.seen().last().unwrap().method, "PUT");
    assert_eq!(
        server.seen().last().unwrap().path,
        "/repos/Octo-Cat/Tool/rulesets/7"
    );
    for tampered in [
        r#"{"rulesets":{"squash_only":7}}"#,
        r#"{"rulesets":{"conversation_resolution":7}}"#,
        r#"{"rulesets":{"pull_request":-1}}"#,
        r#"{"rulesets":{},"extra":1}"#,
        "not json",
    ] {
        write(tampered);
        let before = server.seen().len();
        assert_eq!(
            account
                .apply_rules("octo-cat/tool", &GitHubRule::defaults(), &facts)
                .unwrap_err()
                .0,
            "github_state_invalid",
            "{tampered}"
        );
        assert_eq!(server.seen().len(), before, "{tampered}");
    }
}

fn git_out(args: &[&str], directory: &Path) -> String {
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/git")
            .args(args)
            .current_dir(directory)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", directory)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn http_backend(served: &Path, seen: &Seen) -> Answer {
    let mut command = Command::new("/usr/bin/git");
    command
        .arg("http-backend")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_PROJECT_ROOT", served)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("REMOTE_USER", "x-access-token")
        .env("REQUEST_METHOD", &seen.method)
        .env("PATH_INFO", &seen.path)
        .env("QUERY_STRING", &seen.query)
        .env("CONTENT_TYPE", seen.header("content-type").unwrap_or(""))
        .env("CONTENT_LENGTH", seen.body.len().to_string())
        .env(
            "HTTP_CONTENT_ENCODING",
            seen.header("content-encoding").unwrap_or(""),
        )
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = crate::spawn_gate::spawn(&mut command).unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let body = seen.body.clone();
    let writer = std::thread::spawn(move || {
        let _ = std::io::Write::write_all(&mut stdin, &body);
    });
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap();
    let end = output
        .stdout
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap();
    let mut answer = Answer {
        status: 200,
        headers: Vec::new(),
        body: output.stdout[end + 4..].to_vec(),
    };
    for line in String::from_utf8_lossy(&output.stdout[..end]).split("\r\n") {
        let (name, value) = line.split_once(':').unwrap();
        if name.eq_ignore_ascii_case("status") {
            answer.status = value.trim()[..3].parse().unwrap();
        } else if !name.eq_ignore_ascii_case("content-length") {
            answer
                .headers
                .push((name.to_owned(), value.trim().to_owned()));
        }
    }
    answer
}

fn pushed_repository(
    full_name: &str,
    secret_text: &str,
    refuse_push: bool,
) -> (tempfile::TempDir, PathBuf, Server) {
    let served = tempfile::tempdir().unwrap();
    let root = served.path().canonicalize().unwrap();
    let bare = root.join(format!("{full_name}.git"));
    std::fs::create_dir_all(&bare).unwrap();
    git(&["init", "--quiet", "--bare"], &bare);
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
        if refuse_push && seen.method == "POST" {
            return Answer::bytes(403, Vec::new());
        }
        http_backend(&root, seen)
    });
    (served, bare, server)
}

fn published_project() -> (tempfile::TempDir, PathBuf, GitHubCommitted) {
    let folder = tempfile::tempdir().unwrap();
    let project = folder.path().canonicalize().unwrap();
    git(&["init", "--quiet", "--initial-branch=main"], &project);
    std::fs::write(project.join("README.md"), "hello\n").unwrap();
    git(&["add", "README.md"], &project);
    git(&["commit", "--quiet", "-m", "first"], &project);
    let commit = git_out(&["rev-parse", "HEAD"], &project).trim().to_owned();
    let committed = GitHubCommitted {
        branch: "main".into(),
        commit,
        left_out: 0,
    };
    (folder, project, committed)
}

fn publish_homes(root: &Path) -> Vec<String> {
    std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".publish-home-"))
        .collect()
}

#[test]
fn publish_commit_uses_the_owners_identity_else_the_github_noreply_address() {
    let scratch = Scratch::new();
    let secret_text = token();
    let expected = secret_text.clone();
    let server = Server::start(move |seen, _| user(seen, &expected));
    let account = scratch.account(&server);
    let folder = tempfile::tempdir().unwrap();
    let project = folder.path().canonicalize().unwrap();
    let owner_home = tempfile::tempdir().unwrap();
    std::fs::write(project.join("a.txt"), "a\n").unwrap();
    let local = account.publish_inspect(&project).unwrap();
    let choices = GitHubPrepare {
        private: true,
        docs: false,
        approved: Vec::new(),
    };
    let written = account.publish_prepare(&project, &local, &choices).unwrap();
    assert_eq!(
        account
            .publish_commit(&project, owner_home.path(), &written)
            .unwrap_err()
            .0,
        "github_not_signed_in"
    );
    account.use_token(secret(&secret_text)).unwrap();
    let committed = account
        .publish_commit(&project, owner_home.path(), &written)
        .unwrap();
    assert_eq!(committed.branch, "main");
    assert_eq!(
        git_out(&["log", "--format=%an|%ae|%s"], &project),
        "octo-cat|583231+octo-cat@users.noreply.github.com|Publish with CrazyAgent\n"
    );
    std::fs::write(
        owner_home.path().join(".gitconfig"),
        "[user]\n\tname = Ada Lovelace\n\temail = ada@example.invalid\n",
    )
    .unwrap();
    std::fs::write(project.join(".gitignore"), "/graphify-out/\nnew/\n").unwrap();
    let again = account
        .publish_commit(&project, owner_home.path(), &[String::from(".gitignore")])
        .unwrap();
    assert_ne!(again.commit, committed.commit);
    assert_eq!(
        git_out(&["log", "-1", "--format=%an|%ae"], &project),
        "Ada Lovelace|ada@example.invalid\n"
    );
    assert!(publish_homes(scratch.root.path()).is_empty());
}

#[test]
fn publish_push_sends_from_a_staging_repository_and_points_the_project_at_github() {
    let scratch = Scratch::new();
    let secret_text = token();
    let (_served, bare, server) = pushed_repository("octo-cat/tool", &secret_text, false);
    let account = scratch.account(&server);
    let (_folder, project, committed) = published_project();
    let remote = format!("http://127.0.0.1:{}/octo-cat/tool.git", server.port);
    git(
        &[
            "config",
            "url.http://127.0.0.1:9/.insteadOf",
            &format!("http://127.0.0.1:{}/", server.port),
        ],
        &project,
    );
    git(&["config", "http.extraHeader", "X-Planted: yes"], &project);
    git(
        &[
            "config",
            "credential.helper",
            "!f() { echo password=planted; }; f",
        ],
        &project,
    );
    assert_eq!(
        account
            .publish_push(&project, "octo-cat/tool", &committed)
            .unwrap_err()
            .0,
        "github_not_signed_in"
    );
    account.use_token(secret(&secret_text)).unwrap();
    account
        .publish_push(&project, "octo-cat/tool", &committed)
        .unwrap();
    assert_eq!(
        git_out(&["rev-parse", "refs/heads/main"], &bare).trim(),
        committed.commit
    );
    let seen = server.seen();
    assert!(seen.iter().any(|request| {
        request.method == "POST" && request.path == "/octo-cat/tool.git/git-receive-pack"
    }));
    assert!(
        seen.iter()
            .all(|request| request.header("x-planted").is_none())
    );
    let config = |key: &str| {
        git_out(&["config", "--get", key], &project)
            .trim()
            .to_owned()
    };
    assert_eq!(config("remote.origin.url"), remote);
    assert_eq!(
        config("remote.origin.fetch"),
        "+refs/heads/*:refs/remotes/origin/*"
    );
    assert_eq!(config("branch.main.remote"), "origin");
    assert_eq!(config("branch.main.merge"), "refs/heads/main");
    assert_eq!(
        git_out(&["rev-parse", "refs/remotes/origin/main"], &project).trim(),
        committed.commit
    );
    assert!(!tree_holds(&project, &secret_text));
    assert!(publish_homes(scratch.root.path()).is_empty());
    account
        .publish_push(&project, "octo-cat/tool", &committed)
        .unwrap();
    assert_eq!(config("remote.origin.url"), remote);
}

#[test]
fn publish_push_refuses_a_moved_head_and_a_refused_push_and_leaves_the_project_alone() {
    let scratch = Scratch::new();
    let secret_text = token();
    let (_served, bare, server) = pushed_repository("octo-cat/tool", &secret_text, true);
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let (_folder, project, committed) = published_project();
    let before = server.seen().len();
    for moved in [
        GitHubCommitted {
            branch: "other".into(),
            ..committed.clone()
        },
        GitHubCommitted {
            commit: "0".repeat(40),
            ..committed.clone()
        },
        GitHubCommitted {
            commit: "zz".into(),
            ..committed.clone()
        },
    ] {
        assert_eq!(
            account
                .publish_push(&project, "octo-cat/tool", &moved)
                .unwrap_err()
                .0,
            "github_publish_head_moved",
            "{moved:?}"
        );
    }
    assert_eq!(
        account
            .publish_push(&project, "../tool", &committed)
            .unwrap_err()
            .0,
        "github_repo_name_invalid"
    );
    assert_eq!(server.seen().len(), before);
    assert_eq!(
        account
            .publish_push(&project, "octo-cat/tool", &committed)
            .unwrap_err()
            .0,
        "github_publish_push_failed"
    );
    assert!(server.seen().len() > before);
    let config = std::fs::read_to_string(project.join(".git/config")).unwrap();
    assert!(!config.contains("remote"), "{config}");
    assert!(!config.contains("[branch"), "{config}");
    assert!(git_out(&["for-each-ref", "refs/remotes"], &project).is_empty());
    assert!(git_out(&["for-each-ref"], &bare).is_empty());
    assert!(publish_homes(scratch.root.path()).is_empty());
}

#[test]
fn publish_push_refuses_a_config_hook_in_the_project_before_anything_is_sent() {
    let scratch = Scratch::new();
    let secret_text = token();
    let (_served, bare, server) = pushed_repository("octo-cat/tool", &secret_text, false);
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let (_folder, project, committed) = published_project();
    let marks = tempfile::tempdir().unwrap();
    let mark = marks.path().join("ran");
    git(
        &["config", "hook.mark.event", "reference-transaction"],
        &project,
    );
    let command = format!("/usr/bin/touch '{}'", mark.display());
    git(&["config", "hook.mark.command", &command], &project);
    let before = server.seen().len();
    let pushed = account.publish_push(&project, "octo-cat/tool", &committed);
    assert!(!mark.exists());
    assert_eq!(pushed.unwrap_err().0, "github_publish_hooks_set");
    assert_eq!(server.seen().len(), before);
    assert!(git_out(&["for-each-ref"], &bare).is_empty());
    let config = std::fs::read_to_string(project.join(".git/config")).unwrap();
    assert!(!config.contains("[remote"), "{config}");
    assert!(publish_homes(scratch.root.path()).is_empty());
}

#[test]
fn publish_push_never_runs_a_hook_an_include_keyed_on_the_remote_url_brings_in() {
    let scratch = Scratch::new();
    let secret_text = token();
    let (_served, bare, server) = pushed_repository("octo-cat/tool", &secret_text, false);
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let (_folder, project, committed) = published_project();
    let marks = tempfile::tempdir().unwrap();
    let mark = marks.path().join("ran");
    std::fs::write(
        project.join(".git/gh.inc"),
        format!(
            "[hook \"mark\"]\n\tevent = reference-transaction\n\tcommand = /usr/bin/touch '{}'\n",
            mark.display()
        ),
    )
    .unwrap();
    git(
        &[
            "config",
            "includeIf.hasconfig:remote.*.url:**.path",
            "gh.inc",
        ],
        &project,
    );
    account
        .publish_push(&project, "octo-cat/tool", &committed)
        .unwrap();
    assert!(!mark.exists());
    assert_eq!(
        git_out(&["rev-parse", "refs/heads/main"], &bare).trim(),
        committed.commit
    );
    assert_eq!(
        git_out(&["rev-parse", "refs/remotes/origin/main"], &project).trim(),
        committed.commit
    );
    assert!(publish_homes(scratch.root.path()).is_empty());
}

const LEAKED: &str = "ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8";

struct Stand {
    create: u16,
    private: bool,
    branches: bool,
    created_at: String,
    scopes: Option<&'static str>,
    recorded: Vec<bool>,
}

fn rfc3339(seconds: u64) -> String {
    let days = i64::try_from(seconds / 86_400).unwrap();
    let rest = seconds % 86_400;
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let of_era = shifted.rem_euclid(146_097);
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * of_year + 2) / 153;
    let day = of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn publish_stand(
    secret_text: &str,
    scratch: &Scratch,
    names: &[&str],
) -> (tempfile::TempDir, PathBuf, Arc<Mutex<Stand>>, Server) {
    let served = tempfile::tempdir().unwrap();
    let root = served.path().canonicalize().unwrap();
    for name in names {
        let bare = root.join(format!("octo-cat/{name}.git"));
        std::fs::create_dir_all(&bare).unwrap();
        git(&["init", "--quiet", "--bare"], &bare);
    }
    let stand = Arc::new(Mutex::new(Stand {
        create: 201,
        private: true,
        branches: false,
        created_at: rfc3339(now()),
        scopes: None,
        recorded: Vec::new(),
    }));
    let records = scratch
        .root
        .path()
        .canonicalize()
        .unwrap()
        .join("github-publish");
    let basic = format!(
        "Basic {}",
        base64(format!("x-access-token:{secret_text}").as_bytes())
    );
    let expected = secret_text.to_owned();
    let held = Arc::clone(&stand);
    let backend = root.clone();
    let server = Server::start(move |seen, _| {
        let mut stand = held.lock().unwrap();
        let path = seen.path.as_str();
        if path == "/user" {
            let mut answer = user(seen, &expected);
            if let Some(scopes) = stand.scopes {
                answer
                    .headers
                    .push(("X-OAuth-Scopes".into(), scopes.into()));
            }
            return answer;
        }
        if path.starts_with("/octo-cat/") {
            if seen.header("authorization") != Some(basic.as_str()) {
                let mut answer = Answer::bytes(401, Vec::new());
                answer
                    .headers
                    .push(("WWW-Authenticate".into(), "Basic realm=\"git\"".into()));
                return answer;
            }
            return http_backend(&backend, seen);
        }
        if !bearer(seen, &expected) {
            return Answer::json(401, serde_json::json!({ "message": "Bad credentials" }));
        }
        if seen.method == "POST" && path == "/user/repos" {
            let creating = std::fs::read_dir(&records)
                .into_iter()
                .flatten()
                .flatten()
                .any(|entry| {
                    std::fs::read_to_string(entry.path())
                        .is_ok_and(|text| text.contains("\"step\":\"creating\""))
                });
            stand.recorded.push(creating);
            let body: serde_json::Value = serde_json::from_slice(&seen.body).unwrap();
            let name = body["name"].as_str().unwrap().to_owned();
            return match stand.create {
                201 => Answer::json(
                    201,
                    serde_json::json!({
                        "full_name": format!("octo-cat/{name}"),
                        "node_id": "R_kgDOtool",
                        "private": body["private"],
                    }),
                ),
                422 => Answer::json(
                    422,
                    serde_json::json!({
                        "message": "Repository creation failed.",
                        "errors": [{
                            "resource": "Repository",
                            "code": "custom",
                            "field": "name",
                            "message": "name already exists on this account",
                        }],
                    }),
                ),
                status => Answer::json(status, serde_json::json!({})),
            };
        }
        if seen.method == "GET"
            && let Some(rest) = path.strip_prefix("/repos/octo-cat/")
        {
            match rest.split_once('/') {
                None => {
                    return Answer::json(
                        200,
                        serde_json::json!({
                            "full_name": format!("octo-cat/{rest}"),
                            "node_id": "R_kgDOtool",
                            "private": stand.private,
                            "created_at": stand.created_at,
                            "pushed_at": null,
                            "size": 0,
                        }),
                    );
                }
                Some((_, "branches")) => {
                    let branches = if stand.branches {
                        serde_json::json!([{ "name": "main" }])
                    } else {
                        serde_json::json!([])
                    };
                    return Answer::json(200, branches);
                }
                Some(_) => {}
            }
        }
        Answer::json(404, serde_json::json!({ "message": "Not Found" }))
    });
    (served, root, stand, server)
}

fn plan(name: &str) -> GitHubPublishPlan {
    GitHubPublishPlan {
        owner: "octo-cat".into(),
        name: name.into(),
        description: "A small tool".into(),
        private: true,
        docs: false,
    }
}

fn baseline() -> Vec<String> {
    [".env*", "*.pem", "*.key", ".DS_Store"]
        .map(String::from)
        .to_vec()
}

fn no_draft(_prompt: &str) -> Result<String, &'static str> {
    Err(crate::instruction_cleanup::UNAVAILABLE)
}

fn creates(server: &Server) -> Vec<Seen> {
    server
        .seen()
        .into_iter()
        .filter(|seen| seen.method == "POST" && seen.path == "/user/repos")
        .collect()
}

fn new_project(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::tempdir().unwrap();
    let project = folder.path().canonicalize().unwrap();
    std::fs::create_dir(project.join(".crazyagent")).unwrap();
    for (path, text) in files {
        let path = project.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    (folder, project)
}

fn done_on(job: &GitHubPublishJob) -> (String, Vec<GitHubRuleOutcome>) {
    match &job.step {
        GitHubPublishStep::Done {
            full_name,
            outcomes,
        } => (full_name.clone(), outcomes.clone()),
        other => panic!("not done: {other:?} {:?}", job.stopped),
    }
}

#[test]
fn publish_job_runs_from_the_ignore_lines_to_done_and_records_each_step() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new();
    let secret_text = token();
    let (_served, root, _stand, server) = publish_stand(&secret_text, &scratch, &["tool"]);
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let (_folder, project) = new_project(&[("main.rs", "fn main() {}\n")]);
    let owner_home = tempfile::tempdir().unwrap();
    assert_eq!(account.publish_status(&project).unwrap(), None);
    let started = account
        .publish_start(&project, &plan("tool"), no_draft)
        .unwrap();
    assert_eq!(
        started,
        GitHubPublishJob {
            project: project.clone(),
            plan: plan("tool"),
            lines: Vec::new(),
            written: Vec::new(),
            step: GitHubPublishStep::Lines {
                lines: baseline(),
                draft: GitHubIgnoreDraft::Baseline {
                    reason: crate::instruction_cleanup::UNAVAILABLE.into(),
                },
            },
            stopped: None,
        }
    );
    assert!(!project.join(".git").exists());
    assert!(creates(&server).is_empty());
    assert_eq!(account.publish_status(&project).unwrap(), Some(started));
    let lines: Vec<String> = [".env*", "*.pem", "target/"].map(String::from).to_vec();
    let done = account
        .publish_approve(&project, owner_home.path(), &lines)
        .unwrap();
    let (full_name, outcomes) = done_on(&done);
    assert_eq!(full_name, "octo-cat/tool");
    assert_eq!(outcomes.len(), GitHubRule::ALL.len() - 2);
    assert_eq!(done.stopped, None);
    assert_eq!(done.lines, lines);
    assert_eq!(done.written, [".gitignore"]);
    assert_eq!(
        std::fs::read_to_string(project.join(".gitignore")).unwrap(),
        "/.crazyagent/\n/graphify-out/\n.env*\n*.pem\ntarget/\n"
    );
    assert_eq!(
        git_out(&["ls-tree", "-r", "--name-only", "HEAD"], &project),
        ".gitignore\nmain.rs\n"
    );
    let bare = root.join("octo-cat/tool.git");
    assert_eq!(
        git_out(&["rev-parse", "refs/heads/main"], &bare),
        git_out(&["rev-parse", "HEAD"], &project)
    );
    assert_eq!(
        git_out(&["config", "--get", "remote.origin.url"], &project),
        format!("http://127.0.0.1:{}/octo-cat/tool.git\n", server.port)
    );
    let made = creates(&server);
    assert_eq!(made.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&made[0].body).unwrap();
    assert_eq!(body["name"], "tool");
    assert_eq!(body["private"], true);
    assert_eq!(
        account.publish_status(&project).unwrap(),
        Some(done.clone())
    );
    let records = scratch
        .root
        .path()
        .canonicalize()
        .unwrap()
        .join("github-publish");
    let files: Vec<PathBuf> = std::fs::read_dir(&records)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().unwrap().to_str().unwrap().to_owned();
    assert!(
        name.len() == 69
            && name.ends_with(".json")
            && name[..64].bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{name}"
    );
    let metadata = std::fs::symlink_metadata(&files[0]).unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert!(
        !std::fs::read_to_string(&files[0])
            .unwrap()
            .contains(&secret_text)
    );
    assert_eq!(
        account
            .publish_continue(&project, owner_home.path())
            .unwrap(),
        done
    );
    assert_eq!(
        account
            .publish_start(&project, &plan("tool"), no_draft)
            .unwrap_err()
            .0,
        "github_publish_on_github"
    );
}

#[test]
fn publish_job_proposes_the_agents_lines_after_the_baseline_and_never_shows_it_a_file() {
    let scratch = Scratch::new();
    let secret_text = token();
    let (_served, _root, _stand, server) = publish_stand(&secret_text, &scratch, &["tool"]);
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let (_folder, project) = new_project(&[
        ("src/main.rs", "fn main() { let word = \"PLAINWORD\"; }\n"),
        (".idea/workspace.xml", "<x/>\n"),
        (".gitignore", "target/\n.env*\n"),
    ]);
    let asked = std::cell::RefCell::new(String::new());
    let reply = "node_modules/\n# deps\n!.env*\n*\n.idea/\ntarget/\nSorry, no more.\n*.log\n";
    let started = account
        .publish_start(&project, &plan("tool"), |prompt| {
            *asked.borrow_mut() = prompt.to_owned();
            Ok(reply.to_owned())
        })
        .unwrap();
    assert_eq!(
        started.step,
        GitHubPublishStep::Lines {
            lines: [
                "*.pem",
                "*.key",
                ".DS_Store",
                "node_modules/",
                ".idea/",
                "*.log"
            ]
            .map(String::from)
            .to_vec(),
            draft: GitHubIgnoreDraft::Agent,
        }
    );
    let asked = asked.into_inner();
    assert!(asked.contains("src/main.rs\n"), "{asked}");
    assert!(asked.contains(".idea/\n"), "{asked}");
    assert!(asked.contains("target/\n"), "{asked}");
    assert!(!asked.contains("PLAINWORD"), "{asked}");
    assert!(!project.join(".git").exists());
    assert_eq!(
        std::fs::read_to_string(project.join(".gitignore")).unwrap(),
        "target/\n.env*\n"
    );
    assert_eq!(account.publish_status(&project).unwrap(), Some(started));
    assert!(creates(&server).is_empty());
}

#[test]
fn publish_job_falls_back_to_the_baseline_and_says_why_no_agent_drafted() {
    let scratch = Scratch::new();
    let secret_text = token();
    let (_served, _root, _stand, server) = publish_stand(&secret_text, &scratch, &["tool"]);
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let (_folder, project) = new_project(&[("main.rs", "fn main() {}\n")]);
    let started = account
        .publish_start(&project, &plan("tool"), |_| {
            Err("instructions_cleanup_no_account")
        })
        .unwrap();
    assert_eq!(
        started.step,
        GitHubPublishStep::Lines {
            lines: baseline(),
            draft: GitHubIgnoreDraft::Baseline {
                reason: "instructions_cleanup_no_account".into(),
            },
        }
    );
    assert!(creates(&server).is_empty());
}

#[test]
fn a_publish_record_from_before_the_draft_still_loads() {
    let step: Option<GitHubPublishStep> =
        serde_json::from_str(r#"{"step":"lines","lines":[".env*"]}"#).ok();
    assert_eq!(
        step,
        Some(GitHubPublishStep::Lines {
            lines: vec![".env*".into()],
            draft: GitHubIgnoreDraft::Baseline {
                reason: "github_publish_draft_not_asked".into(),
            },
        })
    );
}

#[test]
fn publish_job_waits_on_a_secret_until_the_file_is_left_out() {
    let scratch = Scratch::new();
    let secret_text = token();
    let (_served, _root, _stand, server) = publish_stand(&secret_text, &scratch, &["tool"]);
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let leaked = format!("deploy = 1\ntoken {LEAKED}\n");
    let (_folder, project) = new_project(&[("main.rs", "fn main() {}\n"), ("keys.txt", &leaked)]);
    let owner_home = tempfile::tempdir().unwrap();
    account
        .publish_start(&project, &plan("tool"), no_draft)
        .unwrap();
    let blocked = account
        .publish_approve(&project, owner_home.path(), &baseline())
        .unwrap();
    assert_eq!(
        blocked.step,
        GitHubPublishStep::Blocked {
            scan: GitHubSecretScan {
                findings: vec![GitHubSecretFinding {
                    path: "keys.txt".into(),
                    line: 2,
                    kind: GitHubSecretKind::Token,
                }],
                truncated: false,
            },
        }
    );
    assert_eq!(blocked.stopped, None);
    assert!(creates(&server).is_empty());
    assert_eq!(
        account
            .publish_continue(&project, owner_home.path())
            .unwrap_err()
            .0,
        "github_publish_step_invalid"
    );
    for bad in ["main.rs", "../keys.txt", "*.txt", "missing.txt"] {
        assert_eq!(
            account
                .publish_exclude(&project, owner_home.path(), bad)
                .unwrap_err()
                .0,
            "github_publish_exclude_invalid",
            "{bad}"
        );
    }
    assert_eq!(account.publish_status(&project).unwrap(), Some(blocked));
    let done = account
        .publish_exclude(&project, owner_home.path(), "keys.txt")
        .unwrap();
    let (full_name, _) = done_on(&done);
    assert_eq!(full_name, "octo-cat/tool");
    assert_eq!(done.lines.last().map(String::as_str), Some("/keys.txt"));
    assert!(
        std::fs::read_to_string(project.join(".gitignore"))
            .unwrap()
            .ends_with("\n/keys.txt\n")
    );
    assert_eq!(
        git_out(&["ls-tree", "-r", "--name-only", "HEAD"], &project),
        ".gitignore\nmain.rs\n"
    );
    assert_eq!(
        std::fs::read_to_string(project.join("keys.txt")).unwrap(),
        leaked
    );
    assert_eq!(creates(&server).len(), 1);
}

#[test]
fn publish_job_checks_a_fixed_file_again_and_goes_on() {
    let scratch = Scratch::new();
    let secret_text = token();
    let (_served, _root, _stand, server) = publish_stand(&secret_text, &scratch, &["tool"]);
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let (_folder, project) = new_project(&[("keys.txt", &format!("token {LEAKED}\n"))]);
    let owner_home = tempfile::tempdir().unwrap();
    account
        .publish_start(&project, &plan("tool"), no_draft)
        .unwrap();
    let blocked = account
        .publish_approve(&project, owner_home.path(), &baseline())
        .unwrap();
    assert!(matches!(blocked.step, GitHubPublishStep::Blocked { .. }));
    assert_eq!(
        account
            .publish_recheck(&project, owner_home.path())
            .unwrap(),
        blocked
    );
    std::fs::write(project.join("keys.txt"), "token removed\n").unwrap();
    let done = account
        .publish_recheck(&project, owner_home.path())
        .unwrap();
    done_on(&done);
    assert_eq!(
        git_out(&["ls-tree", "-r", "--name-only", "HEAD"], &project),
        ".gitignore\nkeys.txt\n"
    );
    assert_eq!(creates(&server).len(), 1);
}

#[test]
fn publish_job_continued_after_a_stop_before_its_commit_checks_the_project_again() {
    let scratch = Scratch::new();
    let secret_text = token();
    let (_served, _root, _stand, server) = publish_stand(&secret_text, &scratch, &["tool"]);
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let (_folder, project) = new_project(&[("main.rs", "fn main() {}\n")]);
    let owner_home = tempfile::tempdir().unwrap();
    git(&["init", "--quiet", "--initial-branch=main"], &project);
    git(&["config", "hook.mark.event", "pre-commit"], &project);
    git(&["config", "hook.mark.command", "/usr/bin/true"], &project);
    account
        .publish_start(&project, &plan("tool"), no_draft)
        .unwrap();
    let stopped = account
        .publish_approve(&project, owner_home.path(), &baseline())
        .unwrap();
    assert_eq!(stopped.step, GitHubPublishStep::Checked);
    assert_eq!(stopped.stopped.as_deref(), Some("github_publish_hooks_set"));
    git(&["config", "--remove-section", "hook.mark"], &project);
    std::fs::write(project.join("keys.txt"), format!("token {LEAKED}\n")).unwrap();
    let continued = account
        .publish_continue(&project, owner_home.path())
        .unwrap();
    assert_eq!(
        continued.step,
        GitHubPublishStep::Blocked {
            scan: GitHubSecretScan {
                findings: vec![GitHubSecretFinding {
                    path: "keys.txt".into(),
                    line: 1,
                    kind: GitHubSecretKind::Token,
                }],
                truncated: false,
            },
        }
    );
    assert_eq!(continued.stopped, None);
    assert!(creates(&server).is_empty());
    assert_eq!(account.publish_status(&project).unwrap(), Some(continued));
}

#[test]
fn publish_job_started_again_keeps_the_paths_publish_wrote_for_its_commit() {
    let scratch = Scratch::new();
    let secret_text = token();
    let (_served, _root, _stand, server) = publish_stand(&secret_text, &scratch, &["tool"]);
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let (_folder, project) = new_project(&[("main.rs", "fn main() {}\n")]);
    let owner_home = tempfile::tempdir().unwrap();
    git(&["init", "--quiet", "--initial-branch=main"], &project);
    git(&["add", "main.rs"], &project);
    git(&["commit", "--quiet", "-m", "first"], &project);
    git(&["config", "hook.mark.event", "pre-commit"], &project);
    git(&["config", "hook.mark.command", "/usr/bin/true"], &project);
    account
        .publish_start(&project, &plan("tool"), no_draft)
        .unwrap();
    let stopped = account
        .publish_approve(&project, owner_home.path(), &baseline())
        .unwrap();
    assert_eq!(stopped.step, GitHubPublishStep::Checked);
    assert_eq!(stopped.stopped.as_deref(), Some("github_publish_hooks_set"));
    assert_eq!(stopped.written, [".gitignore"]);
    git(&["config", "--remove-section", "hook.mark"], &project);
    let restarted = account
        .publish_start(&project, &plan("tool"), no_draft)
        .unwrap();
    assert_eq!(restarted.written, [".gitignore"]);
    let done = account
        .publish_approve(&project, owner_home.path(), &baseline())
        .unwrap();
    let (full_name, _) = done_on(&done);
    assert_eq!(full_name, "octo-cat/tool");
    assert_eq!(
        git_out(&["ls-tree", "-r", "--name-only", "HEAD"], &project),
        ".gitignore\nmain.rs\n"
    );
    assert_eq!(creates(&server).len(), 1);
}

#[test]
fn publish_job_checks_the_files_publish_wrote_on_a_project_with_history() {
    let scratch = Scratch::new();
    let secret_text = token();
    let (_served, _root, _stand, server) = publish_stand(&secret_text, &scratch, &["tool"]);
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let (_folder, project) = new_project(&[("main.rs", "fn main() {}\n")]);
    let owner_home = tempfile::tempdir().unwrap();
    git(&["init", "--quiet", "--initial-branch=main"], &project);
    git(&["add", "main.rs"], &project);
    git(&["commit", "--quiet", "-m", "first"], &project);
    git(&["config", "hook.mark.event", "pre-commit"], &project);
    git(&["config", "hook.mark.command", "/usr/bin/true"], &project);
    account
        .publish_start(&project, &plan("tool"), no_draft)
        .unwrap();
    let stopped = account
        .publish_approve(&project, owner_home.path(), &baseline())
        .unwrap();
    assert_eq!(stopped.step, GitHubPublishStep::Checked);
    assert_eq!(stopped.written, [".gitignore"]);
    git(&["config", "--remove-section", "hook.mark"], &project);
    let ignore = project.join(".gitignore");
    let mut text = std::fs::read_to_string(&ignore).unwrap();
    text.push_str(&format!("token {LEAKED}\n"));
    std::fs::write(&ignore, text).unwrap();
    let continued = account
        .publish_continue(&project, owner_home.path())
        .unwrap();
    assert!(
        matches!(
            &continued.step,
            GitHubPublishStep::Blocked { scan }
                if scan.findings.len() == 1
                    && scan.findings[0].path == ".gitignore"
                    && scan.findings[0].kind == GitHubSecretKind::Token
        ),
        "{continued:?}"
    );
    assert!(creates(&server).is_empty());
}

fn hooked_owner_project(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    let (folder, project) = new_project(files);
    git(&["init", "--quiet", "--initial-branch=main"], &project);
    git(&["add", "-A"], &project);
    git(&["commit", "--quiet", "-m", "first"], &project);
    git(&["config", "hook.mark.event", "pre-commit"], &project);
    git(&["config", "hook.mark.command", "/usr/bin/true"], &project);
    (folder, project)
}

#[test]
fn publish_job_stopped_at_created_pushes_the_moved_head_after_a_new_check() {
    let scratch = Scratch::new();
    let secret_text = token();
    let (_served, root, stand, server) = publish_stand(&secret_text, &scratch, &["tool"]);
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let (_folder, project) = new_project(&[(".github/workflows/ci.yml", "on: push\n")]);
    let owner_home = tempfile::tempdir().unwrap();
    stand.lock().unwrap().scopes = Some("repo, read:org");
    account
        .publish_start(&project, &plan("tool"), no_draft)
        .unwrap();
    let stopped = account
        .publish_approve(&project, owner_home.path(), &baseline())
        .unwrap();
    assert!(
        matches!(stopped.step, GitHubPublishStep::Created { .. }),
        "{stopped:?}"
    );
    std::fs::write(project.join("notes.md"), "later\n").unwrap();
    git(&["add", "notes.md"], &project);
    git(&["commit", "--quiet", "-m", "later"], &project);
    stand.lock().unwrap().scopes = Some("repo, workflow");
    let done = account
        .publish_continue(&project, owner_home.path())
        .unwrap();
    assert!(
        matches!(done.step, GitHubPublishStep::Done { .. }),
        "{done:?}"
    );
    assert_eq!(done_on(&done).0, "octo-cat/tool");
    assert_eq!(
        git_out(
            &["rev-parse", "refs/heads/main"],
            &root.join("octo-cat/tool.git")
        ),
        git_out(&["rev-parse", "HEAD"], &project)
    );
    assert!(git_out(&["ls-tree", "-r", "--name-only", "HEAD"], &project).contains("notes.md\n"));
    assert_eq!(creates(&server).len(), 1);
}

#[test]
fn publish_job_discarded_short_of_done_starts_fresh_and_names_the_repository_left() {
    let scratch = Scratch::new();
    let secret_text = token();
    let (_served, root, stand, server) = publish_stand(&secret_text, &scratch, &["tool"]);
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let (_folder, project) = new_project(&[(".github/workflows/ci.yml", "on: push\n")]);
    let owner_home = tempfile::tempdir().unwrap();
    assert_eq!(
        account.publish_discard(&project).map_err(|error| error.0),
        Err("github_publish_not_started".to_owned())
    );
    stand.lock().unwrap().scopes = Some("repo, read:org");
    account
        .publish_start(&project, &plan("tool"), no_draft)
        .unwrap();
    let stopped = account
        .publish_approve(&project, owner_home.path(), &baseline())
        .unwrap();
    assert!(
        matches!(stopped.step, GitHubPublishStep::Created { .. }),
        "{stopped:?}"
    );
    let bare = root.join("octo-cat/tool.git");
    std::fs::remove_dir_all(&bare).unwrap();
    stand.lock().unwrap().scopes = Some("repo, workflow");
    let failed = account
        .publish_continue(&project, owner_home.path())
        .unwrap();
    assert!(
        matches!(failed.step, GitHubPublishStep::Created { .. }),
        "{failed:?}"
    );
    assert_eq!(
        account.publish_discard(&project).map_err(|error| error.0),
        Ok(Some("octo-cat/tool".to_owned()))
    );
    assert_eq!(account.publish_status(&project).unwrap(), None);
    std::fs::create_dir_all(&bare).unwrap();
    git(&["init", "--quiet", "--bare"], &bare);
    account
        .publish_start(&project, &plan("tool"), no_draft)
        .unwrap();
    let done = account
        .publish_approve(&project, owner_home.path(), &baseline())
        .unwrap();
    assert!(
        matches!(done.step, GitHubPublishStep::Done { .. }),
        "{done:?}"
    );
    assert_eq!(done_on(&done).0, "octo-cat/tool");
    assert_eq!(
        git_out(&["rev-parse", "refs/heads/main"], &bare),
        git_out(&["rev-parse", "HEAD"], &project)
    );
    assert_eq!(creates(&server).len(), 2);
    assert_eq!(
        account.publish_discard(&project).map_err(|error| error.0),
        Err("github_publish_step_invalid".to_owned())
    );
}

#[test]
fn publish_job_refuses_a_written_path_that_became_a_folder() {
    let scratch = Scratch::new();
    let secret_text = token();
    let (_served, _root, _stand, server) = publish_stand(&secret_text, &scratch, &["tool"]);
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let (_folder, project) = hooked_owner_project(&[("main.rs", "fn main() {}\n")]);
    let owner_home = tempfile::tempdir().unwrap();
    account
        .publish_start(&project, &plan("tool"), no_draft)
        .unwrap();
    let stopped = account
        .publish_approve(&project, owner_home.path(), &baseline())
        .unwrap();
    assert_eq!(stopped.step, GitHubPublishStep::Checked);
    assert_eq!(stopped.written, [".gitignore"]);
    git(&["config", "--remove-section", "hook.mark"], &project);
    let head = git_out(&["rev-parse", "HEAD"], &project);
    std::fs::remove_file(project.join(".gitignore")).unwrap();
    std::fs::create_dir(project.join(".gitignore")).unwrap();
    std::fs::write(
        project.join(".gitignore/keys.txt"),
        format!("token {LEAKED}\n"),
    )
    .unwrap();
    let continued = account
        .publish_continue(&project, owner_home.path())
        .unwrap();
    assert_eq!(continued.step, GitHubPublishStep::Checked);
    assert_eq!(
        continued.stopped.as_deref(),
        Some("github_publish_file_invalid")
    );
    assert_eq!(git_out(&["rev-parse", "HEAD"], &project), head);
    assert!(creates(&server).is_empty());
}

#[test]
fn publish_job_goes_on_when_a_file_publish_wrote_was_deleted_since() {
    let scratch = Scratch::new();
    let secret_text = token();
    let (_served, _root, _stand, server) = publish_stand(&secret_text, &scratch, &["tool"]);
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let (_folder, project) = hooked_owner_project(&[("main.rs", "fn main() {}\n")]);
    let owner_home = tempfile::tempdir().unwrap();
    let public = GitHubPublishPlan {
        private: false,
        ..plan("tool")
    };
    account.publish_start(&project, &public, no_draft).unwrap();
    let stopped = account
        .publish_approve(&project, owner_home.path(), &baseline())
        .unwrap();
    assert_eq!(stopped.step, GitHubPublishStep::Checked);
    assert_eq!(stopped.written, [".gitignore", "SECURITY.md"]);
    git(&["config", "--remove-section", "hook.mark"], &project);
    std::fs::remove_file(project.join("SECURITY.md")).unwrap();
    account
        .publish_start(&project, &plan("tool"), no_draft)
        .unwrap();
    let done = account
        .publish_approve(&project, owner_home.path(), &baseline())
        .unwrap();
    assert!(
        matches!(done.step, GitHubPublishStep::Done { .. }),
        "{done:?}"
    );
    assert_eq!(done_on(&done).0, "octo-cat/tool");
    assert_eq!(
        git_out(&["ls-tree", "-r", "--name-only", "HEAD"], &project),
        ".gitignore\nmain.rs\n"
    );
    assert_eq!(creates(&server).len(), 1);
}

#[test]
fn publish_job_with_no_history_leaves_out_a_written_file_the_owner_excludes() {
    let scratch = Scratch::new();
    let secret_text = token();
    let (_served, _root, _stand, server) = publish_stand(&secret_text, &scratch, &["tool"]);
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let (_folder, project) = new_project(&[
        ("Cargo.toml", "[package]\nname = \"tool\"\n"),
        ("src/main.rs", "fn main() {}\n"),
    ]);
    let owner_home = tempfile::tempdir().unwrap();
    git(&["init", "--quiet", "--initial-branch=main"], &project);
    git(&["config", "hook.mark.event", "pre-commit"], &project);
    git(&["config", "hook.mark.command", "/usr/bin/true"], &project);
    account
        .publish_start(&project, &plan("tool"), no_draft)
        .unwrap();
    let stopped = account
        .publish_approve(&project, owner_home.path(), &baseline())
        .unwrap();
    assert_eq!(stopped.step, GitHubPublishStep::Checked);
    assert_eq!(stopped.written, [".gitignore", ".github/dependabot.yml"]);
    git(&["config", "--remove-section", "hook.mark"], &project);
    let dependabot = project.join(".github/dependabot.yml");
    let mut text = std::fs::read_to_string(&dependabot).unwrap();
    text.push_str(&format!("token {LEAKED}\n"));
    std::fs::write(&dependabot, text).unwrap();
    let blocked = account
        .publish_continue(&project, owner_home.path())
        .unwrap();
    assert!(
        matches!(blocked.step, GitHubPublishStep::Blocked { .. }),
        "{blocked:?}"
    );
    let done = account
        .publish_exclude(&project, owner_home.path(), ".github/dependabot.yml")
        .unwrap();
    assert!(
        matches!(done.step, GitHubPublishStep::Done { .. }),
        "{done:?}"
    );
    assert_eq!(done_on(&done).0, "octo-cat/tool");
    assert_eq!(
        git_out(&["ls-tree", "-r", "--name-only", "HEAD"], &project),
        ".gitignore\nCargo.toml\nsrc/main.rs\n"
    );
    assert_eq!(creates(&server).len(), 1);
}

#[test]
fn publish_job_stopped_at_create_goes_on_and_knows_the_repository_it_made() {
    let scratch = Scratch::new();
    let secret_text = token();
    let (_served, root, stand, server) = publish_stand(&secret_text, &scratch, &["tool"]);
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let (_folder, project) = new_project(&[("main.rs", "fn main() {}\n")]);
    let owner_home = tempfile::tempdir().unwrap();
    let before = now();
    account
        .publish_start(&project, &plan("tool"), no_draft)
        .unwrap();
    stand.lock().unwrap().create = 500;
    let stopped = account
        .publish_approve(&project, owner_home.path(), &baseline())
        .unwrap();
    let GitHubPublishStep::Creating { sent_at, committed } = &stopped.step else {
        panic!("{stopped:?}");
    };
    assert!(*sent_at >= before && *sent_at <= now(), "{sent_at}");
    assert_eq!(
        committed.commit,
        git_out(&["rev-parse", "HEAD"], &project).trim()
    );
    assert_eq!(stopped.stopped.as_deref(), Some("github_unreachable"));
    assert_eq!(
        account.publish_status(&project).unwrap(),
        Some(stopped.clone())
    );
    let refused = [
        (true, true, rfc3339(now())),
        (false, false, rfc3339(now())),
        (false, true, rfc3339(before - 3600)),
    ];
    for (branches, private, created_at) in refused {
        {
            let mut held = stand.lock().unwrap();
            held.create = 422;
            held.branches = branches;
            held.private = private;
            held.created_at = created_at.clone();
        }
        let taken = account
            .publish_continue(&project, owner_home.path())
            .unwrap();
        assert_eq!(
            taken.step, stopped.step,
            "{branches} {private} {created_at}"
        );
        assert_eq!(taken.stopped.as_deref(), Some("github_repo_name_taken"));
    }
    {
        let mut held = stand.lock().unwrap();
        held.branches = false;
        held.private = true;
        held.created_at = rfc3339(now());
    }
    let done = account
        .publish_continue(&project, owner_home.path())
        .unwrap();
    let (full_name, _) = done_on(&done);
    assert_eq!(full_name, "octo-cat/tool");
    assert_eq!(done.stopped, None);
    assert_eq!(
        git_out(
            &["rev-parse", "refs/heads/main"],
            &root.join("octo-cat/tool.git")
        ),
        git_out(&["rev-parse", "HEAD"], &project)
    );
    assert_eq!(creates(&server).len(), 5);
    assert_eq!(stand.lock().unwrap().recorded, [true; 5]);
}

#[test]
fn publish_job_needs_the_workflow_scope_for_a_project_with_workflows() {
    let scratch = Scratch::new();
    let secret_text = token();
    let (_served, root, stand, server) = publish_stand(&secret_text, &scratch, &["tool", "kit"]);
    let account = scratch.account(&server);
    account.use_token(secret(&secret_text)).unwrap();
    let workflow = [(".github/workflows/ci.yml", "on: push\n")];
    let (_folder, project) = new_project(&workflow);
    let owner_home = tempfile::tempdir().unwrap();
    stand.lock().unwrap().scopes = Some("repo, read:org");
    account
        .publish_start(&project, &plan("tool"), no_draft)
        .unwrap();
    let stopped = account
        .publish_approve(&project, owner_home.path(), &baseline())
        .unwrap();
    let GitHubPublishStep::Created {
        full_name, private, ..
    } = &stopped.step
    else {
        panic!("{stopped:?}");
    };
    assert_eq!(full_name, "octo-cat/tool");
    assert!(*private);
    assert_eq!(
        stopped.stopped.as_deref(),
        Some("github_publish_workflow_scope")
    );
    assert_eq!(stopped.written, [".gitignore", ".github/dependabot.yml"]);
    assert!(git_out(&["for-each-ref"], &root.join("octo-cat/tool.git")).is_empty());
    assert_eq!(
        account
            .publish_start(&project, &plan("tool"), no_draft)
            .unwrap_err()
            .0,
        "github_publish_in_progress"
    );
    stand.lock().unwrap().scopes = Some("repo, workflow");
    let done = account
        .publish_continue(&project, owner_home.path())
        .unwrap();
    let (_, outcomes) = done_on(&done);
    let checks = outcomes
        .iter()
        .find(|outcome| outcome.rule == GitHubRule::RequiredChecks)
        .unwrap();
    assert!(
        matches!(checks.state, GitHubRuleState::Waiting { .. }),
        "{checks:?}"
    );
    assert_eq!(creates(&server).len(), 1);
    stand.lock().unwrap().scopes = None;
    let (_other, unknown) = new_project(&workflow);
    account
        .publish_start(&unknown, &plan("kit"), no_draft)
        .unwrap();
    let done = account
        .publish_approve(&unknown, owner_home.path(), &baseline())
        .unwrap();
    assert_eq!(done_on(&done).0, "octo-cat/kit");
}

#[test]
fn publish_job_refuses_a_bad_plan_a_wrong_step_and_a_second_run() {
    let scratch = Scratch::new();
    let secret_text = token();
    let (_served, _root, _stand, server) = publish_stand(&secret_text, &scratch, &["tool"]);
    let account = scratch.account(&server);
    let (_folder, project) = new_project(&[("main.rs", "fn main() {}\n")]);
    let owner_home = tempfile::tempdir().unwrap();
    assert_eq!(
        account
            .publish_start(&project, &plan("tool"), no_draft)
            .unwrap_err()
            .0,
        "github_not_signed_in"
    );
    account.use_token(secret(&secret_text)).unwrap();
    let bad_plans = [
        (plan("../x"), "github_repo_name_invalid"),
        (
            GitHubPublishPlan {
                owner: "-x".into(),
                ..plan("tool")
            },
            "github_repo_name_invalid",
        ),
        (
            GitHubPublishPlan {
                description: "a\u{7}b".into(),
                ..plan("tool")
            },
            "github_repo_description_invalid",
        ),
    ];
    for (bad, code) in bad_plans {
        assert_eq!(
            account
                .publish_start(&project, &bad, no_draft)
                .unwrap_err()
                .0,
            code,
            "{bad:?}"
        );
    }
    assert_eq!(account.publish_status(&project).unwrap(), None);
    assert_eq!(
        account
            .publish_approve(&project, owner_home.path(), &baseline())
            .unwrap_err()
            .0,
        "github_publish_not_started"
    );
    assert_eq!(
        account
            .publish_continue(&project, owner_home.path())
            .unwrap_err()
            .0,
        "github_publish_not_started"
    );
    let started = account
        .publish_start(&project, &plan("tool"), no_draft)
        .unwrap();
    let engine_root = scratch.root.path().canonicalize().unwrap();
    let held = job::publishing(&engine_root, &project).unwrap();
    assert_eq!(
        job::publishing(&engine_root, &project).err().unwrap().0,
        "github_publish_running"
    );
    assert_eq!(
        account
            .publish_start(&project, &plan("tool"), no_draft)
            .unwrap_err()
            .0,
        "github_publish_running"
    );
    assert_eq!(
        account
            .publish_approve(&project, owner_home.path(), &baseline())
            .unwrap_err()
            .0,
        "github_publish_running"
    );
    drop(held);
    assert_eq!(
        account
            .publish_recheck(&project, owner_home.path())
            .unwrap_err()
            .0,
        "github_publish_step_invalid"
    );
    assert_eq!(
        account
            .publish_exclude(&project, owner_home.path(), "main.rs")
            .unwrap_err()
            .0,
        "github_publish_step_invalid"
    );
    assert_eq!(
        account
            .publish_continue(&project, owner_home.path())
            .unwrap_err()
            .0,
        "github_publish_step_invalid"
    );
    assert_eq!(
        account
            .publish_approve(&project, owner_home.path(), &["a\nb".to_owned()])
            .unwrap_err()
            .0,
        "github_publish_ignore_invalid"
    );
    assert_eq!(account.publish_status(&project).unwrap(), Some(started));
    assert!(!project.join(".git").exists());
    assert!(creates(&server).is_empty());
    let done = account
        .publish_approve(&project, owner_home.path(), &baseline())
        .unwrap();
    done_on(&done);
    assert_eq!(
        account
            .publish_approve(&project, owner_home.path(), &baseline())
            .unwrap_err()
            .0,
        "github_publish_step_invalid"
    );
}
