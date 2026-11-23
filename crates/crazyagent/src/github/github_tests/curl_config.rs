use super::*;

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
    let (_, config) = form.command().unwrap();
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
    let (_, config) = json.json_command(r#"{"a":"q\"b\\c"}"#).unwrap();
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
    assert_eq!(refused.command().err().unwrap().0, "github_request_invalid");
}
