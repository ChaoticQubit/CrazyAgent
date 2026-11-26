use super::*;

/// The Rust files under one folder and its subfolders. Input: the folder. Output: the files.
fn rust_files(folder: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(rust_files(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    files
}

/// Whether a file's text opens an owner connection on its own. Input: the text. Output: true
/// when the text holds the checked connect's own call.
fn connects_on_its_own(text: &str) -> bool {
    text.contains(concat!("owner_connect(", "self.root.path.join(SOCKET))"))
}

/// Every spelling of an owner connect is flagged: the call split over lines, a borrowed path,
/// a path bound first, the endpoint named through its module, and a direct Unix socket or pipe
/// client open.
#[test]
fn every_spelling_of_an_owner_connect_is_flagged() {
    let spellings = [
        "let stream = crate::ipc::owner_connect(\n    self.root.path.join(SOCKET),\n)\n.await",
        "crate::ipc::owner_connect(&self.root.path.join(SOCKET)).await",
        "let path = self.root.path.join(SOCKET);\nlet stream = crate::ipc::owner_connect(path).await;",
        "crate::ipc::owner_connect(self.root.path.join(super::SOCKET)).await",
        "kernel_processes::spawn_gate::connect_unix(self.root.path.join(SOCKET)).await",
        "let pipe = named_pipe::ClientOptions::open(&name)?;",
        "let pipe = ClientOptions::new()\n    .open(&name)?;",
        "let socket = std::os::unix::net::UnixStream::connect(&path)?;",
        "let pipe: NamedPipeClient = open(&name)?;",
    ];
    let missed: Vec<&str> = spellings
        .into_iter()
        .filter(|text| !connects_on_its_own(text))
        .collect();
    assert_eq!(
        missed,
        Vec::<&str>::new(),
        "connect spellings the guard misses"
    );
}

/// A file that names the endpoint but opens no connection of its own is not flagged.
#[test]
fn a_file_without_a_connect_is_not_flagged() {
    let text = "let socket = self.root.path.join(SOCKET);\nlet reply = self.exchange(request).await?;\nconnected(socket)";
    assert!(!connects_on_its_own(text), "{text}");
}

/// Every owner client opens its connection through the one checked connect, so none writes
/// its capability to an endpoint it did not check again after the connect.
#[test]
fn every_owner_client_connects_through_the_checked_connect() {
    let owner = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/owner"));
    let shared = owner.join("transport/owner_stream.rs");
    let own_connect = concat!("owner_connect(", "self.root.path.join(SOCKET))");
    let mut files = rust_files(&owner.join("transport"));
    files.push(owner.join("transport.rs"));
    let mut alone: Vec<String> = files
        .iter()
        .filter(|path| **path != shared)
        .filter(|path| std::fs::read_to_string(path).unwrap().contains(own_connect))
        .map(|path| path.strip_prefix(owner).unwrap().display().to_string())
        .collect();
    alone.sort();
    assert_eq!(
        alone,
        Vec::<String>::new(),
        "owner clients that connect on their own"
    );
    let checked = std::fs::read_to_string(&shared).unwrap_or_default();
    assert_eq!(
        checked.matches(own_connect).count(),
        1,
        "the one checked connect"
    );
}

/// An owner endpoint replaced after a client connected fails the check after the connect, so
/// the client writes nothing to that stream; the same check passes before the swap.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_check_after_the_connect_refuses_a_replaced_endpoint() {
    let fixture = super::owner_server_tests::Fixture::start().await;
    let client = fixture.client().await;
    let socket = client.root.path.join(SOCKET);
    let first = crate::ipc::owner_connect(&socket).await.unwrap();
    assert!(
        client.checked(first).is_ok(),
        "the admitted endpoint passes"
    );
    let stream = crate::ipc::owner_connect(&socket).await.unwrap();
    let kept = client.root.path.join("kept.sock");
    std::fs::rename(&socket, &kept).unwrap();
    let swap = kernel_processes::spawn_gate::bind_unix_std(&socket).unwrap();
    std::fs::set_permissions(&socket, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let refused = client.checked(stream).err().map(|error| error.0);
    drop(swap);
    std::fs::rename(&kept, &socket).unwrap();
    assert_eq!(refused.as_deref(), Some("owner_socket_replaced"));
    fixture.stop().await;
}

/// An owner endpoint whose pipes file names another pipe after a client connected fails the
/// check after the connect, so the client writes nothing to that stream; the same check
/// passes before the rewrite.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread")]
async fn the_check_after_the_connect_refuses_a_replaced_endpoint() {
    use super::owner_pipe_windows_tests::engine_and_owner_root;
    let (_temp, engine, config, owners) = engine_and_owner_root().await;
    let (server, _host) = OwnerServer::bind(&owners, engine.clone(), &config)
        .await
        .unwrap();
    let (stop, stopped) = oneshot::channel();
    let running = tokio::spawn(server.run(stopped));
    let client =
        OwnerClient::attach(&owners, OwnerClientKind::Desktop, Duration::from_secs(10)).unwrap();
    let first = crate::ipc::owner_connect(owners.join(SOCKET))
        .await
        .unwrap();
    assert!(
        client.checked(first).is_ok(),
        "the admitted endpoint passes"
    );
    let stream = crate::ipc::owner_connect(owners.join(SOCKET))
        .await
        .unwrap();
    let pipes = owners.join("pipes");
    let text = std::fs::read_to_string(&pipes).unwrap();
    let (line, pin) = text.trim_end().rsplit_once(' ').unwrap();
    let moved = pin.parse::<u64>().unwrap() + 1;
    std::fs::write(&pipes, format!("{line} {moved}\n")).unwrap();
    let refused = client.checked(stream).err().map(|error| error.0);
    std::fs::write(&pipes, &text).unwrap();
    assert_eq!(refused.as_deref(), Some("owner_socket_replaced"));
    stop.send(()).unwrap();
    let _ = running.await.unwrap();
    engine.close().await.unwrap();
}
