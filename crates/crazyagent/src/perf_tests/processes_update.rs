use crate::perf_kit::{Row, SAMPLES, time};
use crate::update::{
    Channel, data_root, data_root_from, download, fail, inside_app_bundle, io_failure, is_newer,
    key_from_config, not_published, parse_manifest, public_key, read_channel, save_channel,
    service_command, service_step, swap, system_tool, target, unpack, verify_file,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::hint::black_box;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const FAST: usize = 1000;
const PARSE: usize = 100;
const FILES: usize = 10;
const ONE: usize = 1;
const ONE_SHOT: usize = SAMPLES / 10 + SAMPLES;
const ARCHIVE_BYTES: u32 = 9_000;
const VERSION: &str = "0.2.0";
const TRUSTED: &str = "timestamp:1761400000\tfile:crazyagent.tar.gz\tversion:0.2.0";
const MANIFEST: &[u8] = br#"{"version":"0.2.0-dev.202610251600","channel":"dev","commit":"abc1234","pub_date":"2026-10-25T16:00:00Z","platforms":{"aarch64-apple-darwin":{"url":"https://example.com/a.tar.gz","sha256":"00","signature":"c2ln"}}}"#;
const CONFIG: &str =
    r#"{"plugins":{"updater":{"pubkey":"dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk="}}}"#;
const ENGINE: &str = "#!/bin/sh\necho crazyagent 0.2.0\n";
const DOWNLOAD_LIMIT: u64 = 1024 * 1024;
const DOWNLOAD_TIME: Duration = Duration::from_secs(30);

fn row(name: &'static str, batch: usize) -> Row {
    Row {
        layer: 1,
        module: "processes",
        name,
        batch,
    }
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

/// A real HTTP server on this computer's loopback address that answers one request per
/// response, in order, as the 2.5 update test support does. Input: the raw responses.
/// Output: the URL to fetch and the server thread, which ends after the last response.
fn serve(responses: Vec<Vec<u8>>) -> (String, std::thread::JoinHandle<()>) {
    let listener = crate::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for response in responses {
            let (mut stream, _) = crate::spawn_gate::accept_tcp_std(&listener).unwrap();
            read_request(&mut stream);
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

fn environment(name: &str) -> Option<OsString> {
    match name {
        "HOME" => Some("/Users/perf".into()),
        _ => None,
    }
}

fn engine(folder: &Path) -> PathBuf {
    std::fs::create_dir_all(folder).unwrap();
    let path = folder.join("crazyagent");
    std::fs::write(&path, ENGINE).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A signed release archive: fixed bytes, their SHA-256 in hex, a minisign signature over
/// them for version 0.2.0 and the matching public key, both base64 as a manifest carries
/// them. Input: the folder. Output: the archive, hex digest, signature and key.
fn signed_archive(folder: &Path) -> (PathBuf, String, String, String) {
    let bytes: Vec<u8> = (0..ARCHIVE_BYTES).map(|at| (at % 241) as u8).collect();
    let archive = folder.join("crazyagent.tar.gz");
    std::fs::write(&archive, &bytes).unwrap();
    let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let key = STANDARD.encode(pair.pk.to_box().unwrap().into_string());
    let signature = minisign::sign(
        Some(&pair.pk),
        &pair.sk,
        std::io::Cursor::new(&bytes),
        Some(TRUSTED),
        None,
    )
    .unwrap();
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    (
        archive,
        sha256,
        STANDARD.encode(signature.into_string()),
        key,
    )
}

/// A gzip tar holding one stand-in `crazyagent`, made with the system tar. Input: the
/// folder. Output: the archive.
fn release_tar(folder: &Path) -> PathBuf {
    let content = folder.join("content");
    engine(&content);
    let archive = folder.join("release.tar.gz");
    let made = crate::spawn_gate::status(
        Command::new(system_tool("tar"))
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&content)
            .arg("crazyagent"),
    )
    .unwrap();
    assert!(made.success());
    archive
}

/// Times the 2.4.4 path of every 2.5 update row, under the 2.5 row names, on the same
/// stand-ins; `update.write_channel` times 2.4.4's `save_channel`. The stand-ins are a
/// temporary data root, a loopback HTTP server for `download`, a freshly signed archive, a
/// gzip tar of a stand-in engine and shell scripts that answer `--version` and
/// `service <action>` as the engine does. One-shot calls (`unpack`, `swap`) take a fresh
/// folder per call, made before timing. `update.control.curl_version` spawns
/// `curl --version` through the spawn gate the same way in both twins, as the control for
/// `update.download`. Prints one PERF line per call, asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn update_calls() {
    let home = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(home.path()).unwrap();
    let root = home.join("engine");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    save_channel(&root, Channel::Dev).unwrap();
    let (archive, sha256, signature, key) = signed_archive(&home);
    let tar = release_tar(&home);
    let script = engine(&home.join("app"));
    let pairs: Vec<(PathBuf, PathBuf)> = (0..ONE_SHOT)
        .map(|at| {
            let folder = home.join("swaps").join(at.to_string());
            engine(&folder.join("install"));
            engine(&folder.join("staged"));
            (folder.join("install"), folder.join("staged"))
        })
        .collect();
    let unpacked: Vec<PathBuf> = (0..ONE_SHOT)
        .map(|at| home.join("unpacked").join(at.to_string()))
        .collect();
    std::fs::create_dir_all(home.join("unpacked")).unwrap();
    let body: Vec<u8> = (0..ARCHIVE_BYTES).map(|at| (at % 241) as u8).collect();
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let (url, server) = serve((0..ONE_SHOT + 1).map(|_| response(&head, &body)).collect());
    assert_eq!(read_channel(&root).unwrap(), Channel::Dev);
    assert!(parse_manifest(MANIFEST).is_ok() && is_newer("0.2.1", VERSION).unwrap());
    assert!(verify_file(&archive, &sha256, &signature, &key, VERSION).is_ok());
    assert!(key_from_config(CONFIG).is_ok() && !inside_app_bundle(&script));
    assert_eq!(
        service_command(&script, "status").unwrap(),
        "crazyagent 0.2.0"
    );
    let mut fetched = Vec::new();
    assert_eq!(
        download(&url, &mut fetched, DOWNLOAD_LIMIT, DOWNLOAD_TIME).unwrap(),
        u64::from(ARCHIVE_BYTES)
    );
    time(&row("update.Channel.parse", FAST), SAMPLES, || {
        black_box(Channel::parse(black_box("dev")));
    });
    time(&row("update.Channel.as_str", FAST), SAMPLES, || {
        black_box(black_box(Channel::Dev).as_str());
    });
    time(
        &row("update.Channel.desktop_manifest_url", FAST),
        SAMPLES,
        || {
            black_box(black_box(Channel::Dev).desktop_manifest_url());
        },
    );
    time(
        &row("update.Channel.cli_manifest_url", FAST),
        SAMPLES,
        || {
            black_box(black_box(Channel::Dev).cli_manifest_url());
        },
    );
    time(&row("update.fail", FAST), SAMPLES, || {
        black_box(fail(black_box("update_io"), black_box("Can't save.")));
    });
    time(&row("update.io_failure", FAST), SAMPLES, || {
        black_box(io_failure(
            black_box("Can't save"),
            std::io::Error::from(std::io::ErrorKind::NotFound),
        ));
    });
    time(&row("update.data_root", FAST), SAMPLES, || {
        black_box(data_root());
    });
    time(&row("update.data_root_from", FAST), SAMPLES, || {
        black_box(data_root_from(environment));
    });
    time(&row("update.read_channel", FILES), SAMPLES, || {
        black_box(read_channel(black_box(&root)).unwrap());
    });
    time(&row("update.write_channel", ONE), SAMPLES, || {
        save_channel(black_box(&root), Channel::Dev).unwrap();
    });
    time(&row("update.parse_manifest", PARSE), SAMPLES, || {
        black_box(parse_manifest(black_box(MANIFEST)).unwrap());
    });
    time(&row("update.target", FAST), SAMPLES, || {
        black_box(target());
    });
    time(&row("update.is_newer", FAST), SAMPLES, || {
        black_box(is_newer(black_box("0.2.1"), black_box(VERSION)).unwrap());
    });
    time(&row("update.system_tool", FILES), SAMPLES, || {
        black_box(system_tool(black_box("curl")));
    });
    let curl = system_tool("curl");
    time(&row("update.control.curl_version", ONE), SAMPLES, || {
        let mut command = Command::new(&curl);
        command
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = crate::spawn_gate::spawn(&mut command).unwrap();
        assert!(child.wait().unwrap().success());
    });
    time(&row("update.download", ONE), SAMPLES, || {
        let mut fetched = Vec::with_capacity(body.len());
        black_box(download(&url, &mut fetched, DOWNLOAD_LIMIT, DOWNLOAD_TIME).unwrap());
    });
    server.join().unwrap();
    time(&row("update.verify_file", ONE), SAMPLES, || {
        verify_file(&archive, &sha256, &signature, &key, VERSION).unwrap();
    });
    time(&row("update.key_from_config", PARSE), SAMPLES, || {
        black_box(key_from_config(black_box(CONFIG)).unwrap());
    });
    time(&row("update.public_key", PARSE), SAMPLES, || {
        black_box(public_key().unwrap());
    });
    time(&row("update.inside_app_bundle", FAST), SAMPLES, || {
        black_box(inside_app_bundle(black_box(&script)));
    });
    let mut fresh = unpacked.iter();
    time(&row("update.unpack", ONE), SAMPLES, || {
        black_box(unpack(&tar, fresh.next().unwrap()).unwrap());
    });
    assert!(fresh.next().is_none() && unpacked[0].join("crazyagent").is_file());
    let mut fresh = pairs.iter();
    time(&row("update.swap", ONE), SAMPLES, || {
        let (install, staged) = fresh.next().unwrap();
        swap(install, staged, VERSION).unwrap();
    });
    assert!(fresh.next().is_none() && !pairs[0].1.join("crazyagent").exists());
    time(&row("update.service_command", ONE), SAMPLES, || {
        black_box(service_command(&script, "status").unwrap());
    });
    time(&row("update.service_step", ONE), SAMPLES, || {
        black_box(
            service_step(Command::new(&script).arg("service").arg("status"), "status").unwrap(),
        );
    });
    time(&row("update.not_published", FAST), SAMPLES, || {
        black_box(not_published(black_box(Channel::Dev)));
    });
}
