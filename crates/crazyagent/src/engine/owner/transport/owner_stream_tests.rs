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

/// The calls that open an owner stream, in whitespace-free form: the shared connect, the Unix
/// socket connects and the Windows pipe client opens.
const CONNECTS: [&str; 6] = [
    "owner_connect(",
    "connect_unix(",
    "ClientOptions::open",
    "ClientOptions::new(",
    "UnixStream::connect(",
    "NamedPipeClient",
];

/// How many owner connects a text holds in any spelling: the text is read with every space and
/// line break removed. Input: the text. Output: the count.
fn connects_in(text: &str) -> usize {
    let flat: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    CONNECTS
        .iter()
        .map(|connect| flat.matches(connect).count())
        .sum()
}

/// Whether a file's text opens an owner connection on its own. Input: the text. Output: true
/// when the text holds any owner connect.
fn connects_on_its_own(text: &str) -> bool {
    connects_in(text) > 0
}

/// Whether a line opens an inline module, `mod <name> {` with an optional visibility.
/// Input: the trimmed line. Output: the answer.
fn opens_inline_module(line: &str) -> bool {
    let rest = ["pub(crate) ", "pub(super) ", "pub "]
        .iter()
        .find_map(|visibility| line.strip_prefix(visibility))
        .unwrap_or(line);
    rest.starts_with("mod ") && rest.ends_with('{')
}

/// Whether a line is a `#[cfg(...)]` attribute naming `test`. Input: the trimmed line.
/// Output: the answer.
fn names_test_cfg(line: &str) -> bool {
    line.starts_with("#[cfg(")
        && line
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .any(|word| word == "test")
}

/// The production part of a file: the text above its first inline test module, a test cfg
/// line whose next non-blank line opens `mod <name> {`. A test cfg on a helper or on a module
/// kept in its own file does not cut. Input: the text. Output: the part and whether it was cut.
fn production(text: &str) -> (&str, bool) {
    let mut lines = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        lines.push((start, line.trim()));
        start += line.len();
    }
    for (index, (start, line)) in lines.iter().enumerate() {
        if !names_test_cfg(line) {
            continue;
        }
        let next = lines[index + 1..]
            .iter()
            .map(|(_, line)| *line)
            .find(|line| !line.is_empty());
        if next.is_some_and(opens_inline_module) {
            return (&text[..*start], true);
        }
    }
    (text, false)
}

/// Whether a path under the owner folder holds test code only: a `tests` or `*_tests` file or
/// folder. Input: the relative path. Output: the answer.
fn test_only(path: &Path) -> bool {
    path.with_extension("").components().any(|part| {
        let part = part.as_os_str().to_string_lossy();
        part == "tests" || part.ends_with("_tests")
    })
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

/// A test cfg on a helper keeps the production text after it, and an inline test module
/// cuts the text where its cfg line starts.
#[test]
fn the_production_cut_starts_at_the_first_inline_test_module() {
    let text = "#[cfg(test)]\nfn helper() {}\n\nfn real() { owner_connect(a) }\n#[cfg(test)]\nmod stream_tests;\n#[cfg(all(test, unix))]\n\nmod tests {\n    owner_connect(b)\n}\n";
    let (kept, cut) = production(text);
    assert!(cut, "{text}");
    assert!(kept.ends_with("mod stream_tests;\n"), "{kept}");
    assert_eq!(connects_in(kept), 1, "{kept}");
    assert_eq!(production("fn real() {}\n"), ("fn real() {}\n", false));
}

/// A production function placed after an inline test module stays in the production text,
/// so its owner connect is counted.
#[test]
fn production_code_after_an_inline_test_module_is_counted() {
    let text = "fn real() {}\n#[cfg(test)]\nmod tests {\n    owner_connect(b)\n}\n\nfn late() { owner_connect(c) }\n";
    assert_eq!(connects_in(&production(text).0), 1, "{text}");
}

/// A module that builds only outside tests is production code, so its owner connect is
/// counted; only cfg(test) and cfg(all(test, ..)) mark test code.
#[test]
fn a_module_built_outside_tests_is_counted() {
    let text = "#[cfg(not(test))]\nmod real {\n    owner_connect(a)\n}\n";
    assert_eq!(connects_in(&production(text).0), 1, "{text}");
}

/// The two guarded files that carry an inline test module are cut on a test cfg line followed
/// by that module, so a moved module shows here instead of leaving its connects in the count.
#[test]
fn the_guarded_files_are_cut_on_their_inline_test_module() {
    let owner = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/owner"));
    for name in ["transport.rs", "transport/native_auth_channel.rs"] {
        let text = std::fs::read_to_string(owner.join(name)).unwrap();
        let (kept, cut) = production(&text);
        assert!(cut, "{name} has no inline test module");
        let mut rest = text[kept.len()..]
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty());
        assert!(rest.next().is_some_and(names_test_cfg), "{name}");
        assert!(rest.next().is_some_and(opens_inline_module), "{name}");
    }
}

/// Owner clients open their connection through the one checked connect, so none writes its
/// capability to an endpoint it did not check again after the connect. The other connects in
/// production text are the runner control and runner request exchanges in transport.rs and the
/// native sign-in custody channel: none is an owner client connect, and each checks the
/// endpoint identity before the connect and the server after it. A new connect in any spelling,
/// in any guarded file, changes this map.
#[test]
fn every_owner_client_connects_through_the_checked_connect() {
    let owner = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/owner"));
    let mut files = rust_files(&owner.join("transport"));
    files.push(owner.join("transport.rs"));
    let guarded: Vec<&Path> = files
        .iter()
        .map(|path| path.strip_prefix(owner).unwrap())
        .filter(|path| !test_only(path))
        .collect();
    assert!(guarded.len() >= 20, "guarded files {}", guarded.len());
    let mut found: Vec<(String, usize)> = guarded
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(owner.join(path)).unwrap();
            let name: Vec<String> = path
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect();
            (name.join("/"), connects_in(production(&text).0))
        })
        .filter(|(_, count)| *count > 0)
        .collect();
    found.sort();
    let expected = [
        ("transport.rs", 2),
        ("transport/native_auth_channel.rs", 1),
        ("transport/owner_stream.rs", 1),
    ]
    .map(|(name, count)| (name.to_string(), count));
    assert_eq!(found, expected, "owner connects per guarded file");
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
