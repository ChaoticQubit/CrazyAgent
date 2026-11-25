use super::*;
use kernel_processes::update::test_support::{TestResult, code, joined, response, said, serve};
use std::path::PathBuf;
use std::time::Duration;

#[test]
fn the_channel_file_defaults_to_stable_round_trips_and_refuses_bad_values() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("engine");
    assert_eq!(read_channel(&root).unwrap(), Channel::Stable);
    save_channel(&root, Channel::Dev).unwrap();
    assert_eq!(read_channel(&root).unwrap(), Channel::Dev);
    let file = root.join("update.toml");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "channel = \"dev\"\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    save_channel(&root, Channel::Stable).unwrap();
    assert_eq!(read_channel(&root).unwrap(), Channel::Stable);
    std::fs::write(&file, "").unwrap();
    assert_eq!(read_channel(&root).unwrap(), Channel::Stable);
    std::fs::write(&file, "channel = \"nightly\"\n").unwrap();
    assert_eq!(code(read_channel(&root)), "update_channel_invalid");
    std::fs::write(&file, "channel = [").unwrap();
    assert_eq!(code(read_channel(&root)), "update_channel_invalid");
    std::fs::write(&file, format!("channel = \"dev\"\n#{}", "x".repeat(5000))).unwrap();
    assert_eq!(code(read_channel(&root)), "update_channel_invalid");
}

#[test]
fn saving_the_channel_creates_a_missing_data_folder_the_engine_accepts() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("CrazyAgent").join("engine");
    save_channel(&root, Channel::Dev).unwrap();
    assert_eq!(read_channel(&root).unwrap(), Channel::Dev);
    crate::paths::Layout::new(root.clone())
        .unwrap()
        .prepare()
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for folder in [root.parent().unwrap(), root.as_path()] {
            assert_eq!(
                std::fs::metadata(folder).unwrap().permissions().mode() & 0o7777,
                0o700
            );
        }
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(code(save_channel(&root, Channel::Stable)), "update_io");
        assert_eq!(read_channel(&root).unwrap(), Channel::Dev);
    }
}

#[test]
fn a_channel_with_no_release_says_so_in_one_plain_sentence() -> TestResult {
    let missing = || {
        response(
            "HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\n",
            b"Not Found",
        )
    };
    let (url, server) = serve(vec![missing(), missing()])?;
    assert_eq!(
        said(fetch_manifest(Channel::Stable, &url).unwrap_err()),
        (
            "update_stable_not_published",
            "No stable release is published yet.".to_owned()
        )
    );
    assert_eq!(
        said(fetch_manifest(Channel::Dev, &url).unwrap_err()),
        (
            "update_dev_not_published",
            "No dev release is published yet.".to_owned()
        )
    );
    joined(server)?;
    Ok(())
}

#[test]
fn download_and_manifest_failures_never_quote_curl_or_the_server() -> TestResult {
    let answered = |head: &str, body: &[u8]| {
        response(
            &format!(
                "HTTP/1.1 {head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            ),
            body,
        )
    };
    let unreadable = (
        "update_manifest_invalid",
        "The release information from the update server can't be read. Try again later.".to_owned(),
    );
    let (url, server) = serve(vec![
        answered("500 Internal Server Error", b"curl: (22) oops"),
        answered("200 OK", b"<html>no</html>"),
        answered("200 OK", b"{\"version\":\"\\u001b[31m9\",\"platforms\":{}}"),
    ])?;
    assert_eq!(
        said(fetch_manifest(Channel::Stable, &url).unwrap_err()),
        (
            "update_download_failed",
            "The update server answered with an error. Try again later.".to_owned()
        )
    );
    assert_eq!(
        said(fetch_manifest(Channel::Stable, &url).unwrap_err()),
        unreadable
    );
    assert_eq!(
        said(fetch_manifest(Channel::Dev, &url).unwrap_err()),
        unreadable
    );
    joined(server)?;

    let (_held, port) = kernel_processes::spawn_gate::refusing_tcp_port().unwrap();
    let address = format!("127.0.0.1:{port}");
    let mut received = Vec::new();
    assert_eq!(
        said(
            kernel_processes::update::download(
                &format!("http://{address}/release"),
                &mut received,
                4096,
                Duration::from_secs(10)
            )
            .unwrap_err()
        ),
        (
            "update_download_failed",
            "Couldn't reach the update server. Check your connection and try again.".to_owned()
        )
    );
    Ok(())
}

#[test]
fn update_arguments_take_check_and_one_channel() {
    let parse =
        |words: &[&str]| parse_arguments(&words.iter().map(OsString::from).collect::<Vec<_>>());
    let options = parse(&[]).unwrap();
    assert!(!options.check);
    assert_eq!(options.channel, None);
    let options = parse(&["--check", "--channel", "dev"]).unwrap();
    assert!(options.check);
    assert_eq!(options.channel, Some(Channel::Dev));
    assert_eq!(
        parse(&["--channel", "stable"]).unwrap().channel,
        Some(Channel::Stable)
    );
    for bad in [
        &["--channel"][..],
        &["--channel", "nightly"],
        &["--check", "--check"],
        &["--channel", "dev", "--channel", "stable"],
        &["--force"],
    ] {
        assert_eq!(code(parse(bad)), "update_usage", "{bad:?}");
    }
}

/// Every `.rs` file below a folder at its real path, skipping `target`, `node_modules` and
/// every link, so a linked folder never adds a file twice. Panics naming the path that
/// could not be read. Inputs: the folder and the list to fill.
fn rust_sources(folder: &Path, found: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(folder).unwrap_or_else(|error| panic!("read {folder:?}: {error}"));
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| panic!("entry in {folder:?}: {error}"));
        let path = entry.path();
        let kind = entry
            .file_type()
            .unwrap_or_else(|error| panic!("type of {path:?}: {error}"));
        let name = entry.file_name().to_string_lossy().into_owned();
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            if !["target", "node_modules"].contains(&name.as_str()) {
                rust_sources(&path, found);
            }
        } else if name.ends_with(".rs") {
            let real = path
                .canonicalize()
                .unwrap_or_else(|error| panic!("real path of {path:?}: {error}"));
            found.push(real);
        }
    }
}

fn function_holding<'a>(path: &Path, text: &'a str, at: usize) -> &'a str {
    let start = ["\nfn ", "\npub fn ", "\nasync fn ", "\npub async fn "]
        .iter()
        .filter_map(|head| text[..at].rfind(head))
        .max()
        .unwrap_or_else(|| panic!("{path:?} byte {at} sits inside no function"));
    &text[start..at]
}

fn test_source(path: &Path) -> bool {
    path.components().any(|part| part.as_os_str() == "tests")
        || path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with("tests.rs"))
}

/// A folder that holds a link to a sibling folder gives each `.rs` file once, at its real
/// path, so a link never counts a file twice.
#[cfg(unix)]
#[test]
fn rust_sources_skips_folder_links_and_finds_each_file_once() {
    let temporary = tempfile::tempdir().unwrap();
    let real = temporary.path().canonicalize().unwrap();
    std::fs::create_dir(real.join("held")).unwrap();
    std::fs::write(real.join("held").join("one.rs"), "fn one() {}\n").unwrap();
    std::os::unix::fs::symlink(real.join("held"), real.join("linked")).unwrap();
    let mut found = Vec::new();
    rust_sources(&real, &mut found);
    assert_eq!(found, vec![real.join("held").join("one.rs")]);
}

/// The desktop's one install step that writes a release over the running app is reached
/// only after its signature check: it installs only the bytes `update.download` verified.
/// The command line needs no such row, since `swap` takes only what `unpack` made from what
/// `verify_file` passed.
#[test]
fn no_install_path_skips_verification() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = crate_root
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|error| panic!("real path of {crate_root:?}/../..: {error}"));
    let mut sources = Vec::new();
    rust_sources(&root.join("crates"), &mut sources);
    rust_sources(&root.join("apps/desktop/src-tauri/src"), &mut sources);
    assert!(sources.len() > 100, "{}", sources.len());
    let installs = [
        ["update", ".install("].concat(),
        ["install", "(&bytes"].concat(),
    ];
    let mut desktop = Vec::new();
    for path in sources.iter().filter(|path| !test_source(path)) {
        let text =
            std::fs::read_to_string(path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
        let mut sites = std::collections::BTreeSet::new();
        for needle in &installs {
            let call = needle.find("install(").expect(needle) + "install(".len();
            for (at, _) in text.match_indices(needle.as_str()) {
                sites.insert(at + call);
            }
        }
        for site in sites {
            desktop.push((path.clone(), function_holding(path, &text, site).to_owned()));
        }
    }
    assert_eq!(desktop.len(), 1, "{desktop:?}");
    let (path, body) = &desktop[0];
    assert!(
        path.ends_with("apps/desktop/src-tauri/src/updates.rs"),
        "{path:?}"
    );
    let downloaded = body.find("update.download(").expect(body);
    assert!(Some(downloaded) < body.find(&installs[0]), "{body}");
}
