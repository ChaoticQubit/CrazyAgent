use super::*;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use std::io::{Read, Write};
use std::net::TcpStream;

struct Signer {
    pair: minisign::KeyPair,
    key: String,
}

fn signer() -> Signer {
    let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let key = STANDARD.encode(pair.pk.to_box().unwrap().into_string());
    Signer { pair, key }
}

impl Signer {
    fn sign(&self, bytes: &[u8], trusted: &str) -> String {
        let signature = minisign::sign(
            Some(&self.pair.pk),
            &self.pair.sk,
            std::io::Cursor::new(bytes),
            Some(trusted),
            None,
        )
        .unwrap();
        STANDARD.encode(signature.into_string())
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn code<T: std::fmt::Debug>(result: Result<T, UpdateError>) -> &'static str {
    result.unwrap_err().code
}

fn read_request(stream: &mut TcpStream) {
    let mut request = Vec::new();
    let mut byte = [0_u8; 1];
    while !request.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).unwrap_or(0) == 0 {
            return;
        }
        request.push(byte[0]);
    }
}

fn serve(responses: Vec<Vec<u8>>, hold: Duration) -> (String, std::thread::JoinHandle<()>) {
    let listener = crate::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for response in responses {
            let (mut stream, _) = crate::spawn_gate::accept_tcp_std(&listener).unwrap();
            read_request(&mut stream);
            std::thread::sleep(hold);
            let _ = stream.write_all(&response);
        }
    });
    (format!("http://{address}/release"), server)
}

fn response(head: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = head.as_bytes().to_vec();
    bytes.extend_from_slice(body);
    bytes
}

#[test]
fn the_channel_file_defaults_to_stable_round_trips_and_refuses_bad_values() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(read_channel(root.path()).unwrap(), Channel::Stable);
    save_channel(root.path(), Channel::Dev).unwrap();
    assert_eq!(read_channel(root.path()).unwrap(), Channel::Dev);
    let file = root.path().join("update.toml");
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
    save_channel(root.path(), Channel::Stable).unwrap();
    assert_eq!(read_channel(root.path()).unwrap(), Channel::Stable);
    std::fs::write(&file, "").unwrap();
    assert_eq!(read_channel(root.path()).unwrap(), Channel::Stable);
    std::fs::write(&file, "channel = \"nightly\"\n").unwrap();
    assert_eq!(code(read_channel(root.path())), "update_channel_invalid");
    std::fs::write(&file, "channel = [").unwrap();
    assert_eq!(code(read_channel(root.path())), "update_channel_invalid");
    std::fs::write(&file, format!("channel = \"dev\"\n#{}", "x".repeat(5000))).unwrap();
    assert_eq!(code(read_channel(root.path())), "update_channel_invalid");
}

#[test]
fn channels_name_their_release_manifests() {
    assert_eq!(Channel::parse("stable"), Some(Channel::Stable));
    assert_eq!(Channel::parse("dev"), Some(Channel::Dev));
    assert_eq!(Channel::parse("Dev"), None);
    assert_eq!(Channel::Dev.as_str(), "dev");
    assert_eq!(
        Channel::Stable.desktop_manifest_url(),
        "https://github.com/ChaoticQubit/CrazyAgent/releases/latest/download/latest.json"
    );
    assert_eq!(
        Channel::Dev.desktop_manifest_url(),
        "https://github.com/ChaoticQubit/CrazyAgent/releases/download/dev/latest.json"
    );
    assert_eq!(
        Channel::Stable.cli_manifest_url(),
        "https://github.com/ChaoticQubit/CrazyAgent/releases/latest/download/cli-latest.json"
    );
    assert_eq!(
        Channel::Dev.cli_manifest_url(),
        "https://github.com/ChaoticQubit/CrazyAgent/releases/download/dev/cli-latest.json"
    );
}

#[test]
fn the_data_root_follows_the_release_contract() {
    let vars = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    };
    assert_eq!(
        data_root_from(vars(&[
            ("CRAZYAGENT_HOME", "/srv/crazy"),
            ("HOME", "/Users/mira")
        ])),
        Some(PathBuf::from("/srv/crazy"))
    );
    #[cfg(target_os = "macos")]
    {
        assert_eq!(
            data_root_from(vars(&[
                ("CRAZYAGENT_HOME", "relative"),
                ("HOME", "/Users/mira")
            ])),
            Some(PathBuf::from(
                "/Users/mira/Library/Application Support/CrazyAgent/engine"
            ))
        );
        assert_eq!(data_root_from(vars(&[("HOME", "relative")])), None);
    }
    #[cfg(target_os = "linux")]
    {
        assert_eq!(
            data_root_from(vars(&[("HOME", "/home/mira")])),
            Some(PathBuf::from("/home/mira/.local/share/crazyagent/engine"))
        );
        assert_eq!(
            data_root_from(vars(&[("HOME", "/home/mira"), ("XDG_DATA_HOME", "/data")])),
            Some(PathBuf::from("/data/crazyagent/engine"))
        );
    }
    #[cfg(target_os = "windows")]
    assert_eq!(
        data_root_from(vars(&[("LOCALAPPDATA", "C:\\Users\\mira\\AppData\\Local")])),
        Some(PathBuf::from(
            "C:\\Users\\mira\\AppData\\Local\\CrazyAgent\\engine"
        ))
    );
    assert_eq!(data_root_from(vars(&[])), None);
}

#[test]
fn manifests_parse_and_name_this_platform() {
    let manifest = parse_manifest(
        br#"{"version":"0.2.0-dev.202610251600","channel":"dev","commit":"abc1234","pub_date":"2026-10-25T16:00:00Z",
        "platforms":{"aarch64-apple-darwin":{"url":"https://example.com/a.tar.gz","sha256":"00","signature":"c2ln"}}}"#,
    )
    .unwrap();
    assert_eq!(manifest.version, "0.2.0-dev.202610251600");
    assert_eq!(
        manifest.platforms["aarch64-apple-darwin"].url,
        "https://example.com/a.tar.gz"
    );
    assert_eq!(
        code(parse_manifest(br#"{"version":"two","platforms":{}}"#)),
        "update_manifest_invalid"
    );
    assert_eq!(
        code(parse_manifest(br#"{"version":"0.2.0"}"#)),
        "update_manifest_invalid"
    );
    assert_eq!(code(parse_manifest(b"not json")), "update_manifest_invalid");
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    assert_eq!(target(), Some("aarch64-apple-darwin"));
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    assert_eq!(target(), Some("x86_64-unknown-linux-gnu"));
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    assert_eq!(target(), Some("x86_64-pc-windows-msvc"));
}

#[test]
fn versions_compare_as_semver_with_dev_builds_below_their_stable() {
    assert!(is_newer("0.2.0-dev.202610251600", "0.2.0-dev.202610241200").unwrap());
    assert!(is_newer("0.2.0", "0.2.0-dev.202610251600").unwrap());
    assert!(is_newer("0.3.0-dev.202611010000", "0.2.0").unwrap());
    assert!(!is_newer("0.2.0-dev.202610251600", "0.2.0").unwrap());
    assert!(!is_newer("0.2.0", "0.2.0").unwrap());
    assert!(!is_newer("0.1.0", "0.2.0").unwrap());
    assert_eq!(code(is_newer("latest", "0.2.0")), "update_manifest_invalid");
}

#[test]
fn a_signed_archive_verifies_and_tampered_or_mismatched_ones_are_refused() {
    let folder = tempfile::tempdir().unwrap();
    let archive = folder.path().join("crazyagent.tar.gz");
    let bytes: Vec<u8> = (0..70_000_u32).map(|value| (value % 251) as u8).collect();
    std::fs::write(&archive, &bytes).unwrap();
    let owner = signer();
    let signed = owner.sign(
        &bytes,
        "timestamp:1761400000\tfile:crazyagent.tar.gz\tversion:0.2.0",
    );
    let sha = sha256_hex(&bytes);
    verify_file(&archive, &sha, &signed, &owner.key, "0.2.0").unwrap();
    verify_file(&archive, &sha.to_uppercase(), &signed, &owner.key, "v0.2.0").unwrap();

    let mut tampered = bytes.clone();
    tampered[40_000] ^= 0x01;
    std::fs::write(&archive, &tampered).unwrap();
    assert_eq!(
        code(verify_file(&archive, &sha, &signed, &owner.key, "0.2.0")),
        "update_checksum_mismatch"
    );
    assert_eq!(
        code(verify_file(
            &archive,
            &sha256_hex(&tampered),
            &signed,
            &owner.key,
            "0.2.0"
        )),
        "update_signature_invalid"
    );

    std::fs::write(&archive, &bytes).unwrap();
    let stranger = signer();
    assert_eq!(
        code(verify_file(&archive, &sha, &signed, &stranger.key, "0.2.0")),
        "update_signature_invalid"
    );
    assert_eq!(
        code(verify_file(&archive, &sha, &signed, &owner.key, "0.3.0")),
        "update_signed_version_mismatch"
    );
    let unversioned = owner.sign(&bytes, "timestamp:1761400000\tfile:crazyagent.tar.gz");
    assert_eq!(
        code(verify_file(
            &archive,
            &sha,
            &unversioned,
            &owner.key,
            "0.2.0"
        )),
        "update_signature_unversioned"
    );
    assert_eq!(
        code(verify_file(
            &archive,
            &sha,
            "not base64!",
            &owner.key,
            "0.2.0"
        )),
        "update_signature_invalid"
    );
    assert_eq!(
        code(verify_file(&archive, "abc", &signed, &owner.key, "0.2.0")),
        "update_checksum_mismatch"
    );
    assert_eq!(
        code(verify_file(
            &archive,
            &sha,
            &signed,
            "REPLACE_WITH_LEADER_PUBKEY",
            "0.2.0"
        )),
        "update_key_missing"
    );
}

#[test]
fn the_compiled_in_key_comes_from_the_desktop_updater_config() {
    let owner = signer();
    let config = format!(
        r#"{{"plugins":{{"updater":{{"pubkey":"{}"}}}}}}"#,
        owner.key
    );
    assert_eq!(key_from_config(&config).unwrap(), owner.key);
    assert_eq!(
        code(key_from_config(
            r#"{"plugins":{"updater":{"pubkey":"REPLACE_WITH_LEADER_PUBKEY"}}}"#
        )),
        "update_key_missing"
    );
    assert_eq!(
        code(key_from_config(r#"{"plugins":{}}"#)),
        "update_key_missing"
    );
    let shipped = public_key();
    let config: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../apps/desktop/src-tauri/tauri.conf.json"
    ))
    .unwrap();
    let configured = config["plugins"]["updater"]["pubkey"].as_str().unwrap();
    if configured == "REPLACE_WITH_LEADER_PUBKEY" {
        assert_eq!(code(shipped), "update_key_missing");
    } else {
        assert_eq!(shipped.unwrap(), configured);
    }
}

#[test]
fn downloads_come_from_a_real_local_server_and_stop_at_the_size_cap() {
    let body: Vec<u8> = (0..3000_u32).map(|value| (value % 256) as u8).collect();
    let oversize = vec![b'x'; 10_000];
    let (url, server) = serve(
        vec![
            response(
                &format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                ),
                &body,
            ),
            response(
                &format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    oversize.len()
                ),
                &oversize,
            ),
            response("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n", &oversize),
            response(
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                b"",
            ),
        ],
        Duration::ZERO,
    );
    let timeout = Duration::from_secs(30);
    let mut received = Vec::new();
    assert_eq!(download(&url, &mut received, 4096, timeout).unwrap(), 3000);
    assert_eq!(received, body);

    let mut received = Vec::new();
    assert_eq!(
        code(download(&url, &mut received, 4096, timeout)),
        "update_too_large"
    );
    assert!(received.len() <= 4096);

    let mut received = Vec::new();
    assert_eq!(
        code(download(&url, &mut received, 4096, timeout)),
        "update_too_large"
    );
    assert!(received.len() <= 4096);

    let mut received = Vec::new();
    assert_eq!(
        code(download(&url, &mut received, 4096, timeout)),
        "update_download_failed"
    );
    assert!(received.is_empty());
    server.join().unwrap();
}

#[test]
fn a_slow_server_is_cut_off_at_the_timeout() {
    let (url, server) = serve(
        vec![response(
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n",
            b"ok",
        )],
        Duration::from_secs(4),
    );
    let started = Instant::now();
    let mut received = Vec::new();
    assert_eq!(
        code(download(&url, &mut received, 4096, Duration::from_secs(1))),
        "update_download_failed"
    );
    assert!(started.elapsed() < Duration::from_secs(4));
    server.join().unwrap();
}

#[test]
fn only_https_or_this_computer_is_downloaded_from() {
    let mut received = Vec::new();
    for url in [
        "http://example.com/release",
        "ftp://127.0.0.1/release",
        "file:///etc/passwd",
        "not a url",
    ] {
        assert_eq!(
            code(download(url, &mut received, 4096, Duration::from_secs(1))),
            "update_url_refused",
            "{url}"
        );
    }
    assert!(received.is_empty());
}

#[test]
fn executables_inside_an_installed_app_are_not_replaced() {
    assert!(inside_app_bundle(Path::new(
        "/Applications/CrazyAgent.app/Contents/MacOS/crazyagent"
    )));
    assert!(inside_app_bundle(Path::new(
        "/tmp/.mount_CrazyAb1c2d/usr/bin/crazyagent"
    )));
    assert!(!inside_app_bundle(Path::new(
        "/Users/mira/.local/bin/crazyagent"
    )));
    let install = tempfile::tempdir().unwrap();
    let executable = install.path().join("crazyagent.exe");
    std::fs::write(&executable, b"").unwrap();
    assert!(!inside_app_bundle(&executable));
    std::fs::write(install.path().join("uninstall.exe"), b"").unwrap();
    assert!(inside_app_bundle(&executable));
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

#[cfg(unix)]
#[test]
fn a_helper_that_leaves_a_process_holding_its_output_still_returns() {
    let started = Instant::now();
    let output = run_bounded(
        std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 6 & echo started; echo warned >&2"),
        Duration::from_secs(5),
        "update_service_failed",
    )
    .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "started");
    assert_eq!(String::from_utf8_lossy(&output.stderr).trim(), "warned");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[cfg(unix)]
#[test]
fn a_helper_past_its_time_limit_is_stopped_and_its_output_is_bounded() {
    let started = Instant::now();
    let result = run_bounded(
        std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30"),
        Duration::from_secs(1),
        "update_service_failed",
    );
    assert_eq!(code(result), "update_service_failed");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    let output = run_bounded(
        std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("head -c 1000000 /dev/zero"),
        Duration::from_secs(10),
        "update_service_failed",
    )
    .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout.len() as u64, OUTPUT_BYTES_MAX);
}
