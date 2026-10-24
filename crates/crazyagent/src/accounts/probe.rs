use crate::engine::EngineError;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const RESPONSE_MAX: usize = 1 << 20;
const MODELS_MAX: usize = 500;

pub(crate) struct Response {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
}

fn failure(code: &str) -> EngineError {
    EngineError(code.into())
}

fn authority(url: &url::Url) -> Option<String> {
    let port = url.port_or_known_default()?;
    let host = match url.host()? {
        url::Host::Ipv6(address) => format!("[{address}]"),
        host => host.to_string(),
    };
    Some(format!("{host}:{port}"))
}

/// Opens and closes one TCP connection to the URL's host and port within the timeout.
/// Input: an HTTP(S) URL. Output: whether anything accepted the connection; nothing is sent.
pub(crate) async fn reachable(url: &url::Url, timeout: Duration) -> bool {
    let Some(address) = authority(url) else {
        return false;
    };
    matches!(
        tokio::time::timeout(timeout, crate::spawn_gate::connect_tcp(address)).await,
        Ok(Ok(_))
    )
}

fn joined(url: &url::Url, suffix: &str) -> String {
    format!("{}{suffix}", url.path().trim_end_matches('/'))
}

/// Sends one bounded plain-HTTP GET and reads the whole reply. An optional bearer key is
/// written only into the Authorization header of this request and wiped afterwards.
/// Inputs: http URL, request path, key and timeout. Output: status and body, or an error.
pub(crate) async fn get(
    url: &url::Url,
    path: &str,
    bearer: Option<&[u8]>,
    timeout: Duration,
) -> Result<Response, EngineError> {
    if url.scheme() != "http" {
        return Err(failure("account_probe_https_unsupported"));
    }
    let address = authority(url).ok_or_else(|| failure("account_url_invalid"))?;
    tokio::time::timeout(timeout, async {
        let mut stream = crate::spawn_gate::connect_tcp(&address).await.map_err(|_| failure("account_url_unreachable"))?;
        let mut request = format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nAccept: application/json\r\nConnection: close\r\n").into_bytes();
        if let Some(key) = bearer {
            request.extend_from_slice(b"Authorization: Bearer ");
            request.extend_from_slice(key);
            request.extend_from_slice(b"\r\n");
        }
        request.extend_from_slice(b"\r\n");
        let sent = stream.write_all(&request).await;
        request.fill(0);
        sent.map_err(|_| failure("account_url_unreachable"))?;
        let mut reply = Vec::new();
        (&mut stream).take(RESPONSE_MAX as u64 + 1).read_to_end(&mut reply).await.map_err(|_| failure("account_url_unreachable"))?;
        if reply.len() > RESPONSE_MAX {
            return Err(failure("account_reply_too_large"));
        }
        parse(&reply)
    })
    .await
    .map_err(|_| failure("account_url_unreachable"))?
}

fn parse(reply: &[u8]) -> Result<Response, EngineError> {
    let invalid = || failure("account_reply_invalid");
    let split = reply
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(invalid)?;
    let head = std::str::from_utf8(&reply[..split]).map_err(|_| invalid())?;
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|line| line.strip_prefix("HTTP/1."))
        .and_then(|line| line.get(2..5))
        .and_then(|code| code.parse().ok())
        .ok_or_else(invalid)?;
    let chunked = lines.any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.trim().eq_ignore_ascii_case("transfer-encoding")
                && value.trim().eq_ignore_ascii_case("chunked")
        })
    });
    let body = &reply[split + 4..];
    let body = if chunked {
        dechunk(body).ok_or_else(invalid)?
    } else {
        body.to_vec()
    };
    Ok(Response { status, body })
}

fn dechunk(mut body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let end = body.windows(2).position(|window| window == b"\r\n")?;
        let size = std::str::from_utf8(&body[..end])
            .ok()?
            .split(';')
            .next()?
            .trim();
        let size = usize::from_str_radix(size, 16).ok()?;
        body = &body[end + 2..];
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(body.get(..size)?);
        body = body.get(size + 2..)?;
    }
}

fn model_names(values: impl Iterator<Item = String>) -> Vec<String> {
    let mut names: Vec<String> = values
        .filter(|name| {
            !name.is_empty()
                && name.len() <= crate::accounts::ACCOUNT_MODEL_MAX
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:/@+".contains(&byte))
        })
        .take(MODELS_MAX)
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Reads an OpenAI-compatible `/models` list. Input: a reply body. Output: valid model ids.
pub(crate) fn openai_models(body: &[u8]) -> Option<Vec<String>> {
    #[derive(serde::Deserialize)]
    struct Item {
        id: String,
    }
    #[derive(serde::Deserialize)]
    struct List {
        data: Vec<Item>,
    }
    let list: List = serde_json::from_slice(body).ok()?;
    Some(model_names(list.data.into_iter().map(|item| item.id)))
}

/// Lists the models a local Ollama-style server has pulled. It asks `/api/tags` at the
/// server root first, then the OpenAI-compatible `/models` under the given path.
/// Input: a loopback http URL. Output: sorted model names, or an error when unreachable.
pub(crate) async fn local_models(
    url: &url::Url,
    timeout: Duration,
) -> Result<Vec<String>, EngineError> {
    #[derive(serde::Deserialize)]
    struct Item {
        name: String,
    }
    #[derive(serde::Deserialize)]
    struct Tags {
        models: Vec<Item>,
    }
    let deadline = tokio::time::Instant::now() + timeout;
    let tags = get(url, "/api/tags", None, timeout).await?;
    if tags.status == 200
        && let Ok(tags) = serde_json::from_slice::<Tags>(&tags.body)
    {
        return Ok(model_names(tags.models.into_iter().map(|item| item.name)));
    }
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    let reply = get(url, &joined(url, "/models"), None, remaining).await?;
    if reply.status != 200 {
        return Err(failure("account_models_unavailable"));
    }
    openai_models(&reply.body).ok_or_else(|| failure("account_models_unavailable"))
}

/// Asks an OpenAI-compatible server for its models with the saved key, to check that the
/// address answers and accepts the key. Inputs: http URL, optional key, timeout.
/// Output: the HTTP status and any model ids, or an error when the address is unreachable.
pub(crate) async fn check(
    url: &url::Url,
    key: Option<&[u8]>,
    timeout: Duration,
) -> Result<(u16, Vec<String>), EngineError> {
    let reply = get(url, &joined(url, "/models"), key, timeout).await?;
    let models = if reply.status == 200 {
        openai_models(&reply.body).unwrap_or_default()
    } else {
        Vec::new()
    };
    Ok((reply.status, models))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_parse_plain_and_chunked_bodies() {
        let plain = parse(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").unwrap();
        assert_eq!(
            (plain.status, plain.body.as_slice()),
            (200, b"ok".as_slice())
        );
        let chunked = parse(b"HTTP/1.1 401 Unauthorized\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nab\r\n1\r\nc\r\n0\r\n\r\n").unwrap();
        assert_eq!(
            (chunked.status, chunked.body.as_slice()),
            (401, b"abc".as_slice())
        );
        assert!(parse(b"garbage").is_err());
        assert_eq!(
            openai_models(br#"{"data":[{"id":"b"},{"id":"a"},{"id":"bad name"}]}"#).unwrap(),
            ["a", "b"]
        );
    }
}
