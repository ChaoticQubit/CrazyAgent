use super::*;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

const PUBLIC: IpAddr = IpAddr::V4(Ipv4Addr::new(140, 82, 112, 6));

/// A lookup that answers one public address, as DNS does for the GitHub hosts. Inputs:
/// the host and port. Output: that address.
pub(super) fn public(_: &str, _: u16) -> io::Result<Vec<IpAddr>> {
    Ok(vec![PUBLIC])
}

/// A lookup no request may reach: a literal address or a refused scheme never resolves.
/// Inputs: the host and port. Output: never returns.
pub(super) fn unreached(host: &str, port: u16) -> io::Result<Vec<IpAddr>> {
    panic!("{host}:{port} was looked up")
}

/// A GET for one URL with no token, form or extra header. Inputs: the URL and whether
/// plain HTTP is allowed. Output: the request.
fn get(url: &str, allow_http: bool) -> http::Request<'static> {
    http::Request {
        url: url::Url::parse(url).unwrap(),
        accept: "application/vnd.github+json",
        token: None,
        form: None,
        headers: Vec::new(),
        allow_http,
    }
}

/// The error code of a request built with one lookup. Inputs: the request and the
/// lookup's answer. Output: the code, or none when the command was built.
fn refusal(request: &http::Request<'_>, answer: io::Result<Vec<IpAddr>>) -> Option<String> {
    request.command(|_, _| answer).err().map(|error| error.0)
}

#[test]
fn curl_command_keeps_url_headers_token_and_form_out_of_argv_and_env() {
    let secret_text = token();
    let token = secret(&secret_text);
    let request = http::Request {
        url: url::Url::parse("https://api.github.com/user?probe=1").unwrap(),
        accept: "application/json",
        token: Some(&token),
        form: Some(vec![("client_id", "Ov23-public"), ("scope", SCOPES)]),
        headers: Vec::new(),
        allow_http: false,
    };
    let (command, config) = request.command(public).unwrap();
    let argv: Vec<String> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let env: Vec<String> = command
        .get_envs()
        .map(|(name, value)| format!("{name:?}={value:?}"))
        .collect();
    for shown in argv.iter().chain(env.iter()) {
        assert!(!shown.contains(&secret_text));
        assert!(!shown.contains("api.github.com"));
        assert!(!shown.contains("Ov23-public"));
    }
    assert!(argv.contains(&"=https".to_owned()));
    assert!(argv.contains(&"--max-redirs".to_owned()));
    assert_eq!(env, vec![r#""PATH"=Some("/usr/bin:/bin")"#.to_owned()]);
    assert!(contains(
        &config,
        &format!("Authorization: Bearer {secret_text}")
    ));
    assert!(contains(&config, "client_id=Ov23-public"));
    assert!(contains(
        &config,
        "scope=repo+read%3Auser+read%3Aorg+workflow"
    ));
    let plain = http::Request {
        url: url::Url::parse("http://api.github.com/user").unwrap(),
        accept: "application/json",
        token: None,
        form: None,
        headers: Vec::new(),
        allow_http: true,
    };
    assert_eq!(
        plain.command(unreached).unwrap_err().0,
        "github_request_invalid"
    );
}

/// A GitHub host whose lookup gives a link-local or cloud metadata address, alone or next
/// to a public one, or no address at all, is refused before curl is built; so is a literal
/// metadata address, without a lookup; a failed lookup stays `github_unreachable`.
#[test]
fn a_github_host_that_resolves_to_a_metadata_address_is_refused_before_curl() {
    let api = get("https://api.github.com/user", false);
    let link_local = IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254));
    let aws_v6 = IpAddr::V6(Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254));
    let refused = Some("github_address_refused".to_owned());
    assert_eq!(refusal(&api, Ok(vec![link_local])), refused);
    assert_eq!(refusal(&api, Ok(vec![PUBLIC, aws_v6])), refused);
    assert_eq!(refusal(&api, Ok(Vec::new())), refused);
    let failed = Err(io::Error::other("no such host"));
    assert_eq!(refusal(&api, failed), Some("github_unreachable".to_owned()));
    let literal = get("https://169.254.169.254/latest/meta-data/", false);
    assert_eq!(
        literal.command(unreached).err().map(|error| error.0),
        refused
    );
    assert!(refusal(&api, Ok(vec![PUBLIC])).is_none());
}

/// curl's config pins a resolved host name to the address that was checked, an IPv6 one
/// in brackets, while a loopback literal is checked without a lookup and gets no pin.
#[test]
fn the_checked_address_is_pinned_in_the_curl_config() {
    let api = get("https://api.github.com/user", false);
    let (_, config) = api.command(public).unwrap();
    let config = String::from_utf8(config).unwrap();
    assert!(
        config.contains("resolve = \"api.github.com:443:140.82.112.6\"\n"),
        "{config}"
    );
    let v6 = IpAddr::V6(Ipv6Addr::new(0x2606, 0x50c0, 0x8000, 0, 0, 0, 0, 0x0154));
    let (_, config) = api.command(|_, _| Ok(vec![v6])).unwrap();
    let config = String::from_utf8(config).unwrap();
    assert!(
        config.contains("resolve = \"api.github.com:443:[2606:50c0:8000::154]\"\n"),
        "{config}"
    );
    let loopback = get("http://127.0.0.1:9/user", true);
    let (_, config) = loopback.command(unreached).unwrap();
    assert!(!String::from_utf8(config).unwrap().contains("resolve ="));
}

/// SECRET-FORMAT-COPIES: curl's config keeps the same bytes, with the token, a header, a
/// form value and a JSON body that need escaping or encoding, and a control byte in a
/// header still refuses the request.
#[test]
fn curl_config_bytes_stay_the_same() {
    let token = secret("gho_pin\"q\\b0123");
    let form = http::Request {
        url: url::Url::parse("https://api.github.com/user?probe=1").unwrap(),
        accept: "application/vnd.github+json",
        token: Some(&token),
        form: Some(vec![
            ("client_id", "Ov23-public"),
            ("device_code", "a b&c=d%e\"f\\g"),
        ]),
        headers: vec![("X-Pin", "quote\" back\\slash")],
        allow_http: false,
    };
    let (_, config) = form.command(public).unwrap();
    assert_eq!(
        String::from_utf8(config).unwrap(),
        concat!(
            r#"url = "https://api.github.com/user?probe=1""#,
            "\n",
            r#"header = "Accept: application/vnd.github+json""#,
            "\n",
            r#"user-agent = "CrazyAgent""#,
            "\n",
            r#"header = "X-GitHub-Api-Version: 2022-11-28""#,
            "\n",
            r#"header = "Authorization: Bearer gho_pin\"q\\b0123""#,
            "\n",
            r#"header = "X-Pin: quote\" back\\slash""#,
            "\n",
            r#"data = "client_id=Ov23-public&device_code=a+b%26c%3Dd%25e%22f%5Cg""#,
            "\n",
        )
    );
    let json = http::Request {
        url: url::Url::parse("https://api.github.com/repos/x/y").unwrap(),
        accept: "application/json",
        token: None,
        form: None,
        headers: Vec::new(),
        allow_http: false,
    };
    let (_, config) = json.json_command(r#"{"a":"q\"b\\c"}"#, public).unwrap();
    assert_eq!(
        String::from_utf8(config).unwrap(),
        concat!(
            r#"url = "https://api.github.com/repos/x/y""#,
            "\n",
            r#"header = "Accept: application/json""#,
            "\n",
            r#"user-agent = "CrazyAgent""#,
            "\n",
            r#"header = "X-GitHub-Api-Version: 2022-11-28""#,
            "\n",
            r#"request = "POST""#,
            "\n",
            r#"header = "Content-Type: application/json""#,
            "\n",
            r#"data-raw = "{\"a\":\"q\\\"b\\\\c\"}""#,
            "\n",
        )
    );
    let refused = http::Request {
        headers: vec![("X-Pin", "line\u{1}break")],
        ..form
    };
    assert_eq!(
        refused.command(unreached).err().unwrap().0,
        "github_request_invalid"
    );
}
