#![cfg(unix)]

mod e2e_support;

use crazyagent::credentials::{SecretString, Store, platform_store, reference};
use crazyagent::engine::owner::{OwnerClient, OwnerClientKind, OwnerResult};
use crazyagent::github::{Endpoints, GitHub, GitHubMethod, namespace};
use crazyagent::projects_home::FoundState;
use crazyagent::protocol::CredentialRef;
use e2e_support::{Engine, Owner, git, hex, named, repository, scratch};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const STORE_PREREQUISITE: &str = "UNAVAILABLE: this computer's credential store for the signed-in \
user (macOS Keychain, Linux Secret Service or Windows Credential Manager)";
const README: &str = "hello from the loopback stand-in\n";
const REQUEST_BYTES_MAX: u64 = 16 * 1024;
const OWNER_TIME: Duration = Duration::from_secs(10);

struct Forget {
    store: Arc<dyn Store>,
    item: CredentialRef,
}

impl Drop for Forget {
    fn drop(&mut self) {
        let _ = self.store.delete(&self.item);
    }
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let joined = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |value, (index, byte)| {
                value | (u32::from(*byte) << (16 - 8 * index))
            });
        for index in 0..4 {
            if index <= chunk.len() {
                text.push(char::from(
                    TABLE[((joined >> (18 - 6 * index)) & 63) as usize],
                ));
            } else {
                text.push('=');
            }
        }
    }
    text
}

fn route(
    path: &str,
    authorization: Option<&str>,
    served: &Path,
    token: &str,
) -> (&'static str, String, Vec<u8>) {
    let json = "Content-Type: application/json\r\n".to_owned();
    if path == "/user" {
        return if authorization == Some(format!("Bearer {token}").as_str()) {
            ("200 OK", json, br#"{"login":"octo-cat"}"#.to_vec())
        } else {
            (
                "401 Unauthorized",
                json,
                br#"{"message":"Bad credentials"}"#.to_vec(),
            )
        };
    }
    let basic = format!(
        "Basic {}",
        base64(format!("x-access-token:{token}").as_bytes())
    );
    if authorization != Some(basic.as_str()) {
        return (
            "401 Unauthorized",
            "WWW-Authenticate: Basic realm=\"git\"\r\n".into(),
            Vec::new(),
        );
    }
    let relative = path.trim_start_matches('/');
    if relative
        .split('/')
        .any(|part| part == ".." || part.is_empty())
    {
        return ("404 Not Found", String::new(), Vec::new());
    }
    match std::fs::read(served.join(relative)) {
        Ok(bytes) => ("200 OK", String::new(), bytes),
        Err(_) => ("404 Not Found", String::new(), Vec::new()),
    }
}

fn answer(mut stream: TcpStream, served: &Path, token: &str) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let Ok(copy) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(copy).take(REQUEST_BYTES_MAX);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let path = line
        .split(' ')
        .nth(1)
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_owned();
    let mut authorization = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("authorization")
        {
            authorization = Some(value.trim().to_owned());
        }
    }
    let (status, extra, body) = route(&path, authorization.as_deref(), served, token);
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
}

fn stand_in(served: PathBuf, token: String) -> u16 {
    let listener = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in crazyagent::spawn_gate::incoming_tcp_std(&listener).flatten() {
            answer(stream, &served, &token);
        }
    });
    port
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

async fn github(engine: &Engine) -> OwnerClient {
    OwnerClient::discover(engine.path(), OwnerClientKind::Tui, OWNER_TIME)
        .await
        .unwrap()
        .0
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clone_through_the_engine_credential_helper_lands_in_the_home_with_no_token_on_disk() {
    let mut engine = Engine::start("ca-e2e-clone-");
    let disk = scratch("ca-e2e-git-");
    let home = disk.path().join("Projects");
    std::fs::create_dir(&home).unwrap();
    let work = disk.path().join("work");
    repository(&work, "README.md", README);
    let served = disk.path().join("served");
    let bare = served.join("octo-cat").join("tool.git");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    git(
        disk.path(),
        &[
            "clone",
            "--quiet",
            "--bare",
            work.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    git(&bare, &["update-server-info"]);
    let token = format!("gho_e2e{}", hex(18));
    let port = stand_in(served, token.clone());

    let mut owner = Owner::attach(&engine).await;
    assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
    owner.controller.detach();

    let client = github(&engine).await;
    assert!(!client.github_status().await.unwrap().signed_in);
    assert_eq!(
        client
            .clone_repo("octo-cat/tool".into())
            .await
            .unwrap_err()
            .0,
        "github_not_signed_in"
    );

    let space = namespace(engine.path());
    let store = platform_store(&space).expect(STORE_PREREQUISITE);
    let kept = Forget {
        store: Arc::clone(&store),
        item: reference(&format!("{space}.github"), "token").unwrap(),
    };
    let account = GitHub::new(
        engine.path(),
        space.clone(),
        Arc::clone(&store),
        Endpoints::loopback(port),
    );
    let signed = account
        .use_token(SecretString::new(token.as_bytes().to_vec()).unwrap())
        .unwrap();
    assert!(signed.signed_in);
    let status = client.github_status().await.unwrap();
    assert!(
        status.signed_in,
        "the engine reads the sign-in kept under its own namespace: {status:?}"
    );
    assert_eq!(status.login.as_deref(), Some("octo-cat"));
    assert_eq!(status.method, GitHubMethod::Token);
    drop(client);

    let receipt = account.clone_repo(&home, "octo-cat/tool").unwrap();
    let project = home.join("tool");
    assert_eq!(receipt.full_name, "octo-cat/tool");
    assert_eq!(receipt.path, project.to_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(project.join("README.md")).unwrap(),
        README
    );
    assert_eq!(
        String::from_utf8(git(&project, &["log", "--format=%s"]).stdout).unwrap(),
        "first\n"
    );
    let config = std::fs::read_to_string(project.join(".git").join("config")).unwrap();
    assert!(
        config.contains(&format!("url = http://127.0.0.1:{port}/octo-cat/tool.git")),
        "{config}"
    );
    for kept_text in [token.as_str(), "x-access-token", "@127.0.0.1"] {
        assert!(!config.contains(kept_text), "{kept_text} in {config}");
    }
    assert!(
        !holds(&project, token.as_bytes()),
        "no file in the clone holds the token"
    );

    let mut owner = Owner::attach(&engine).await;
    let found = owner
        .found_until("the clone shows on Home as Found", |found| {
            named(found, "tool").is_some()
        })
        .await;
    let entry = named(&found, "tool").unwrap();
    assert_eq!(entry.state, FoundState::Found);
    assert!(entry.git && entry.project_id.is_none(), "{entry:?}");
    owner.controller.detach();

    let client = github(&engine).await;
    assert!(!client.github_sign_out().await.unwrap().signed_in);
    assert_eq!(
        store.get(&kept.item).unwrap_err().0,
        "credential_missing",
        "signing out deletes the kept token"
    );
    drop(client);
    let stderr = engine.stop();
    assert!(!stderr.contains(&token));
    assert!(
        !holds(engine.path(), token.as_bytes()),
        "the engine root keeps only a reference to the token"
    );
}
