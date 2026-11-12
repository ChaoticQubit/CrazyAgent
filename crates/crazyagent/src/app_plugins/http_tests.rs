use super::*;
use crate::app_plugins::Origin;
use crate::app_plugins::manifest::{self, Purpose, Runs};
use crate::app_plugins::store::Loaded;
use std::io::{BufRead, BufReader, Read, Write};

const KEY: &str = "sk-test-key-0001";

fn acme() -> String {
    "schema: 1
kind: embedding
name: acme
title: Acme
version: 1
http:
  url: https://api.acme.example/v1/embed
  hosts: [api.acme.example]
  secrets: [api_key]
  headers:
    Authorization: \"Bearer {{secret.api_key}}\"
    X-Team: research
  body:
    input: \"{{inputs}}\"
  vectors: data.*.embedding
  dimensions: 64
"
    .to_owned()
}

fn near(url: &str, host: &str) -> String {
    format!(
        "schema: 1
kind: embedding
name: near
title: Near
version: 1
http:
  url: {url}
  hosts: [{host}]
  body:
    input: \"{{{{inputs}}}}\"
    kind: \"{{{{purpose}}}}\"
  vectors: data.*.embedding
  dimensions: 64
"
    )
}

fn loaded(text: &str) -> Loaded {
    Loaded {
        manifest: manifest::parse(text).unwrap(),
        origin: Origin::User,
        bindings: Vec::new(),
    }
}

fn spec(loaded: &Loaded) -> &manifest::HttpSpec {
    match &loaded.manifest.runs {
        Runs::Http(http) => http,
        Runs::Local { .. } => panic!("an HTTP plugin"),
    }
}

fn reply(vectors: &[Vec<f64>]) -> Vec<u8> {
    let data: Vec<serde_json::Value> = vectors
        .iter()
        .map(|vector| serde_json::json!({"embedding": vector}))
        .collect();
    serde_json::to_vec(&serde_json::json!({"data": data, "model": "x"})).unwrap()
}

fn vector(head: &[f64]) -> Vec<f64> {
    let mut vector = head.to_vec();
    vector.resize(64, 0.0);
    vector
}

fn arguments(command: &std::process::Command) -> Vec<String> {
    command
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn the_key_url_and_headers_go_only_to_curl_stdin_never_to_its_arguments_or_environment() {
    let plugin = loaded(&acme());
    let secrets = [crate::credentials::SecretString::new(KEY.as_bytes().to_vec()).unwrap()];
    let config = config(
        spec(&plugin),
        &secrets,
        std::path::Path::new("/tmp/crazyagent-plugin-x/request"),
    )
    .unwrap();
    let config = String::from_utf8(config).unwrap();
    assert!(config.contains("url = \"https://api.acme.example/v1/embed\"\n"));
    assert!(config.contains("request = \"POST\"\n"));
    assert!(config.contains(&format!("header = \"Authorization: Bearer {KEY}\"\n")));
    assert!(config.contains("header = \"X-Team: research\"\n"));
    assert!(config.contains("header = \"Content-Type: application/json\"\n"));
    assert!(config.contains("data-binary = \"@/tmp/crazyagent-plugin-x/request\"\n"));
    assert!(!config.contains("resolve"));
    let command = command(spec(&plugin));
    assert_eq!(command.get_program(), CURL);
    let arguments = arguments(&command);
    for pair in [
        ["--proto", "=https"],
        ["--max-redirs", "0"],
        ["--config", "-"],
    ] {
        assert!(
            arguments.windows(2).any(|window| window == pair),
            "{pair:?}"
        );
    }
    assert!(arguments.iter().any(|argument| argument == "-q"));
    for argument in &arguments {
        assert!(!argument.contains("acme"), "{argument}");
        assert!(!argument.contains(KEY), "{argument}");
        assert!(!argument.contains("research"), "{argument}");
    }
    let environment: Vec<_> = command.get_envs().collect();
    assert_eq!(
        environment,
        [(
            std::ffi::OsStr::new("PATH"),
            Some(std::ffi::OsStr::new("/usr/bin:/bin"))
        )]
    );
}

#[test]
fn a_loopback_plugin_uses_plain_http_and_pins_localhost_to_127_0_0_1() {
    let plugin = loaded(&near("http://localhost:9000/embed", "localhost"));
    let config = String::from_utf8(
        config(spec(&plugin), &[], std::path::Path::new("/tmp/x/request")).unwrap(),
    )
    .unwrap();
    assert!(config.contains("resolve = \"localhost:9000:127.0.0.1\"\n"));
    assert!(!config.contains("Authorization"));
    let arguments = arguments(&command(spec(&plugin)));
    assert!(
        arguments
            .windows(2)
            .any(|window| window == ["--proto", "=http"])
    );
    let plain = loaded(&near("http://127.0.0.1:9000/embed", "127.0.0.1"));
    let config = String::from_utf8(
        config(spec(&plain), &[], std::path::Path::new("/tmp/x/request")).unwrap(),
    )
    .unwrap();
    assert!(!config.contains("resolve"));
}

#[test]
fn a_reply_gives_one_normalized_vector_per_input_at_the_declared_size() {
    let plugin = loaded(&acme());
    let spec = spec(&plugin);
    let got = vectors(&reply(&[vector(&[3.0, 4.0]), vector(&[])]), spec, 2).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].len(), 64);
    assert!((got[0][0] - 0.6).abs() < 1e-6 && (got[0][1] - 0.8).abs() < 1e-6);
    assert!(got[1].iter().all(|value| *value == 0.0));
    let refused = |bytes: &[u8], count: usize| vectors(bytes, spec, count).unwrap_err().0;
    assert_eq!(
        refused(&reply(&[vector(&[1.0])]), 2),
        "app_plugin_reply_invalid"
    );
    assert_eq!(
        refused(&reply(&[vec![1.0; 63], vector(&[1.0])]), 2),
        "app_plugin_reply_invalid"
    );
    assert_eq!(
        refused(&reply(&[vector(&[1e40])]), 1),
        "app_plugin_reply_invalid"
    );
    assert_eq!(refused(b"{\"data\": 1}", 1), "app_plugin_reply_invalid");
    assert_eq!(
        refused(b"{\"data\": [{\"embedding\": [\"a\"]}]}", 1),
        "app_plugin_reply_invalid"
    );
    assert_eq!(refused(b"not json", 1), "app_plugin_reply_invalid");
}

struct Served {
    request_line: String,
    body: serde_json::Value,
}

fn serve_once(status: &'static str, body: Vec<u8>) -> (u16, std::thread::JoinHandle<Served>) {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse().unwrap();
            }
        }
        let mut request = vec![0; length];
        reader.read_exact(&mut request).unwrap();
        let mut stream = stream;
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
        Served {
            request_line: request_line.trim_end().to_owned(),
            body: serde_json::from_slice(&request).unwrap(),
        }
    });
    (port, handle)
}

#[test]
fn embed_posts_the_inputs_to_a_loopback_plugin_and_reads_its_vectors() {
    let root = tempfile::tempdir().unwrap();
    let (port, served) = serve_once("200 OK", reply(&[vector(&[0.0, 2.0]), vector(&[1.0, 0.0])]));
    let plugin = loaded(&near(
        &format!("http://127.0.0.1:{port}/embed"),
        "127.0.0.1",
    ));
    let got = embed(
        root.path(),
        &plugin,
        &["first text", "second text"],
        Purpose::Query,
    )
    .unwrap();
    assert_eq!(got.len(), 2);
    assert!((got[0][1] - 1.0).abs() < 1e-6 && (got[1][0] - 1.0).abs() < 1e-6);
    let served = served.join().unwrap();
    assert_eq!(served.request_line, "POST /embed HTTP/1.1");
    assert_eq!(
        served.body,
        serde_json::json!({"input": ["first text", "second text"], "kind": "query"})
    );
}

#[test]
fn a_host_that_fails_or_redirects_is_a_failed_call() {
    let root = tempfile::tempdir().unwrap();
    for status in ["500 Internal Server Error", "302 Found"] {
        let (port, served) = serve_once(status, reply(&[vector(&[1.0])]));
        let plugin = loaded(&near(
            &format!("http://127.0.0.1:{port}/embed"),
            "127.0.0.1",
        ));
        assert_eq!(
            embed(root.path(), &plugin, &["text"], Purpose::Document)
                .unwrap_err()
                .0,
            "app_plugin_call_failed",
            "{status}"
        );
        served.join().unwrap();
    }
    let closed = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let plugin = loaded(&near(
        &format!("http://127.0.0.1:{port}/embed"),
        "127.0.0.1",
    ));
    assert_eq!(
        embed(root.path(), &plugin, &["text"], Purpose::Document)
            .unwrap_err()
            .0,
        "app_plugin_call_failed"
    );
}

#[test]
fn a_plugin_whose_bound_key_is_gone_makes_no_call() {
    let root = tempfile::tempdir().unwrap();
    let mut plugin = loaded(&acme());
    plugin.bindings = vec![crate::app_plugins::store::Bound::Key(
        crate::protocol::CredentialRef::new("plugin-key-gone").unwrap(),
    )];
    assert_eq!(
        embed(root.path(), &plugin, &["text"], Purpose::Document)
            .unwrap_err()
            .0,
        "app_plugin_needs_key"
    );
}
