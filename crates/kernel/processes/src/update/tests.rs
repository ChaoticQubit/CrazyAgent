use super::test_support::{TestResult, at, code, joined, read_request, response, said, serve};
use super::*;

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
    #[cfg(unix)]
    const CUSTOM: &str = "/srv/crazy";
    #[cfg(not(unix))]
    const CUSTOM: &str = r"C:\srv\crazy";
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
            ("CRAZYAGENT_HOME", CUSTOM),
            ("HOME", "/Users/mira")
        ])),
        Some(PathBuf::from(CUSTOM))
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
    #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
    assert_eq!(target(), Some("x86_64-unknown-linux-gnu"));
    #[cfg(all(target_os = "windows", target_arch = "x86_64", target_env = "msvc"))]
    assert_eq!(target(), Some("x86_64-pc-windows-msvc"));
}

/// Only the platforms a release archive is published for name a target: an operating
/// system and processor with a published build but another C library or toolchain, such as
/// musl Linux or GNU Windows, names none, so an update never fetches an archive built for
/// another runtime.
#[test]
fn a_platform_with_no_release_build_names_no_target() {
    for (os, arch, env, published) in [
        ("macos", "aarch64", "", Some("aarch64-apple-darwin")),
        ("macos", "x86_64", "", Some("x86_64-apple-darwin")),
        ("linux", "x86_64", "gnu", Some("x86_64-unknown-linux-gnu")),
        ("linux", "aarch64", "gnu", Some("aarch64-unknown-linux-gnu")),
        ("windows", "x86_64", "msvc", Some("x86_64-pc-windows-msvc")),
        ("linux", "x86_64", "musl", None),
        ("linux", "aarch64", "musl", None),
        ("windows", "x86_64", "gnu", None),
        ("windows", "aarch64", "msvc", None),
        ("freebsd", "x86_64", "", None),
    ] {
        assert_eq!(target_for(os, arch, env), published, "{os} {arch} {env}");
    }
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

/// Build metadata carries no precedence in semver, so two releases that differ only in it
/// are the same release: neither reads as newer than the other.
#[test]
fn versions_that_differ_only_in_build_metadata_are_not_newer() {
    assert!(!is_newer("1.2.3+b", "1.2.3+a").unwrap());
    assert!(!is_newer("1.2.3+a", "1.2.3+b").unwrap());
    assert!(!is_newer("1.2.3+build.9", "1.2.3").unwrap());
    assert!(is_newer("1.2.4+a", "1.2.3+b").unwrap());
}

#[test]
fn a_signed_archive_verifies_and_tampered_or_mismatched_ones_are_refused() -> TestResult {
    let folder = tempfile::tempdir()?;
    let archive = folder.path().join("crazyagent.tar.gz");
    let bytes: Vec<u8> = (0..70_000_u32).map(|value| (value % 251) as u8).collect();
    at(&archive, std::fs::write(&archive, &bytes))?;
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
    at(&archive, std::fs::write(&archive, &tampered))?;
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

    at(&archive, std::fs::write(&archive, &bytes))?;
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
    Ok(())
}

/// A trusted comment that names a field twice is refused before its version is read, so the
/// updater and the release asset check can never read different versions from one signature.
#[test]
fn a_trusted_comment_that_names_a_field_twice_is_refused() -> TestResult {
    let folder = tempfile::tempdir()?;
    let archive = folder.path().join("crazyagent.tar.gz");
    let bytes = b"release archive bytes".to_vec();
    at(&archive, std::fs::write(&archive, &bytes))?;
    let owner = signer();
    let sha = sha256_hex(&bytes);
    for trusted in [
        "timestamp:1761400000\tfile:crazyagent.tar.gz\tversion:0.2.0\tversion:0.3.0",
        "timestamp:1761400000\tfile:crazyagent.tar.gz\tversion:0.3.0\tversion:0.2.0",
        "timestamp:1761400000\tfile:crazyagent.tar.gz\tfile:other.tar.gz\tversion:0.2.0",
    ] {
        let signed = owner.sign(&bytes, trusted);
        assert_eq!(
            code(verify_file(&archive, &sha, &signed, &owner.key, "0.2.0")),
            "update_signature_ambiguous",
            "{trusted}"
        );
    }
    Ok(())
}

#[test]
fn the_desktop_config_names_only_the_stable_manifest_and_each_channel_has_its_own() {
    let config: serde_json::Value = serde_json::from_str(DESKTOP_CONFIG).unwrap();
    assert_eq!(
        config.pointer("/plugins/updater/endpoints").unwrap(),
        &serde_json::json!([Channel::Stable.desktop_manifest_url()])
    );
    for channel in [Channel::Stable, Channel::Dev] {
        for url in [channel.desktop_manifest_url(), channel.cli_manifest_url()] {
            let parsed = allowed_url(url).unwrap();
            assert_eq!(parsed.host_str(), Some("github.com"), "{url}");
            assert!(
                parsed
                    .path()
                    .starts_with("/ChaoticQubit/CrazyAgent/releases/"),
                "{url}"
            );
            let tagged = parsed.path().contains("/releases/download/dev/");
            assert_eq!(tagged, channel == Channel::Dev, "{url}");
            let latest = parsed.path().contains("/releases/latest/download/");
            assert_eq!(latest, channel == Channel::Stable, "{url}");
        }
    }
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
        "../../../../../apps/desktop/src-tauri/tauri.conf.json"
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
fn downloads_come_from_a_real_local_server_and_stop_at_the_size_cap() -> TestResult {
    let body: Vec<u8> = (0..3000_u32).map(|value| (value % 256) as u8).collect();
    let oversize = vec![b'x'; 10_000];
    let (url, server) = serve(vec![
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
    ])?;
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
        "update_download_missing"
    );
    assert!(received.is_empty());
    joined(server)?;
    Ok(())
}

#[test]
fn a_slow_server_is_cut_off_at_the_timeout() -> TestResult {
    let listener = crate::spawn_gate::bind_tcp_std("127.0.0.1:0")?;
    let url = format!("http://{}/release", listener.local_addr()?);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = crate::spawn_gate::accept_tcp_std(&listener)?;
        read_request(&mut stream);
        std::thread::sleep(Duration::from_secs(4));
        let _ = stream.write_all(&response(
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n",
            b"ok",
        ));
        Ok(())
    });
    let started = Instant::now();
    let mut received = Vec::new();
    let error = download(&url, &mut received, 4096, Duration::from_secs(1)).unwrap_err();
    assert_eq!(
        (error.code, error.message.as_str()),
        (
            "update_download_failed",
            "The update server didn't answer in time. Check your connection and try again."
        )
    );
    assert!(started.elapsed() < Duration::from_secs(4));
    joined(server)?;
    Ok(())
}

#[test]
fn an_http_error_is_read_from_curl_whatever_its_exit_code() {
    assert_eq!(
        said(download_failure(
            Some(56),
            b"curl: (56) The requested URL returned error: 404\n"
        )),
        (
            "update_download_missing",
            "The update server doesn't have this download. Try again later.".to_owned()
        )
    );
    assert_eq!(
        said(download_failure(
            Some(56),
            b"curl: (56) The requested URL returned error: 502\n"
        )),
        (
            "update_download_failed",
            "The update server answered with an error. Try again later.".to_owned()
        )
    );
    assert_eq!(
        said(download_failure(
            Some(6),
            b"curl: (6) Could not resolve host: github.com\n"
        )),
        (
            "update_download_failed",
            "Couldn't reach the update server. Check your connection and try again.".to_owned()
        )
    );
}

#[cfg(unix)]
#[test]
fn a_failed_service_step_reads_as_one_plain_sentence_without_its_usage_text() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let folder = tempfile::tempdir()?;
    let executable = folder.path().join("crazyagent");
    at(
        &executable,
        std::fs::write(
            &executable,
            "#!/bin/sh\necho 'Usage: crazyagent service <install|start|stop>'\necho 'error: unrecognized subcommand' >&2\nexit 2\n",
        ),
    )?;
    at(
        &executable,
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)),
    )?;
    assert_eq!(
        said(service_command(&executable, "stop").unwrap_err()),
        (
            "update_service_failed",
            "Couldn't stop the engine for the update, so nothing changed.".to_owned()
        )
    );
    assert_eq!(
        said(service_command(&executable, "start").unwrap_err()),
        (
            "update_service_failed",
            "The engine didn't start. Start it with: crazyagent service start".to_owned()
        )
    );
    Ok(())
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
fn executables_inside_an_installed_app_are_not_replaced() -> TestResult {
    assert!(inside_app_bundle(Path::new(
        "/Applications/CrazyAgent.app/Contents/MacOS/crazyagent"
    )));
    assert!(inside_app_bundle(Path::new(
        "/tmp/.mount_CrazyAb1c2d/usr/bin/crazyagent"
    )));
    assert!(!inside_app_bundle(Path::new(
        "/Users/mira/.local/bin/crazyagent"
    )));
    let install = tempfile::tempdir()?;
    let executable = install.path().join("crazyagent.exe");
    at(&executable, std::fs::write(&executable, b""))?;
    assert!(!inside_app_bundle(&executable));
    let uninstall = install.path().join("uninstall.exe");
    at(&uninstall, std::fs::write(&uninstall, b""))?;
    assert!(inside_app_bundle(&executable));
    Ok(())
}

/// An update command that exits is heard through one watch armed on the calling thread, not
/// found by a poll, and its output is kept.
#[test]
fn an_update_command_that_exits_is_heard_through_one_watch() {
    let before = crate::exit_wait::tests::hooks::arms();
    #[cfg(unix)]
    let mut command = std::process::Command::new("/bin/echo");
    #[cfg(windows)]
    let mut command = std::process::Command::new(windows_tool("cmd.exe"));
    #[cfg(windows)]
    command.args(["/C", "echo"]);
    command.arg("x");
    let output = run_bounded(
        &mut command,
        Duration::from_secs(30),
        "update_service_failed",
    )
    .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "x");
    assert_eq!(crate::exit_wait::tests::hooks::arms() - before, 1);
}

/// An update command past its limit is killed, then reaped once through its watch after its
/// waiter answers that it exited, even with that waiter held back until the reap releases
/// it. Linux takes the waiter thread too, with process descriptors refused.
#[cfg(unix)]
#[test]
fn an_update_command_past_its_limit_is_received_then_reaped_once() {
    use crate::exit_wait::{Heard, tests::hooks};
    #[cfg(target_os = "linux")]
    hooks::refuse_pidfd(Some(rustix::io::Errno::NOSYS));
    hooks::hold(true);
    let (before, _) = hooks::reaps();
    let result = run_bounded(
        std::process::Command::new("/bin/sleep").arg("5"),
        Duration::from_millis(200),
        "update_service_failed",
    );
    hooks::hold(false);
    #[cfg(target_os = "linux")]
    hooks::refuse_pidfd(None);
    let (code, message) = said(result.unwrap_err());
    assert_eq!(code, "update_service_failed");
    assert!(message.contains("did not finish within"), "{message}");
    let (after, heard) = hooks::reaps();
    assert_eq!(after - before, 1);
    assert_eq!(heard, Some(Heard::Exited));
}

/// An update command whose watch fails returns that failure at once, with the command killed
/// and reaped, not left to run out its 5 s: its process number is gone when the call returns.
#[cfg(unix)]
#[test]
fn an_update_command_whose_watch_fails_is_killed_and_reaped() {
    use crate::exit_wait::tests::hooks;
    hooks::force_watch_error(true);
    let started = Instant::now();
    let result = run_bounded(
        std::process::Command::new("/bin/sleep").arg("5"),
        Duration::from_secs(30),
        "update_service_failed",
    );
    let pid = hooks::failed_watch_child();
    hooks::force_watch_error(false);
    let (code, message) = said(result.unwrap_err());
    assert_eq!(code, "update_service_failed");
    assert!(
        message.contains("a test forced the watch to fail"),
        "{message}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    let pid = pid.expect("the watch was forced to fail");
    let alive = crate::spawn_gate::status(
        std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    assert!(
        !alive.success(),
        "the command {pid} outlived its failed watch"
    );
}

/// On Windows, an update command past its limit is ended at the limit and reaped once
/// through its watch, not waited on until its own exit 5 s later.
#[cfg(windows)]
#[test]
fn an_update_command_past_its_limit_is_ended_and_reaped_once_on_windows() {
    use crate::exit_wait::tests::hooks;
    let (before, _) = hooks::reaps();
    let started = Instant::now();
    let result = run_bounded(
        std::process::Command::new(windows_tool("PING.EXE")).args(["-n", "6", "127.0.0.1"]),
        Duration::from_millis(200),
        "update_service_failed",
    );
    assert_eq!(code(result), "update_service_failed");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(hooks::reaps().0 - before, 1);
}

/// A tool under the Windows system folder. Input: its file name. Output: its path.
#[cfg(windows)]
fn windows_tool(name: &str) -> std::path::PathBuf {
    let root = std::env::var_os("SystemRoot").expect("the SystemRoot variable is set");
    std::path::PathBuf::from(root).join("System32").join(name)
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

fn signed_archive(folder: &Path) -> TestResult<(PathBuf, Vec<u8>)> {
    let archive = folder.join("crazyagent.tar.gz");
    let bytes: Vec<u8> = (0..9_000_u32).map(|value| (value % 241) as u8).collect();
    at(&archive, std::fs::write(&archive, &bytes))?;
    Ok((archive, bytes))
}

#[test]
fn cli_update_refuses_a_bad_signature_archive() -> TestResult {
    let folder = tempfile::tempdir()?;
    let (archive, bytes) = signed_archive(folder.path())?;
    let release = signer();
    let other = signer();
    let trusted = "timestamp:1761400000\tfile:crazyagent.tar.gz\tversion:0.2.0";
    let sha = sha256_hex(&bytes);
    verify_file(
        &archive,
        &sha,
        &release.sign(&bytes, trusted),
        &release.key,
        "0.2.0",
    )
    .unwrap();
    assert_eq!(
        code(verify_file(
            &archive,
            &sha,
            &other.sign(&bytes, trusted),
            &release.key,
            "0.2.0"
        )),
        "update_signature_invalid"
    );
    Ok(())
}

#[test]
fn cli_update_refuses_an_unsigned_archive() -> TestResult {
    let folder = tempfile::tempdir()?;
    let (archive, bytes) = signed_archive(folder.path())?;
    let release = signer();
    let sha = sha256_hex(&bytes);
    for unsigned in ["", "   ", &STANDARD.encode("")] {
        assert_eq!(
            code(verify_file(&archive, &sha, unsigned, &release.key, "0.2.0")),
            "update_signature_invalid",
            "{unsigned:?}"
        );
    }
    Ok(())
}

#[test]
fn the_desktop_config_public_key_is_the_one_read() -> TestResult {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../apps/desktop/src-tauri/tauri.conf.json");
    let text = at(&path, std::fs::read_to_string(&path))?;
    let config: serde_json::Value = serde_json::from_str(&text)?;
    let configured = config["plugins"]["updater"]["pubkey"]
        .as_str()
        .ok_or_else(|| format!("{path:?} names no plugins.updater.pubkey string"))?;
    assert_eq!(DESKTOP_CONFIG, text);
    if configured == KEY_PLACEHOLDER {
        assert_eq!(code(public_key()), "update_key_missing");
    } else {
        assert_eq!(public_key().unwrap(), configured);
    }
    Ok(())
}

/// A URL holding `[`, `]`, `{` or `}` reaches the server as the URL parser wrote it, in one
/// request, never expanded by curl as a range or a list.
#[test]
fn a_url_with_brackets_and_braces_reaches_the_server_as_written() -> TestResult {
    use std::io::{Read, Write};
    let listener = crate::spawn_gate::bind_tcp_std("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = crate::spawn_gate::accept_tcp_std(&listener)?;
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte)? == 1 {
            head.push(byte[0]);
        }
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")?;
        Ok(String::from_utf8_lossy(&head)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned())
    });
    let url = format!("http://127.0.0.1:{port}/release/x[1-2]/y{{a,b}}");
    let mut received = Vec::new();
    assert_eq!(
        download(&url, &mut received, 64, Duration::from_secs(30)).unwrap(),
        2
    );
    assert_eq!(received, b"ok");
    let path = allowed_url(&url).unwrap().path().to_owned();
    assert!(path.contains("[1-2]"), "{path}");
    assert_eq!(joined(server)?, format!("GET {path} HTTP/1.1"));
    Ok(())
}

/// Writers of the update channel at the same moment never clobber each other: each of 8
/// threads saves stable or dev 50 times and every save succeeds, `update.toml` then reads
/// as one of the two channels, and no half-written file is left in the folder.
#[test]
fn channel_writers_at_the_same_moment_each_save_whole() -> TestResult {
    let folder = tempfile::tempdir()?;
    let root = folder.path().to_path_buf();
    let writers: Vec<_> = (0..8)
        .map(|n| {
            let root = root.clone();
            std::thread::spawn(move || {
                let channel = if n % 2 == 0 {
                    Channel::Stable
                } else {
                    Channel::Dev
                };
                (0..50)
                    .map(|_| write_channel(&root, channel))
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    for writer in writers {
        for saved in writer.join().unwrap() {
            assert!(saved.is_ok(), "{:?}", saved.err().map(|error| error.code));
        }
    }
    assert!(matches!(
        read_channel(&root).unwrap(),
        Channel::Stable | Channel::Dev
    ));
    let left: Vec<String> = at(&root, std::fs::read_dir(&root))?
        .map(|entry| Ok(at(&root, entry)?.file_name().to_string_lossy().into_owned()))
        .collect::<TestResult<_>>()?;
    assert_eq!(left, vec!["update.toml".to_owned()]);
    Ok(())
}

/// A save while another program holds `update.toml` without delete sharing waits a bounded
/// time, then reports the file busy and leaves no half-written file; once the file is let
/// go the save goes through.
#[cfg(windows)]
#[test]
fn a_channel_file_another_program_holds_is_busy_and_saves_once_let_go() -> TestResult {
    use std::os::windows::fs::OpenOptionsExt;
    let folder = tempfile::tempdir()?;
    let root = folder.path();
    write_channel(root, Channel::Stable).unwrap();
    let file = root.join("update.toml");
    let held = at(
        &file,
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x2)
            .open(&file),
    )?;
    let started = Instant::now();
    let busy = write_channel(root, Channel::Dev).unwrap_err();
    assert_eq!(busy.code, "update_channel_busy", "{}", busy.message);
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(held);
    write_channel(root, Channel::Dev).unwrap();
    assert_eq!(read_channel(root).unwrap(), Channel::Dev);
    let left: Vec<String> = at(root, std::fs::read_dir(root))?
        .map(|entry| Ok(at(root, entry)?.file_name().to_string_lossy().into_owned()))
        .collect::<TestResult<_>>()?;
    assert_eq!(left, vec!["update.toml".to_owned()]);
    Ok(())
}
