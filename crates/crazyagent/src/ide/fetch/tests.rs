use super::*;
use crate::ide::manifest;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar};
use std::time::Instant;

const VERSION: &str = "1.2.3";
const OFFICIAL_VERSION: &str = "1.143.17183";
const OFFICIAL_DARWIN_ARM64: &str = "https://github.com/VSCodium/vscodium/releases/download/1.143.17183/vscodium-reh-web-darwin-arm64-1.143.17183.tar.gz";
const DARWIN_ARM64_SHA256: &str =
    "bf9f1b87841ee4d7b15754698206eb360436bbdc2771553674e517e111b47784";
const TARGETS: [&str; 5] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
];

/// A real loopback HTTP server answering every request with one body. It counts the
/// connections it accepted and, while held, answers none until released.
struct Server {
    url: String,
    hits: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    gate: Arc<(Mutex<bool>, Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn start(body: Vec<u8>, held: bool) -> Self {
        let listener = crate::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!(
            "http://{}/vscodium-reh-web.tar.gz",
            listener.local_addr().unwrap()
        );
        let hits = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let gate = Arc::new((Mutex::new(held), Condvar::new()));
        let body = Arc::new(body);
        let thread = {
            let (hits, stop, gate) = (Arc::clone(&hits), Arc::clone(&stop), Arc::clone(&gate));
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let stream = match crate::spawn_gate::accept_tcp_std(&listener) {
                        Ok((stream, _)) => stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(error) => panic!("accept failed: {error}"),
                    };
                    hits.fetch_add(1, Ordering::SeqCst);
                    let (body, gate) = (Arc::clone(&body), Arc::clone(&gate));
                    std::thread::spawn(move || answer(stream, &body, &gate));
                }
            })
        };
        Self {
            url,
            hits,
            stop,
            gate,
            thread: Some(thread),
        }
    }

    fn release(&self) {
        let (held, changed) = &*self.gate;
        *held.lock().unwrap() = false;
        changed.notify_all();
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.release();
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

fn answer(mut stream: std::net::TcpStream, body: &[u8], gate: &(Mutex<bool>, Condvar)) {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut request = Vec::new();
    let mut byte = [0_u8; 1];
    while !request.ends_with(b"\r\n\r\n") && request.len() < 64 * 1024 {
        match stream.read(&mut byte) {
            Ok(1) => request.push(byte[0]),
            _ => return,
        }
    }
    let (held, changed) = gate;
    let mut waiting = held.lock().unwrap();
    while *waiting {
        waiting = changed.wait(waiting).unwrap();
    }
    drop(waiting);
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(body));
}

fn run(command: &mut Command) {
    let output = crate::spawn_gate::output(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{command:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// The folder a build of [`VERSION`] installs under.
fn installed_folder() -> String {
    format!("{VERSION}-{}", crate::ide::hide::REVISION)
}

/// A small real server tree: a start script, a product file and a workbench bundle holding
/// the pinned build's removed parts.
fn server_tree(root: &Path) -> PathBuf {
    let source = root.join("source");
    write(&source.join("bin/codium-server"), "#!/bin/sh\necho ok\n");
    write(
        &source.join(crate::ide::hide::BUNDLE),
        crate::ide::hide::tests::SHIPPED,
    );
    write(
        &source.join("product.json"),
        "{\"nameShort\":\"VSCodium\"}\n",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            source.join("bin/codium-server"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    source
}

/// Makes a symbolic link at `at` that reads `target`, a folder link when `folder`.
fn link(target: &str, at: &Path, folder: bool) {
    #[cfg(unix)]
    {
        let _ = folder;
        std::os::unix::fs::symlink(target, at).unwrap();
    }
    #[cfg(windows)]
    {
        let target = target.replace('/', "\\");
        if folder {
            std::os::windows::fs::symlink_dir(target, at).unwrap();
        } else {
            std::os::windows::fs::symlink_file(target, at).unwrap();
        }
    }
}

/// Packs a folder's contents into a gzip tar with the system tar.
fn pack(source: &Path, archive: &Path) -> Vec<u8> {
    run(Command::new("tar")
        .arg("-czf")
        .arg(archive)
        .arg("-C")
        .arg(source)
        .arg("."));
    std::fs::read(archive).unwrap()
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn build(url: &str, bytes: &[u8]) -> Build {
    Build {
        version: VERSION.into(),
        url: url.into(),
        sha256: digest(bytes),
        size_max: bytes.len() as u64,
    }
}

fn runtime(root: &Path) -> PathBuf {
    let runtime = root.join("engine");
    std::fs::create_dir(&runtime).unwrap();
    runtime
}

fn files_in(folder: &Path) -> Vec<String> {
    match std::fs::read_dir(folder) {
        Ok(entries) => entries
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn wait_for(runtime: &Path, done: impl Fn(&BuildState) -> bool) -> BuildState {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let now = state(runtime);
        if done(&now) {
            return now;
        }
        assert!(Instant::now() < deadline, "still {now:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn refused(result: Result<Build, EngineError>) -> String {
    result.unwrap_err().0
}

fn manifest_text(version: &str, url: &str, sha256: &str, bytes: u64) -> String {
    format!(
        r#"{{"version":"{version}","builds":{{"aarch64-apple-darwin":{{"url":"{url}","sha256":"{sha256}","bytes":{bytes}}}}}}}"#
    )
}

#[test]
fn manifest_accepts_only_the_official_https_release_url() {
    let official = manifest_text(
        OFFICIAL_VERSION,
        OFFICIAL_DARWIN_ARM64,
        DARWIN_ARM64_SHA256,
        96_507_861,
    );
    assert_eq!(
        manifest::parse(&official, "aarch64-apple-darwin").unwrap(),
        Build {
            version: OFFICIAL_VERSION.into(),
            url: OFFICIAL_DARWIN_ARM64.into(),
            sha256: DARWIN_ARM64_SHA256.into(),
            size_max: 96_507_861,
        }
    );
    for url in [
        "http://github.com/VSCodium/vscodium/releases/download/1.143.17183/vscodium-reh-web-darwin-arm64-1.143.17183.tar.gz",
        "https://example.com/VSCodium/vscodium/releases/download/1.143.17183/vscodium-reh-web-darwin-arm64-1.143.17183.tar.gz",
        "https://github.com.example.com/VSCodium/vscodium/releases/download/1.143.17183/vscodium-reh-web-darwin-arm64-1.143.17183.tar.gz",
        "https://github.com:8443/VSCodium/vscodium/releases/download/1.143.17183/vscodium-reh-web-darwin-arm64-1.143.17183.tar.gz",
        "https://owner@github.com/VSCodium/vscodium/releases/download/1.143.17183/vscodium-reh-web-darwin-arm64-1.143.17183.tar.gz",
        "https://github.com/someone/vscodium/releases/download/1.143.17183/vscodium-reh-web-darwin-arm64-1.143.17183.tar.gz",
        "https://github.com/VSCodium/vscodium/releases/download/1.140.06880/vscodium-reh-web-darwin-arm64-1.143.17183.tar.gz",
        "https://github.com/VSCodium/vscodium/releases/download/1.143.17183/vscodium-reh-web-linux-x64-1.143.17183.tar.gz",
        "https://github.com/VSCodium/vscodium/releases/download/1.143.17183/../../../evil/vscodium-reh-web-darwin-arm64-1.143.17183.tar.gz",
        "https://github.com/VSCodium/vscodium/releases/download/1.143.17183/vscodium-reh-web-darwin-arm64-1.143.17183.tar.gz?x=1",
        "https://github.com/VSCodium/vscodium/releases/download/1.143.17183/vscodium-reh-web-darwin-arm64-1.143.17183.zip",
    ] {
        let text = manifest_text(OFFICIAL_VERSION, url, DARWIN_ARM64_SHA256, 96_507_861);
        assert_eq!(
            refused(manifest::parse(&text, "aarch64-apple-darwin")),
            "ide_build_url_refused",
            "{url}"
        );
    }
    for (version, sha256, bytes) in [
        (OFFICIAL_VERSION, &DARWIN_ARM64_SHA256[1..], 96_507_861),
        (
            OFFICIAL_VERSION,
            &DARWIN_ARM64_SHA256.to_uppercase()[..],
            96_507_861,
        ),
        (OFFICIAL_VERSION, DARWIN_ARM64_SHA256, 0),
        (OFFICIAL_VERSION, DARWIN_ARM64_SHA256, 151 * 1024 * 1024),
        ("1.143.17183/..", DARWIN_ARM64_SHA256, 96_507_861),
        ("", DARWIN_ARM64_SHA256, 96_507_861),
    ] {
        let text = manifest_text(version, OFFICIAL_DARWIN_ARM64, sha256, bytes);
        assert_eq!(
            refused(manifest::parse(&text, "aarch64-apple-darwin")),
            "ide_build_manifest_invalid",
            "{version} {sha256} {bytes}"
        );
    }
    assert_eq!(
        refused(manifest::parse(&official, "x86_64-apple-darwin")),
        "ide_build_unsupported_platform"
    );
    assert_eq!(
        refused(manifest::current("riscv64gc-unknown-linux-gnu")),
        "ide_build_unsupported_platform"
    );
    for target in TARGETS {
        let pinned = manifest::current(target).unwrap();
        assert_eq!(pinned.version, OFFICIAL_VERSION, "{target}");
        assert!(
            pinned.url.starts_with(
                "https://github.com/VSCodium/vscodium/releases/download/1.143.17183/vscodium-reh-web-"
            ),
            "{target}"
        );
        assert_eq!(pinned.sha256.len(), 64, "{target}");
        assert!(pinned.size_max <= manifest::BYTES_MAX, "{target}");
    }
    assert_eq!(
        manifest::current("aarch64-apple-darwin").unwrap(),
        Build {
            version: OFFICIAL_VERSION.into(),
            url: OFFICIAL_DARWIN_ARM64.into(),
            sha256: DARWIN_ARM64_SHA256.into(),
            size_max: 96_507_861,
        }
    );
}

#[test]
fn archive_with_a_different_sha256_is_never_extracted() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = runtime(tmp.path());
    let bytes = pack(&server_tree(tmp.path()), &tmp.path().join("build.tar.gz"));
    let mut changed = bytes.clone();
    let last = changed.len() - 1;
    changed[last] ^= 0xff;
    let server = Server::start(changed, false);
    let finished = fetch(&runtime, &build(&server.url, &bytes));
    assert_eq!(
        finished,
        BuildState::Failed {
            code: "ide_build_hash_mismatch".into()
        }
    );
    assert_eq!(state(&runtime), finished);
    assert_eq!(server.hits(), 1);
    assert!(files_in(&runtime.join("ide/downloads")).is_empty());
    assert!(files_in(&runtime.join("ide/builds")).is_empty());
    assert!(!runtime.join("ide/builds").join(VERSION).exists());
}

#[test]
fn extract_refuses_entries_outside_the_target() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let absolute = root.join("abs/evil.txt");
    write(&absolute, "absolute\n");
    let absolute_archive = root.join("absolute.tar.gz");
    run(Command::new("tar")
        .arg("-czPf")
        .arg(&absolute_archive)
        .arg(&absolute));
    std::fs::remove_file(&absolute).unwrap();
    let inner = root.join("inner");
    std::fs::create_dir(&inner).unwrap();
    write(&root.join("evil.txt"), "dots\n");
    let dots_archive = root.join("dots.tar.gz");
    run(Command::new("tar")
        .arg("-czPf")
        .arg(&dots_archive)
        .arg("-C")
        .arg(&inner)
        .arg("../evil.txt"));
    std::fs::remove_file(root.join("evil.txt")).unwrap();
    let linked = root.join("linked");
    write(&linked.join("product.json"), "{}\n");
    std::fs::create_dir(linked.join("bin")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("../../../outside", linked.join("bin/escape")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file("..\\..\\..\\outside", linked.join("bin/escape")).unwrap();
    let link_archive = root.join("link.tar.gz");
    pack(&linked, &link_archive);
    let rooted = root.join("rooted");
    write(&rooted.join("product.json"), "{}\n");
    #[cfg(unix)]
    std::os::unix::fs::symlink("/etc", rooted.join("system")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir("C:\\Windows", rooted.join("system")).unwrap();
    let rooted_archive = root.join("rooted.tar.gz");
    pack(&rooted, &rooted_archive);
    let through = root.join("through");
    write(&through.join("product.json"), "{}\n");
    link(".", &through.join("L"), true);
    link("L/..", &through.join("M"), true);
    let through_archive = root.join("through.tar.gz");
    pack(&through, &through_archive);
    let dotted = root.join("dotted");
    write(&dotted.join("product.json"), "{}\n");
    link(".", &dotted.join("a"), true);
    link("a/../product.json", &dotted.join("b"), false);
    let dotted_archive = root.join("dotted.tar.gz");
    pack(&dotted, &dotted_archive);
    let dangling = root.join("dangling");
    write(&dangling.join("product.json"), "{}\n");
    link("missing.json", &dangling.join("gone.json"), false);
    let dangling_archive = root.join("dangling.tar.gz");
    pack(&dangling, &dangling_archive);
    let looping = root.join("looping");
    write(&looping.join("product.json"), "{}\n");
    link("second", &looping.join("first"), false);
    link("first", &looping.join("second"), false);
    let looping_archive = root.join("looping.tar.gz");
    pack(&looping, &looping_archive);
    let inside = root.join("inside");
    write(&inside.join("product.json"), "{}\n");
    std::fs::create_dir(inside.join("bin")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("../product.json", inside.join("bin/product")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file("..\\product.json", inside.join("bin/product")).unwrap();
    let inside_archive = root.join("inside.tar.gz");
    pack(&inside, &inside_archive);
    let chained = root.join("chained");
    write(&chained.join("product.json"), "{}\n");
    std::fs::create_dir(chained.join("d")).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("..", chained.join("d/a")).unwrap();
        std::os::unix::fs::symlink("d/a/../outside", chained.join("x")).unwrap();
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir("..", chained.join("d/a")).unwrap();
        std::os::windows::fs::symlink_file("d\\a\\..\\outside", chained.join("x")).unwrap();
    }
    let chained_archive = root.join("chained.tar.gz");
    pack(&chained, &chained_archive);
    for (case, archive) in [
        ("absolute", &absolute_archive),
        ("dots", &dots_archive),
        ("link", &link_archive),
        ("rooted", &rooted_archive),
        ("through", &through_archive),
        ("dotted", &dotted_archive),
        ("dangling", &dangling_archive),
        ("looping", &looping_archive),
        ("inside", &inside_archive),
        ("chained", &chained_archive),
    ] {
        let place = root.join(format!("case-{case}"));
        std::fs::create_dir(&place).unwrap();
        let runtime = runtime(&place);
        let bytes = std::fs::read(archive).unwrap();
        let server = Server::start(bytes.clone(), false);
        assert_eq!(
            fetch(&runtime, &build(&server.url, &bytes)),
            BuildState::Failed {
                code: "ide_build_archive_invalid".into()
            },
            "{case}"
        );
        assert!(!runtime.join("ide/builds").join(VERSION).exists(), "{case}");
        assert!(files_in(&runtime.join("ide/builds")).is_empty(), "{case}");
        assert!(
            files_in(&runtime.join("ide/downloads")).is_empty(),
            "{case}"
        );
        assert!(!runtime.join("ide/evil.txt").exists(), "{case}");
        assert!(!runtime.join("ide/builds/evil.txt").exists(), "{case}");
        assert!(!place.join("outside").exists(), "{case}");
    }
    assert!(!absolute.exists());
    assert!(!root.join("evil.txt").exists());
}

#[test]
fn verified_archive_extracts_atomically_and_reports_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = runtime(tmp.path());
    let bytes = pack(&server_tree(tmp.path()), &tmp.path().join("build.tar.gz"));
    let server = Server::start(bytes.clone(), false);
    let finished = fetch(&runtime, &build(&server.url, &bytes));
    let root = runtime.join("ide/builds").join(installed_folder());
    assert_eq!(
        finished,
        BuildState::Ready {
            version: VERSION.into(),
            root: root.clone()
        }
    );
    assert_eq!(state(&runtime), finished);
    assert_eq!(
        std::fs::read_to_string(root.join("bin/codium-server")).unwrap(),
        "#!/bin/sh\necho ok\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("product.json")).unwrap(),
        "{\"nameShort\":\"VSCodium\"}\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join(crate::ide::hide::BUNDLE)).unwrap(),
        crate::ide::hide::tests::HIDDEN
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(root.join("bin/codium-server"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
    }
    assert_eq!(files_in(&runtime.join("ide/builds")), [installed_folder()]);
    assert!(files_in(&runtime.join("ide/downloads")).is_empty());
    std::fs::remove_dir_all(&root).unwrap();
    assert_eq!(state(&runtime), BuildState::Missing);
}

#[test]
fn ensure_twice_starts_one_download() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = runtime(tmp.path());
    let bytes = pack(&server_tree(tmp.path()), &tmp.path().join("build.tar.gz"));
    let server = Server::start(bytes.clone(), true);
    assert_eq!(state(&runtime), BuildState::Missing);
    assert!(ensure_with(&runtime, build(&server.url, &bytes)));
    assert!(!ensure_with(&runtime, build(&server.url, &bytes)));
    assert_eq!(
        state(&runtime),
        BuildState::Downloading {
            bytes: 0,
            total: bytes.len() as u64
        }
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while server.hits() == 0 {
        assert!(Instant::now() < deadline, "the download never connected");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!ensure_with(&runtime, build(&server.url, &bytes)));
    server.release();
    let ready = wait_for(&runtime, |now| matches!(now, BuildState::Ready { .. }));
    assert_eq!(
        ready,
        BuildState::Ready {
            version: VERSION.into(),
            root: runtime.join("ide/builds").join(installed_folder())
        }
    );
    assert!(!ensure_with(&runtime, build(&server.url, &bytes)));
    assert_eq!(server.hits(), 1);
}

#[test]
fn a_build_whose_removed_parts_cannot_be_hidden_never_installs() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = runtime(tmp.path());
    let source = server_tree(tmp.path());
    write(
        &source.join(crate::ide::hide::BUNDLE),
        &crate::ide::hide::tests::SHIPPED.replace("\"Manage Accounts\")", "\"Accounts\")"),
    );
    let bytes = pack(&source, &tmp.path().join("build.tar.gz"));
    let server = Server::start(bytes.clone(), false);
    let finished = fetch(&runtime, &build(&server.url, &bytes));
    assert_eq!(
        finished,
        BuildState::Failed {
            code: "ide_build_patch_failed".into()
        }
    );
    assert_eq!(state(&runtime), finished);
    assert!(files_in(&runtime.join("ide/builds")).is_empty());
    assert!(files_in(&runtime.join("ide/downloads")).is_empty());
}

#[test]
fn a_ready_build_installed_before_hiding_is_fetched_again_and_then_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = runtime(tmp.path());
    let old = runtime.join("ide/builds").join(VERSION);
    write(&old.join("product.json"), "{}\n");
    write_state(
        &runtime,
        &BuildState::Ready {
            version: VERSION.into(),
            root: old.clone(),
        },
    )
    .unwrap();
    assert!(matches!(state(&runtime), BuildState::Ready { .. }));
    let bytes = pack(&server_tree(tmp.path()), &tmp.path().join("build.tar.gz"));
    let server = Server::start(bytes.clone(), false);
    assert!(ensure_with(&runtime, build(&server.url, &bytes)));
    let root = runtime.join("ide/builds").join(installed_folder());
    let ready = wait_for(
        &runtime,
        |now| matches!(now, BuildState::Ready { root: at, .. } if *at == root),
    );
    assert_eq!(
        ready,
        BuildState::Ready {
            version: VERSION.into(),
            root: root.clone()
        }
    );
    assert_eq!(
        std::fs::read_to_string(root.join(crate::ide::hide::BUNDLE)).unwrap(),
        crate::ide::hide::tests::HIDDEN
    );
    assert!(!old.exists());
    assert_eq!(files_in(&runtime.join("ide/builds")), [installed_folder()]);

    let builds = runtime.join("ide/builds");
    write(&builds.join("0.9.0-h1/product.json"), "{}\n");
    write(
        &builds.join(format!(".staging-{VERSION}/product.json")),
        "{}\n",
    );
    let outside = tmp.path().join("outside");
    write(&outside.join("keep.txt"), "keep\n");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, builds.join("linked")).unwrap();
    assert!(!ensure_with(&runtime, build(&server.url, &bytes)));
    assert_eq!(files_in(&builds), [installed_folder()]);
    assert_eq!(
        std::fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "keep\n"
    );
    assert!(root.join(crate::ide::hide::BUNDLE).is_file());
    assert_eq!(server.hits(), 1);
}

#[test]
fn an_interrupted_download_reads_missing_and_the_engine_start_never_fetches_off_the_default_root() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = runtime(tmp.path());
    write(
        &runtime.join("ide/build-state.json"),
        "{\"state\":\"downloading\",\"bytes\":10,\"total\":20}",
    );
    assert_eq!(state(&runtime), BuildState::Missing);
    write(&runtime.join("ide/build-state.json"), "not json");
    assert_eq!(state(&runtime), BuildState::Missing);
    let other = tmp.path().join("other");
    std::fs::create_dir(&other).unwrap();
    assert!(!on_engine_start(&other));
    assert!(!other.join("ide").exists());
}

/// A failure recorded less than `FAILED_BACKOFF` ago keeps the engine start from fetching
/// again; an older failure, one dated in the future or any other state does not, and the
/// owner's Retry fetches at once either way.
#[test]
fn a_recent_failure_backs_off_the_engine_start_but_not_the_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = runtime(tmp.path());
    assert!(!backing_off(&runtime));
    let failed = BuildState::Failed {
        code: "ide_build_hash_mismatch".into(),
    };
    write_state(&runtime, &failed).unwrap();
    assert!(backing_off(&runtime));
    let dated = |at: std::time::SystemTime| {
        std::fs::File::options()
            .write(true)
            .open(runtime.join(IDE_FOLDER).join(STATE_FILE))
            .unwrap()
            .set_modified(at)
            .unwrap();
    };
    let now = std::time::SystemTime::now();
    dated(now - FAILED_BACKOFF + Duration::from_secs(60));
    assert!(backing_off(&runtime));
    dated(now - FAILED_BACKOFF - Duration::from_secs(60));
    assert!(!backing_off(&runtime));
    dated(now + Duration::from_secs(60 * 60));
    assert!(!backing_off(&runtime));
    write_state(&runtime, &BuildState::Missing).unwrap();
    assert!(!backing_off(&runtime));
    write_state(&runtime, &failed).unwrap();
    assert!(backing_off(&runtime));
    let bytes = pack(&server_tree(tmp.path()), &tmp.path().join("build.tar.gz"));
    let server = Server::start(bytes.clone(), false);
    assert!(ensure_with(&runtime, build(&server.url, &bytes)));
    let ready = wait_for(&runtime, |now| matches!(now, BuildState::Ready { .. }));
    assert_eq!(
        ready,
        BuildState::Ready {
            version: VERSION.into(),
            root: runtime.join("ide/builds").join(installed_folder())
        }
    );
    assert!(!backing_off(&runtime));
    assert_eq!(server.hits(), 1);
}

/// macOS with Seatbelt runs the IDE server, so the engine fetches its build there.
#[cfg(target_os = "macos")]
#[test]
fn macos_with_seatbelt_is_qualified_to_fetch_the_ide_build() {
    assert!(Path::new("/usr/bin/sandbox-exec").is_file());
    assert!(qualified());
}

/// Linux and Windows run no IDE server until IDE-12a and IDE-12b, so neither the engine
/// start nor the owner's Retry downloads, extracts or records a build there.
#[cfg(not(target_os = "macos"))]
#[test]
fn an_unqualified_platform_never_fetches_or_records_the_ide_build() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = runtime(tmp.path());
    assert!(!qualified());
    assert!(!ensure(&runtime));
    assert_eq!(state(&runtime), BuildState::Missing);
    assert!(!runtime.join(IDE_FOLDER).exists());
}

#[cfg(unix)]
#[test]
fn the_ide_folder_is_private_from_the_first_state_written() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let tmp = tempfile::tempdir().unwrap();
    let fresh = runtime(tmp.path());
    write_state(&fresh, &BuildState::Verifying).unwrap();
    assert_eq!(mode(&fresh.join("ide")), 0o700);
    assert!(
        crate::ide::bridge::private_folder(&fresh.join("ide")).is_ok(),
        "the IDE open accepts the folder the fetch made"
    );
    let older = tmp.path().join("older");
    std::fs::create_dir(&older).unwrap();
    std::fs::create_dir(older.join("ide")).unwrap();
    std::fs::set_permissions(older.join("ide"), std::fs::Permissions::from_mode(0o755)).unwrap();
    write_state(&older, &BuildState::Verifying).unwrap();
    assert_eq!(
        mode(&older.join("ide")),
        0o700,
        "a folder an earlier version left open is narrowed"
    );
}
