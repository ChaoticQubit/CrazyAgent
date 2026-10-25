use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// One request the loopback server received, with lowercase header names.
#[derive(Clone, Debug)]
pub(crate) struct Seen {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) query: String,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: Vec<u8>,
    pub(crate) at: Instant,
}

impl Seen {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    pub(crate) fn form(&self) -> BTreeMap<String, String> {
        url::form_urlencoded::parse(&self.body)
            .into_owned()
            .collect()
    }

    pub(crate) fn query_pairs(&self) -> BTreeMap<String, String> {
        url::form_urlencoded::parse(self.query.as_bytes())
            .into_owned()
            .collect()
    }
}

/// A status, extra headers and a body to send back.
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl Answer {
    pub(crate) fn json(status: u16, body: serde_json::Value) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: body.to_string().into_bytes(),
        }
    }

    pub(crate) fn bytes(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".into(), "application/octet-stream".into())],
            body,
        }
    }
}

type Handler = dyn Fn(&Seen, usize) -> Answer + Send + Sync;

/// A real HTTP/1.1 server on 127.0.0.1 that answers each request with the handler (given
/// the request and how many came before it) and records every request it saw.
pub(crate) struct Server {
    pub(crate) port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    pub(crate) fn start(handler: impl Fn(&Seen, usize) -> Answer + Send + Sync + 'static) -> Self {
        let listener = crate::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(handler);
        let recorded = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in crate::spawn_gate::incoming_tcp_std(&listener) {
                let Ok(stream) = stream else { return };
                let handler = Arc::clone(&handler);
                let recorded = Arc::clone(&recorded);
                std::thread::spawn(move || serve(stream, &*handler, &recorded));
            }
        });
        Self { port, seen }
    }

    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

fn serve(stream: TcpStream, handler: &Handler, recorded: &Mutex<Vec<Seen>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut words = line.split_whitespace();
    let method = words.next().unwrap_or_default().to_owned();
    let target = words.next().unwrap_or_default().to_owned();
    let (path, query) = target
        .split_once('?')
        .map(|(path, query)| (path.to_owned(), query.to_owned()))
        .unwrap_or((target.clone(), String::new()));
    let mut headers = BTreeMap::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':').unwrap();
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    let length = headers
        .get("content-length")
        .map(|value| value.parse::<usize>().unwrap())
        .unwrap_or(0);
    assert!(length <= 1 << 20);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let request = Seen {
        method,
        path,
        query,
        headers,
        body,
        at: Instant::now(),
    };
    let index = {
        let mut seen = recorded.lock().unwrap();
        seen.push(request.clone());
        seen.len() - 1
    };
    let answer = handler(&request, index);
    let mut response = format!(
        "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
        answer.status,
        answer.body.len()
    );
    for (name, value) in &answer.headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str("\r\n");
    let mut stream = stream;
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(&answer.body);
}

/// Encodes bytes as standard Base64 with padding, the form of an HTTP Basic credential.
/// Input: the bytes. Output: the Base64 text.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let joined = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |value, (index, byte)| {
                value | (u32::from(*byte) << (16 - 8 * index))
            });
        for index in 0..4 {
            if index <= chunk.len() {
                text.push(TABLE[((joined >> (18 - 6 * index)) & 63) as usize] as char);
            } else {
                text.push('=');
            }
        }
    }
    text
}
