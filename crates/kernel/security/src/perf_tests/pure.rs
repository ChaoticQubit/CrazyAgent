use crate::error::StoreError;
use crate::exposure::{holds, redact};
use crate::net::address::{address_allowed, pinned};
use crate::net::capture::{field_fits, host_key};
use crate::net::{Query, target};
use crate::perf_kit::{Row, SAMPLES, time};
use crate::read_limited::read_limited;
use crate::relay_token::RelayToken;
use crate::secret::{Piece, SecretString, sized, wipe, wipe_owned};
use crate::store::{SecretRef, parts};
use std::hint::black_box;
use std::net::IpAddr;

const FAST: usize = 1000;
const PARSE: usize = 100;
const SECRET: &[u8] = b"ghp_owner-paste-0123456789abcdef0123";
const OTHER: &[u8] = b"sk-other-secret-value-0123456789";
const THIRD: &[u8] = b"third-secret-9876543210";
const ENV_HOME: &[u8] = b"/Users/perf/home";
const ENV_VALUE: &[u8] = b"value-of-env-12345";
const REFERENCE: &str = "dev.crazyagent.plugins:plugin-key-openai";
const URL: &str = "https://api.github.com/v1/user?page=2";
const ADDRESS: &str = "140.82.112.4";
const OUTPUT_BYTES: usize = 4_096;
const READ_LIMIT: usize = 8_192;

fn row(name: &'static str, batch: usize) -> Row {
    Row {
        layer: 1,
        module: "security",
        name,
        batch,
    }
}

/// An installer output of `OUTPUT_BYTES` printable bytes holding each secret once and one
/// environment value. Output: the bytes.
fn output() -> Vec<u8> {
    let mut bytes: Vec<u8> = (0..OUTPUT_BYTES).map(|at| b'a' + (at % 26) as u8).collect();
    for (at, value) in [
        (100, SECRET),
        (1_500, OTHER),
        (3_000, THIRD),
        (3_600, ENV_VALUE),
    ] {
        bytes[at..at + value.len()].copy_from_slice(value);
    }
    bytes
}

/// Times the calls of the security crate that read no store, keychain or network: the
/// error code, the exposure check and redaction, the bounded read, the relay token, the
/// secret wrapper, the reference parts and the net address and capture rules, on fixed
/// inputs. Prints one PERF line per call and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn pure_calls() {
    let output = output();
    let text: Vec<u8> = output[200..456].to_vec();
    let token = RelayToken::mint().unwrap();
    let presented = token.as_str().to_owned();
    let secret = SecretString::new(SECRET.to_vec()).unwrap();
    let found = SecretRef::try_from(REFERENCE).unwrap();
    let parsed = url::Url::parse(URL).unwrap();
    let address: IpAddr = ADDRESS.parse().unwrap();
    let refused = StoreError::code("credential_missing");
    assert!(holds(SECRET, &output) && !holds(SECRET, &text));
    assert!(!holds(
        SECRET,
        &redact(&output, &[SECRET, OTHER, THIRD], &[ENV_HOME, ENV_VALUE])
    ));
    assert!(token.matches(&presented) && address_allowed(address));
    assert_eq!(
        parts(&found).unwrap(),
        ("dev.crazyagent.plugins", "plugin-key-openai")
    );
    assert!(target(URL, Query::Allowed).is_ok() && field_fits("access_token"));
    time(&row("error.StoreError.code", FAST), SAMPLES, || {
        black_box(StoreError::code(black_box("credential_missing")));
    });
    time(&row("error.StoreError.fmt", FAST), SAMPLES, || {
        black_box(black_box(&refused).to_string());
    });
    time(&row("exposure.holds", FAST), SAMPLES, || {
        black_box(holds(black_box(SECRET), black_box(&text)));
    });
    time(&row("exposure.redact", PARSE), SAMPLES, || {
        black_box(redact(
            black_box(&output),
            &[SECRET, OTHER, THIRD],
            &[ENV_HOME, ENV_VALUE],
        ));
    });
    time(&row("read_limited.read_limited", PARSE), SAMPLES, || {
        black_box(read_limited(black_box(&output[..]), READ_LIMIT).1);
    });
    time(&row("relay_token.RelayToken.mint", PARSE), SAMPLES, || {
        black_box(RelayToken::mint().unwrap());
    });
    time(&row("relay_token.RelayToken.as_str", FAST), SAMPLES, || {
        black_box(black_box(&token).as_str());
    });
    time(
        &row("relay_token.RelayToken.matches", FAST),
        SAMPLES,
        || {
            black_box(black_box(&token).matches(black_box(&presented)));
        },
    );
    time(&row("secret.SecretString.new", FAST), SAMPLES, || {
        black_box(SecretString::new(black_box(SECRET).to_vec()).unwrap());
    });
    time(&row("secret.SecretString.bearer", FAST), SAMPLES, || {
        black_box(SecretString::bearer(black_box(SECRET).to_vec()).unwrap());
    });
    time(&row("secret.SecretString.stored", FAST), SAMPLES, || {
        black_box(SecretString::stored(black_box(SECRET).to_vec()).unwrap());
    });
    time(&row("secret.SecretString.expose", FAST), SAMPLES, || {
        black_box(black_box(&secret).expose());
    });
    let mut buffer = [7_u8; 64];
    time(&row("secret.wipe", FAST), SAMPLES, || {
        wipe(black_box(&mut buffer[..]));
    });
    time(&row("secret.wipe_owned", FAST), SAMPLES, || {
        let mut owned = black_box(SECRET).to_vec();
        wipe_owned(&mut owned);
        black_box(owned);
    });
    time(&row("secret.sized", FAST), SAMPLES, || {
        black_box(sized(&[
            Piece::Plain(black_box("header = \"Authorization: Bearer ")),
            Piece::Escaped(black_box("ghp_owner\"paste\\0123")),
            Piece::Form(black_box("a value&more=1")),
            Piece::Hex(black_box(SECRET)),
        ]));
    });
    #[cfg(unix)]
    {
        let value = serde_json::json!({
            "token": "ghp_owner-paste-0123456789abcdef0123",
            "hosts": ["api.github.com", "github.com"],
            "expires": 1_761_400_000,
        });
        time(&row("secret.sized_json", PARSE), SAMPLES, || {
            black_box(crate::secret::sized_json(black_box(&value)).unwrap());
        });
    }
    time(&row("store.SecretRef.try_from", FAST), SAMPLES, || {
        black_box(SecretRef::try_from(black_box(REFERENCE)).unwrap());
    });
    time(&row("store.SecretRef.as_str", FAST), SAMPLES, || {
        black_box(black_box(&found).as_str());
    });
    time(&row("store.parts", FAST), SAMPLES, || {
        black_box(parts(black_box(&found)).unwrap());
    });
    time(&row("net.target", PARSE), SAMPLES, || {
        black_box(target(black_box(URL), Query::Allowed).unwrap());
    });
    time(&row("net.address.address_allowed", FAST), SAMPLES, || {
        black_box(address_allowed(black_box(address)));
    });
    time(&row("net.address.pinned", PARSE), SAMPLES, || {
        black_box(
            pinned(
                black_box(&parsed),
                443,
                |_, _| Ok::<_, StoreError>(vec![address]),
                || StoreError::code("net_address_refused"),
            )
            .unwrap(),
        );
    });
    time(&row("net.capture.field_fits", FAST), SAMPLES, || {
        black_box(field_fits(black_box("access_token")));
    });
    time(&row("net.capture.host_key", FAST), SAMPLES, || {
        black_box(host_key(black_box(&parsed)));
    });
}
