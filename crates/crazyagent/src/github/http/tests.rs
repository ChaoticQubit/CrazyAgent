use super::*;

#[test]
fn each_exchange_failure_maps_to_its_github_code() {
    assert_eq!(code(ExchangeError::Unreachable).0, "github_unreachable");
    assert_eq!(code(ExchangeError::ReplyInvalid).0, "github_reply_invalid");
}

#[cfg(unix)]
#[test]
fn a_request_no_server_answers_is_github_unreachable() {
    let request = Request {
        url: url::Url::parse("http://127.0.0.1:1/user").unwrap(),
        accept: "application/vnd.github+json",
        token: None,
        form: None,
        headers: Vec::new(),
        allow_http: true,
    };
    assert_eq!(
        request.send().err().map(|error| error.0),
        Some("github_unreachable".to_owned())
    );
}
