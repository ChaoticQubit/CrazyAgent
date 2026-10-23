use crazyagent::accounts::{AccountDraft, AccountView, Provider, SignInMethod};
use crazyagent::engine::owner::{AccountSecret, OwnerClient, OwnerClientKind};
use std::fs::File;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct Engine {
    child: Child,
    stdout: PathBuf,
}

impl Engine {
    fn start(root: &Path, run: &str) -> Self {
        let stdout = root.join(format!("{run}.stdout"));
        let child = Command::new(env!("CARGO_BIN_EXE_crazyagent"))
            .arg("engine")
            .arg(root)
            .arg(root.join("global.toml"))
            .arg(root.join("project.toml"))
            .arg("engine.sock")
            .arg("--writable-settings")
            .stdin(Stdio::null())
            .stdout(File::create(&stdout).unwrap())
            .stderr(File::create(root.join(format!("{run}.stderr"))).unwrap())
            .spawn()
            .unwrap();
        let mut engine = Self { child, stdout };
        let deadline = Instant::now() + Duration::from_secs(20);
        while std::fs::read(&engine.stdout).unwrap() != b"ready\n" {
            assert!(
                engine.child.try_wait().unwrap().is_none(),
                "engine exited before ready"
            );
            assert!(Instant::now() < deadline, "engine readiness deadline");
            std::thread::sleep(Duration::from_millis(20));
        }
        engine
    }

    fn stop(&mut self) {
        let terminated = Command::new("/bin/kill")
            .args(["-s", "TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(terminated.success());
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "engine shutdown deadline");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        if metadata.is_dir() {
            found.extend(files(&path));
        } else if metadata.is_file() {
            found.push(path);
        }
    }
    found
}

fn holders(root: &Path, secret: &[u8]) -> Vec<PathBuf> {
    files(root)
        .into_iter()
        .filter(|path| {
            std::fs::read(path)
                .is_ok_and(|bytes| bytes.windows(secret.len()).any(|window| window == secret))
        })
        .collect()
}

async fn client(root: &Path) -> OwnerClient {
    OwnerClient::discover(root, OwnerClientKind::Desktop, Duration::from_secs(20))
        .await
        .unwrap()
        .0
}

async fn all_accounts(client: &OwnerClient) -> Vec<AccountView> {
    let mut accounts = Vec::new();
    let mut after = None;
    loop {
        let page = client.list_accounts(after, 200).await.unwrap();
        accounts.extend(page.accounts);
        match page.next {
            Some(next) => after = Some(next),
            None => return accounts,
        }
    }
}

async fn serve_models(listener: &TcpListener) -> String {
    let (mut stream, request) = loop {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let size = stream.read(&mut buffer).await.unwrap();
            if size == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..size]);
        }
        if !request.is_empty() {
            break (stream, request);
        }
    };
    let body = r#"{"data":[{"id":"lab-model"}]}"#;
    let reply = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(reply.as_bytes()).await.unwrap();
    stream.shutdown().await.unwrap();
    String::from_utf8(request).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_engine_binary_keeps_accounts_and_keys_across_restart() {
    let root = tempfile::Builder::new()
        .prefix("ca-bacct-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = std::fs::canonicalize(root.path()).unwrap();
    let (_guard, root) = (root, path);
    let global = root.join("global.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 5000\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let secret = format!("sk-binary-{}", std::process::id());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lab_url = format!(
        "http://127.0.0.1:{}/v1",
        listener.local_addr().unwrap().port()
    );

    let mut first = Engine::start(&root, "first");
    let owner = client(&root).await;
    let lab = owner
        .add_account(
            AccountDraft {
                provider: Provider::OpenaiCompatible,
                method: SignInMethod::Url,
                name: "Lab server".into(),
                url: Some(lab_url.clone()),
                model: Some("lab-model".into()),
            },
            Some(AccountSecret::new(secret.as_bytes().to_vec()).unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(lab.status_label, "Key saved");
    let local = owner
        .add_account(
            AccountDraft {
                provider: Provider::Local,
                method: SignInMethod::Url,
                name: "Ollama".into(),
                url: None,
                model: Some("qwen2.5:0.5b".into()),
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(local.status_label, "Signed in");
    let claude = owner
        .add_account(
            AccountDraft {
                provider: Provider::Claude,
                method: SignInMethod::Subscription,
                name: "Work Claude".into(),
                url: None,
                model: None,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(claude.status_label, "Not signed in");
    drop(owner);
    first.stop();

    let mut second = Engine::start(&root, "second");
    let owner = client(&root).await;
    let listed = all_accounts(&owner).await;
    let shown: Vec<(&str, &str, &str)> = listed
        .iter()
        .map(|account| {
            (
                account.name.as_str(),
                account.provider_name.as_str(),
                account.status_label.as_str(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("Lab server", "Any OpenAI-compatible URL", "Key saved"),
            ("Ollama", "Local models (Ollama)", "Signed in"),
            ("Work Claude", "Claude", "Not signed in"),
        ]
    );
    let (request, checked) =
        tokio::join!(serve_models(&listener), owner.check_account(lab.id.clone()));
    assert!(
        request.contains(&format!("\r\nAuthorization: Bearer {secret}\r\n")),
        "the restarted binary must read the saved key"
    );
    assert_eq!(checked.unwrap().models, ["lab-model"]);
    drop(owner);
    second.stop();

    let held = holders(&root, secret.as_bytes());
    assert_eq!(
        held.len(),
        1,
        "only the private key file may hold the key, not config, database, stdout or stderr: {held:?}"
    );
    assert!(held[0].parent().unwrap().ends_with("account-keys"));
}
