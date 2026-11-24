#![cfg(unix)]

mod e2e_support;
#[cfg(target_os = "macos")]
mod keychain_support;

#[cfg(target_os = "macos")]
use crazyagent::credentials::Keychain;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
use crazyagent::credentials::platform_store;
use crazyagent::engine::owner::{OwnerClient, OwnerClientKind, OwnerResult};
use crazyagent::github::{Endpoints, GitHub, GitHubMethod, namespace};
use crazyagent::projects_home::FoundState;
use e2e_support::{Engine, Owner, git, hex, named, repository, scratch};
use kernel_security::secret::SecretString;
#[cfg(target_os = "linux")]
use kernel_security::store::os_keyring::OsKeyring;
use kernel_security::store::{SecretRef, Store, reference};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[cfg(not(target_os = "macos"))]
const STORE_PREREQUISITE: &str = "UNAVAILABLE: this computer's credential store for the signed-in \
user (macOS Keychain, Linux Secret Service or Windows Credential Manager)";
const README: &str = "hello from the loopback stand-in\n";
const REQUEST_BYTES_MAX: u64 = 16 * 1024;
const OWNER_TIME: Duration = Duration::from_secs(10);

struct Forget {
    store: Arc<dyn Store>,
    item: SecretRef,
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

/// One request the loopback server read: its method, path, query, the headers the routes
/// read and the body.
struct Request {
    method: String,
    path: String,
    query: String,
    authorization: Option<String>,
    content_type: Option<String>,
    content_encoding: Option<String>,
    git_protocol: Option<String>,
    body: Vec<u8>,
}

/// What the loopback server answers: `/user` stands in for the account API, and every other
/// path goes, behind the Basic `x-access-token` check, to the real `git http-backend`.
/// Inputs: the request, the served folder, the token and the backend. Output: the status
/// line, the extra header lines and the body.
fn route(
    request: &Request,
    served: &Path,
    token: &str,
    backend: &Backend,
) -> (String, String, Vec<u8>) {
    let json = "Content-Type: application/json\r\n".to_owned();
    if request.path == "/user" {
        return if request.authorization.as_deref() == Some(format!("Bearer {token}").as_str()) {
            ("200 OK".into(), json, br#"{"login":"octo-cat"}"#.to_vec())
        } else {
            (
                "401 Unauthorized".into(),
                json,
                br#"{"message":"Bad credentials"}"#.to_vec(),
            )
        };
    }
    let basic = format!(
        "Basic {}",
        base64(format!("x-access-token:{token}").as_bytes())
    );
    if request.authorization.as_deref() != Some(basic.as_str()) {
        return (
            "401 Unauthorized".into(),
            "WWW-Authenticate: Basic realm=\"git\"\r\n".into(),
            Vec::new(),
        );
    }
    if request
        .path
        .trim_start_matches('/')
        .split('/')
        .any(|part| part == ".." || part.is_empty())
    {
        return ("404 Not Found".into(), String::new(), Vec::new());
    }
    backend.answer(request, served)
}

/// The most bytes one `git http-backend` reply may carry.
const BACKEND_BYTES_MAX: u64 = 16 * 1024 * 1024;

/// The real `git http-backend` beside the `git` the rows run, found through
/// `git --exec-path`, and that folder, which its own `git upload-pack` needs on PATH.
struct Backend {
    program: PathBuf,
    exec_path: PathBuf,
}

impl Backend {
    /// Finds the backend. Output: it, or a panic naming what `git --exec-path` printed.
    fn find() -> Self {
        let output = kernel_processes::spawn_gate::collected(
            std::process::Command::new("git").arg("--exec-path"),
        )
        .expect("git --exec-path starts");
        assert!(output.status.success(), "git --exec-path: {output:?}");
        let exec_path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        let program = exec_path.join("git-http-backend");
        assert!(
            exec_path.is_absolute() && program.is_file(),
            "no git-http-backend in {}",
            exec_path.display()
        );
        Self { program, exec_path }
    }

    /// Runs one request through the backend as CGI on a cleared environment holding only
    /// what it reads, the body on its standard input. Inputs: the request and the served
    /// folder. Output: the status line, the header lines it set and the body.
    fn answer(&self, request: &Request, served: &Path) -> (String, String, Vec<u8>) {
        let mut command = std::process::Command::new(&self.program);
        command
            .env_clear()
            .env("GIT_PROJECT_ROOT", served)
            .env("GIT_HTTP_EXPORT_ALL", "1")
            .env("GIT_EXEC_PATH", &self.exec_path)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.exec_path.display()),
            )
            .env("REQUEST_METHOD", &request.method)
            .env("PATH_INFO", &request.path)
            .env("QUERY_STRING", &request.query)
            .env("CONTENT_LENGTH", request.body.len().to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        for (name, value) in [
            ("CONTENT_TYPE", &request.content_type),
            ("HTTP_CONTENT_ENCODING", &request.content_encoding),
            ("HTTP_GIT_PROTOCOL", &request.git_protocol),
        ] {
            if let Some(value) = value {
                command.env(name, value);
            }
        }
        let mut child = kernel_processes::spawn_gate::spawn(&mut command)
            .unwrap_or_else(|error| panic!("{}: {error}", self.program.display()));
        let mut input = child.stdin.take().expect("the backend's stdin");
        let _ = input.write_all(&request.body);
        drop(input);
        let mut reply = Vec::new();
        let read = child
            .stdout
            .take()
            .expect("the backend's stdout")
            .take(BACKEND_BYTES_MAX)
            .read_to_end(&mut reply);
        let ended = child.wait();
        assert!(read.is_ok() && ended.is_ok(), "{read:?} {ended:?}");
        cgi_reply(&reply)
    }
}

/// Splits a CGI reply into the HTTP status line, the header lines to pass on and the body.
/// Input: the reply. Output: its parts, `200 OK` when it names no `Status`, or a 502 when it
/// holds no end of headers.
fn cgi_reply(reply: &[u8]) -> (String, String, Vec<u8>) {
    let Some(end) = reply.windows(4).position(|window| window == b"\r\n\r\n") else {
        return ("502 Bad Gateway".into(), String::new(), Vec::new());
    };
    let head = String::from_utf8_lossy(&reply[..end]);
    let mut status = "200 OK".to_owned();
    let mut extra = String::new();
    for line in head.split("\r\n") {
        match line.split_once(':') {
            Some((name, value)) if name.eq_ignore_ascii_case("status") => {
                status = value.trim().to_owned();
            }
            Some(_) => {
                extra.push_str(line);
                extra.push_str("\r\n");
            }
            None => {}
        }
    }
    (status, extra, reply[end + 4..].to_vec())
}

/// Reads one request, at most [`REQUEST_BYTES_MAX`] bytes with its body, writes the route's
/// answer and counts each smart-HTTP fetch answered `200`. Inputs: the connection, the
/// served folder, the token, the backend and the fetch count.
fn answer(
    mut stream: TcpStream,
    served: &Path,
    token: &str,
    backend: &Backend,
    fetches: &AtomicUsize,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let Ok(copy) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(copy).take(REQUEST_BYTES_MAX);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut words = line.split(' ');
    let method = words.next().unwrap_or("GET").to_owned();
    let target = words.next().unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut request = Request {
        method,
        path: path.to_owned(),
        query: query.to_owned(),
        authorization: None,
        content_type: None,
        content_encoding: None,
        git_protocol: None,
        body: Vec::new(),
    };
    let mut length = 0_usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let value = Some(value.trim().to_owned());
        match name.to_ascii_lowercase().as_str() {
            "authorization" => request.authorization = value,
            "content-type" => request.content_type = value,
            "content-encoding" => request.content_encoding = value,
            "git-protocol" => request.git_protocol = value,
            "content-length" => length = value.and_then(|v| v.parse().ok()).unwrap_or(0),
            _ => {}
        }
    }
    request
        .body
        .resize(length.min(REQUEST_BYTES_MAX as usize), 0);
    if reader.read_exact(&mut request.body).is_err() {
        return;
    }
    let (status, extra, body) = route(&request, served, token, backend);
    if request.method == "POST"
        && request.path.ends_with("/git-upload-pack")
        && status.starts_with("200")
    {
        fetches.fetch_add(1, Ordering::SeqCst);
    }
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
}

/// Starts the loopback server on a free port: the account stand-in and, behind the token,
/// the real `git http-backend` serving the bare repositories under `served`. Inputs: the
/// served folder and the token. Output: the port and the count of smart-HTTP fetches.
fn stand_in(served: PathBuf, token: String) -> (u16, Arc<AtomicUsize>) {
    let backend = Backend::find();
    let fetches = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&fetches);
    let listener = kernel_processes::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in kernel_processes::spawn_gate::incoming_tcp_std(&listener).flatten() {
            answer(stream, &served, &token, &backend, &counted);
        }
    });
    (port, fetches)
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
async fn a_sign_in_kept_for_the_engine_clones_through_the_credential_helper_and_the_engine_signs_it_out()
 {
    #[cfg(target_os = "macos")]
    let keychain = keychain_support::TemporaryKeychain::create();
    let mut engine = Engine::new("ca-e2e-clone-");
    #[cfg(target_os = "macos")]
    {
        engine.home = Some(keychain.home().to_path_buf());
    }
    engine.launch();
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
    let token = format!("gho_e2e{}", hex(18));
    let (port, fetches) = stand_in(served, token.clone());

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
    #[cfg(target_os = "macos")]
    let store: Arc<dyn Store> = Arc::new(Keychain::at(&space, keychain.path()).unwrap());
    #[cfg(target_os = "linux")]
    let store: Arc<dyn Store> = Arc::new(
        OsKeyring::open(&space, Path::new(env!("CARGO_BIN_EXE_crazyagent")))
            .expect(STORE_PREREQUISITE),
    );
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
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
    assert!(
        store.get(&kept.item).is_ok(),
        "the sign-in is kept under the engine's credential reference"
    );
    let status = client.github_status().await.unwrap();
    assert!(
        status.signed_in,
        "the engine reads the sign-in kept under its own namespace: {status:?}"
    );
    assert_eq!(status.login.as_deref(), Some("octo-cat"));
    assert_eq!(status.method, GitHubMethod::Token);
    drop(client);

    let receipt = account.clone_repo(&home, "octo-cat/tool").unwrap();
    assert!(
        fetches.load(Ordering::SeqCst) >= 1,
        "the clone fetched over smart HTTP through git http-backend"
    );
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
