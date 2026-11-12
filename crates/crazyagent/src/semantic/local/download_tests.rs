use super::*;
use crate::semantic::ModelState;
use crate::semantic::local::tests::{
    FIXTURE_SHAPE, fixture_model_bytes, fixture_tokenizer_json, sha256_hex, write_file,
};
use crate::semantic::local::{DOWNLOAD_BYTES, LocalModel, MODEL, PINNED, Pinned, folder};
use std::time::{Duration, Instant};

/// A pin for a fixture file, served at a huggingface.co URL ending in its name. Inputs: the
/// name and the bytes. Output: the pin, its strings leaked for the test's life.
fn pin(name: &'static str, bytes: &[u8]) -> Pinned {
    Pinned {
        name,
        url: String::leak(format!("https://huggingface.co/x/{name}")),
        bytes: bytes.len() as u64,
        sha256: String::leak(sha256_hex(bytes)),
    }
}

fn fixture_pins() -> [Pinned; 2] {
    [
        pin("tokenizer.json", fixture_tokenizer_json().as_bytes()),
        pin("model.safetensors", &fixture_model_bytes()),
    ]
}

/// Writes an executable shell script that stands in for curl. Inputs: the folder and the
/// script body. Output: the script's path.
#[cfg(unix)]
fn stand_in(folder: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = write_file(folder, "curl", format!("#!/bin/sh\n{body}\n").as_bytes());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Waits up to 30 s for the download to reach a phase. Inputs: the downloads and the test.
/// Output: the phase reached.
fn wait_for(downloads: &Downloads, reached: impl Fn(Phase) -> bool) -> Phase {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let phase = downloads.phase();
        if reached(phase) {
            return phase;
        }
        assert!(Instant::now() < deadline, "still {phase:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn names_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn has_pair(args: &[String], flag: &str, value: &str) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == flag && pair[1] == value)
}

#[test]
fn a_start_without_the_pinned_size_confirmed_is_refused_and_nothing_starts() {
    let root = tempfile::tempdir().unwrap();
    let downloads = Downloads::new(root.path().to_path_buf());
    assert_eq!(downloads.start(0), Err("search_model_confirm"));
    assert_eq!(
        downloads.start(DOWNLOAD_BYTES - 1),
        Err("search_model_confirm")
    );
    assert_eq!(
        downloads.start(DOWNLOAD_BYTES + 1),
        Err("search_model_confirm")
    );
    assert_eq!(downloads.phase(), Phase::NotDownloaded);
    assert_eq!(
        downloads.state(),
        ModelState::NotDownloaded {
            size_bytes: DOWNLOAD_BYTES
        }
    );
    assert!(!root.path().join("models").exists(), "no folder is made");
}

#[test]
fn the_curl_command_sends_the_pinned_url_on_stdin_follows_one_https_redirect_and_caps_the_size() {
    let (command, config) = curl_command(Path::new("/usr/bin/curl"), &MODEL);
    assert_eq!(command.get_program(), "/usr/bin/curl");
    let args: Vec<String> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(args[0], "-q", "no .curlrc is read");
    for flag in ["--location", "--fail", "--silent"] {
        assert!(args.iter().any(|arg| arg == flag), "{flag}");
    }
    assert!(has_pair(&args, "--max-redirs", "1"));
    assert!(has_pair(&args, "--proto", "=https"));
    assert!(has_pair(&args, "--proto-redir", "=https"));
    assert!(has_pair(&args, "--max-filesize", "125018208"));
    assert!(has_pair(&args, "--config", "-"));
    assert!(
        !args
            .iter()
            .any(|arg| arg.contains("://") || arg.contains("huggingface")),
        "the URL never reaches argv: {args:?}"
    );
    let envs: Vec<(String, Option<String>)> = command
        .get_envs()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect();
    assert_eq!(
        envs,
        vec![("PATH".to_owned(), Some("/usr/bin:/bin".to_owned()))]
    );
    let config = String::from_utf8(config).unwrap();
    assert_eq!(
        config,
        format!("url = \"{}\"\nuser-agent = \"CrazyAgent\"\n", MODEL.url)
    );
    let lower = config.to_lowercase();
    for never in ["header", "cookie", "authorization", "user ="] {
        assert!(!lower.contains(never), "{never}");
    }
}

#[test]
fn a_file_is_kept_only_when_its_size_and_sha256_match_the_pin() {
    let folder = tempfile::tempdir().unwrap();
    let bytes = fixture_tokenizer_json().into_bytes();
    let pinned = pin("tokenizer.json", &bytes);
    let part = folder.path().join("tokenizer.json.part");
    let done = AtomicU64::new(0);
    receive(&bytes[..], &pinned, &part, &done).unwrap();
    assert_eq!(std::fs::read(&part).unwrap(), bytes);
    assert_eq!(done.load(Ordering::Relaxed), bytes.len() as u64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&part).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    std::fs::remove_file(&part).unwrap();
    let mut changed = bytes.clone();
    changed[0] ^= 1;
    let mut longer = bytes.clone();
    longer.push(b' ');
    let shorter = &bytes[..bytes.len() - 1];
    let cases: [(&str, &[u8], &str); 3] = [
        ("one changed byte", &changed, "search_model_mismatch"),
        ("one byte over", &longer, "search_model_mismatch"),
        ("one byte short", shorter, "search_model_mismatch"),
    ];
    for (case, sent, code) in cases {
        let done = AtomicU64::new(0);
        assert_eq!(receive(sent, &pinned, &part, &done), Err(code), "{case}");
        assert!(!part.exists(), "{case}: nothing is left behind");
    }
}

#[cfg(unix)]
#[test]
fn a_download_lands_both_files_in_the_model_folder_and_the_model_opens_from_them() {
    let source = tempfile::tempdir().unwrap();
    write_file(
        source.path(),
        "tokenizer.json",
        fixture_tokenizer_json().as_bytes(),
    );
    write_file(source.path(), "model.safetensors", &fixture_model_bytes());
    let scripts = tempfile::tempdir().unwrap();
    let curl = stand_in(
        scripts.path(),
        &format!(
            "url=$(/usr/bin/sed -n 's/^url = \"\\(.*\\)\"$/\\1/p')\nexec /bin/cat \"{}/${{url##*/}}\"",
            source.path().display()
        ),
    );
    let root = tempfile::tempdir().unwrap();
    let pins = fixture_pins();
    let total = pins[0].bytes + pins[1].bytes;
    let downloads = Downloads::with_curl(root.path().to_path_buf(), curl, pins);
    assert_eq!(downloads.start(DOWNLOAD_BYTES), Err("search_model_confirm"));
    downloads.start(total).unwrap();
    let phase = wait_for(&downloads, |phase| {
        !matches!(phase, Phase::Downloading { .. })
    });
    assert_eq!(phase, Phase::Ready);
    assert_eq!(downloads.state(), ModelState::Ready { size_bytes: total });
    let models = folder(root.path());
    assert_eq!(
        names_in(&models),
        vec!["model.safetensors", "tokenizer.json"]
    );
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&models).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }
    let model = LocalModel::open_pinned(root.path(), &pins, &FIXTURE_SHAPE).unwrap();
    let rows = model.embed(&["hello"]).unwrap();
    assert!((rows[0][0] - 0.980581).abs() < 1e-5 && (rows[0][1] - 0.196116).abs() < 1e-5);
    std::fs::write(models.join("model.safetensors"), b"swapped").unwrap();
    assert_eq!(
        LocalModel::open_pinned(root.path(), &pins, &FIXTURE_SHAPE).err(),
        Some("search_model_mismatch"),
        "every open re-hashes"
    );
}

#[cfg(unix)]
#[test]
fn cancel_kills_the_download_and_leaves_nothing_behind() {
    let scripts = tempfile::tempdir().unwrap();
    let curl = stand_in(scripts.path(), "printf partial\nexec /bin/sleep 60");
    let root = tempfile::tempdir().unwrap();
    let pins = fixture_pins();
    let downloads = Downloads::with_curl(root.path().to_path_buf(), curl, pins);
    downloads.start(pins[0].bytes + pins[1].bytes).unwrap();
    wait_for(&downloads, |phase| {
        phase == Phase::Downloading { done_bytes: 7 }
    });
    let started = Instant::now();
    downloads.cancel();
    let phase = wait_for(&downloads, |phase| {
        !matches!(phase, Phase::Downloading { .. })
    });
    assert_eq!(phase, Phase::NotDownloaded, "a cancel is not a failure");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the child was killed"
    );
    assert!(names_in(&folder(root.path())).is_empty());
}

#[cfg(unix)]
#[test]
fn a_failed_fetch_reads_failed_with_its_code_and_clears_stale_parts() {
    let scripts = tempfile::tempdir().unwrap();
    let curl = stand_in(scripts.path(), "/bin/cat >/dev/null\nexit 22");
    let root = tempfile::tempdir().unwrap();
    let models = folder(root.path());
    std::fs::create_dir_all(&models).unwrap();
    write_file(&models, "model.safetensors.part", b"left from a crash");
    let pins = fixture_pins();
    let downloads = Downloads::with_curl(root.path().to_path_buf(), curl, pins);
    downloads.start(pins[0].bytes + pins[1].bytes).unwrap();
    let phase = wait_for(&downloads, |phase| {
        !matches!(phase, Phase::Downloading { .. })
    });
    assert_eq!(phase, Phase::Failed("search_model_download_failed"));
    match downloads.state() {
        ModelState::Failed {
            problem,
            size_bytes,
        } => {
            assert_eq!(problem.code, "search_model_download_failed");
            assert_eq!(size_bytes, pins[0].bytes + pins[1].bytes);
        }
        other => panic!("{other:?}"),
    }
    assert!(names_in(&models).is_empty());
}

#[cfg(unix)]
#[test]
fn a_second_start_keeps_the_running_download_and_a_delete_while_it_runs_is_refused() {
    let scripts = tempfile::tempdir().unwrap();
    let curl = stand_in(scripts.path(), "printf partial\nexec /bin/sleep 60");
    let root = tempfile::tempdir().unwrap();
    let pins = fixture_pins();
    let total = pins[0].bytes + pins[1].bytes;
    let downloads = Downloads::with_curl(root.path().to_path_buf(), curl, pins);
    downloads.start(total).unwrap();
    wait_for(&downloads, |phase| {
        phase == Phase::Downloading { done_bytes: 7 }
    });
    assert_eq!(
        downloads.start(total),
        Ok(()),
        "a second click is not an error"
    );
    assert_eq!(
        downloads.phase(),
        Phase::Downloading { done_bytes: 7 },
        "the running download goes on, its progress kept"
    );
    assert_eq!(downloads.delete(), Err("search_model_downloading"));
    downloads.cancel();
    wait_for(&downloads, |phase| phase == Phase::NotDownloaded);
}

#[test]
fn a_start_with_both_files_ready_starts_nothing() {
    let root = tempfile::tempdir().unwrap();
    let models = folder(root.path());
    std::fs::create_dir_all(&models).unwrap();
    for pinned in PINNED {
        std::fs::File::create(models.join(pinned.name))
            .unwrap()
            .set_len(pinned.bytes)
            .unwrap();
    }
    let downloads = Downloads::new(root.path().to_path_buf());
    assert_eq!(downloads.start(DOWNLOAD_BYTES), Ok(()));
    assert_eq!(downloads.phase(), Phase::Ready, "no curl child was started");
}

#[test]
fn the_engine_calls_refuse_an_unconfirmed_start_and_read_one_download_per_root() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(
        start_download(root.path(), 0).map_err(|error| error.0),
        Err("search_model_confirm".to_owned())
    );
    assert_eq!(
        state(root.path()),
        ModelState::NotDownloaded {
            size_bytes: DOWNLOAD_BYTES
        }
    );
    let models = folder(root.path());
    std::fs::create_dir_all(&models).unwrap();
    for pinned in PINNED {
        std::fs::File::create(models.join(pinned.name))
            .unwrap()
            .set_len(pinned.bytes)
            .unwrap();
    }
    assert_eq!(
        state(root.path()),
        ModelState::Ready {
            size_bytes: DOWNLOAD_BYTES
        }
    );
    assert!(start_download(root.path(), DOWNLOAD_BYTES).is_ok());
    cancel_download(root.path());
    delete(root.path()).unwrap();
    assert!(names_in(&models).is_empty());
    assert_eq!(
        state(root.path()),
        ModelState::NotDownloaded {
            size_bytes: DOWNLOAD_BYTES
        }
    );
}

#[test]
fn files_at_their_pinned_sizes_read_ready_and_delete_removes_them() {
    let root = tempfile::tempdir().unwrap();
    let models = folder(root.path());
    std::fs::create_dir_all(&models).unwrap();
    let downloads = Downloads::new(root.path().to_path_buf());
    for pinned in PINNED {
        std::fs::File::create(models.join(pinned.name))
            .unwrap()
            .set_len(pinned.bytes)
            .unwrap();
    }
    assert_eq!(downloads.phase(), Phase::Ready);
    assert_eq!(
        downloads.state(),
        ModelState::Ready {
            size_bytes: DOWNLOAD_BYTES
        }
    );
    std::fs::File::options()
        .write(true)
        .open(models.join(MODEL.name))
        .unwrap()
        .set_len(MODEL.bytes - 1)
        .unwrap();
    assert_eq!(downloads.phase(), Phase::NotDownloaded);
    downloads.delete().unwrap();
    assert!(names_in(&models).is_empty());
    assert_eq!(downloads.phase(), Phase::NotDownloaded);
    downloads.delete().unwrap();
}

/// Writes a test model source: a stand-in curl that serves the file its URL names from its
/// own folder, and both fixture files. Input: the folder. Output: none.
fn write_source(folder: &Path) {
    stand_in(
        folder,
        "here=$(/usr/bin/dirname \"$0\")\nurl=$(/usr/bin/sed -n 's/^url = \"\\(.*\\)\"$/\\1/p')\nexec /bin/cat \"$here/${url##*/}\"",
    );
    write_file(
        folder,
        "tokenizer.json",
        fixture_tokenizer_json().as_bytes(),
    );
    write_file(folder, "model.safetensors", &fixture_model_bytes());
}

/// A test model source keeps the pinned names and URLs and takes its own files' sizes and
/// sha256, with the stand-in curl beside them.
#[test]
fn a_test_model_source_keeps_the_pinned_names_and_urls_and_takes_its_files_sizes_and_sha256() {
    let source = tempfile::tempdir().unwrap();
    let place = source.path().canonicalize().unwrap();
    write_source(&place);
    let (curl, pins) = source_pins(&place).unwrap();
    assert_eq!(curl, place.join("curl"));
    let files = [fixture_tokenizer_json().into_bytes(), fixture_model_bytes()];
    for ((pin, pinned), bytes) in pins.iter().zip(PINNED).zip(files) {
        assert_eq!(pin.name, pinned.name);
        assert_eq!(pin.url, pinned.url);
        assert_eq!(pin.bytes, bytes.len() as u64);
        assert_eq!(pin.sha256, sha256_hex(&bytes));
    }
}

/// A download from a test model source runs its stand-in curl on the real pinned URLs and
/// lands both of its files: ready, and never the network.
#[test]
fn a_download_from_a_test_model_source_lands_its_files_through_its_own_curl() {
    let source = tempfile::tempdir().unwrap();
    let place = source.path().canonicalize().unwrap();
    write_source(&place);
    let (curl, pins) = source_pins(&place).unwrap();
    let root = tempfile::tempdir().unwrap();
    let total = pins[0].bytes + pins[1].bytes;
    let downloads = Downloads::with_curl(root.path().to_path_buf(), curl, pins);
    downloads.start(total).unwrap();
    let phase = wait_for(&downloads, |phase| {
        !matches!(phase, Phase::Downloading { .. })
    });
    assert_eq!(phase, Phase::Ready);
    let models = folder(root.path());
    assert_eq!(
        std::fs::read(models.join("model.safetensors")).unwrap(),
        fixture_model_bytes()
    );
    assert_eq!(
        std::fs::read_to_string(models.join("tokenizer.json")).unwrap(),
        fixture_tokenizer_json()
    );
}

/// A test model source must be an absolute folder with its curl and two files that are not
/// empty; anything else is refused, naming what is wrong.
#[test]
fn a_test_model_source_without_its_curl_or_a_file_or_an_absolute_path_is_refused() {
    let source = tempfile::tempdir().unwrap();
    let place = source.path().canonicalize().unwrap();
    write_source(&place);
    assert!(source_pins(&place).is_ok());
    assert!(source_pins(Path::new("relative/source")).is_err());
    let empty = tempfile::tempdir().unwrap();
    let empty = empty.path().canonicalize().unwrap();
    write_source(&empty);
    write_file(&empty, "model.safetensors", b"");
    assert!(
        source_pins(&empty)
            .unwrap_err()
            .contains("model.safetensors")
    );
    std::fs::remove_file(place.join("tokenizer.json")).unwrap();
    assert!(source_pins(&place).unwrap_err().contains("tokenizer.json"));
    std::fs::remove_file(place.join("curl")).unwrap();
    assert!(source_pins(&place).unwrap_err().contains("curl"));
}
