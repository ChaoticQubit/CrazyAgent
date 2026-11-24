use crate::credentials::{
    Piece, SecretString, StoreError, checked_name, parts, reference, sized, sized_json, wipe,
};
use crate::model_gateway::relay::{TOKEN_HEX, TOKEN_PREFIX, random_hex};
use crate::perf_kit::{Row, SAMPLES, time};
use crate::protocol::CredentialRef;
use std::hint::black_box;
use subtle::ConstantTimeEq;

const FAST: usize = 1000;
const PARSE: usize = 100;
const SECRET: &[u8] = b"ghp_owner-paste-0123456789abcdef0123";
const OTHER: &[u8] = b"sk-other-secret-value-0123456789";
const THIRD: &[u8] = b"third-secret-9876543210";
const ENV_VALUE: &[u8] = b"value-of-env-12345";
const REFERENCE: &str = "dev.crazyagent.plugins:plugin-key-openai";
const SERVICE: &str = "dev.crazyagent.plugins";
const ACCOUNT: &str = "plugin-key-openai";
const OUTPUT_BYTES: usize = 4_096;

fn row(name: &'static str, batch: usize) -> Row {
    Row {
        layer: 1,
        module: "security",
        name,
        batch,
    }
}

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

/// The 2.4.4 exposure check: the closure `LaunchContext` ran over each secret of an MCP
/// server's command. Inputs: the value and the text. Output: true when it occurs.
fn holds(value: &[u8], text: &[u8]) -> bool {
    !value.is_empty() && text.windows(value.len()).any(|window| window == value)
}

/// Times the 2.4.4 path of every 2.5 pure security row that had one, under the 2.5 row
/// names and inputs: the credentials module, the relay token mint and its constant-time
/// compare, the credential reference and the launch exposure check. Prints one PERF line
/// per call and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn security_pure_calls() {
    let output = output();
    let text: Vec<u8> = output[..256].to_vec();
    let token = format!("{TOKEN_PREFIX}{}", random_hex(TOKEN_HEX / 2).unwrap());
    let presented = token.clone();
    let matches = |value: &str| bool::from(value.as_bytes().ct_eq(token.as_bytes()));
    let secret = SecretString::new(SECRET.to_vec()).unwrap();
    let found = CredentialRef::try_from(REFERENCE.to_owned()).unwrap();
    let refused = StoreError::code("credential_missing");
    assert!(holds(SECRET, &output) && !holds(SECRET, &text) && matches(&presented));
    assert_eq!(parts(&found).unwrap(), (SERVICE, ACCOUNT));
    assert_eq!(checked_name(SERVICE).ok(), Some(SERVICE));
    time(&row("store.checked_name", FAST), SAMPLES, || {
        let _ = black_box(checked_name(black_box(SERVICE)));
    });
    time(&row("store.reference", FAST), SAMPLES, || {
        let _ = black_box(reference(black_box(SERVICE), black_box(ACCOUNT)));
    });
    time(&row("error.StoreError.code", FAST), SAMPLES, || {
        black_box(StoreError::code(black_box("credential_missing")));
    });
    time(&row("error.StoreError.fmt", FAST), SAMPLES, || {
        black_box(black_box(&refused).to_string());
    });
    time(&row("exposure.holds", FAST), SAMPLES, || {
        black_box(holds(black_box(SECRET), black_box(&text)));
    });
    time(&row("relay_token.RelayToken.mint", PARSE), SAMPLES, || {
        black_box(format!(
            "{TOKEN_PREFIX}{}",
            random_hex(TOKEN_HEX / 2).unwrap()
        ));
    });
    time(&row("relay_token.RelayToken.as_str", FAST), SAMPLES, || {
        black_box(black_box(&token).as_str());
    });
    time(
        &row("relay_token.RelayToken.matches", FAST),
        SAMPLES,
        || {
            black_box(matches(black_box(&presented)));
        },
    );
    time(&row("secret.SecretString.new", FAST), SAMPLES, || {
        black_box(SecretString::new(black_box(SECRET).to_vec()).unwrap());
    });
    time(&row("secret.SecretString.bearer", FAST), SAMPLES, || {
        black_box(SecretString::bearer(black_box(SECRET).to_vec()).unwrap());
    });
    time(&row("secret.SecretString.expose", FAST), SAMPLES, || {
        black_box(black_box(&secret).expose());
    });
    let mut buffer = [7_u8; 64];
    time(&row("secret.wipe", FAST), SAMPLES, || {
        wipe(black_box(&mut buffer[..]));
    });
    time(&row("secret.sized", FAST), SAMPLES, || {
        black_box(sized(&[
            Piece::Plain(black_box("header = \"Authorization: Bearer ")),
            Piece::Escaped(black_box("ghp_owner\"paste\\0123")),
            Piece::Form(black_box("a value&more=1")),
            Piece::Hex(black_box(SECRET)),
        ]));
    });
    let value = serde_json::json!({
        "token": "ghp_owner-paste-0123456789abcdef0123",
        "hosts": ["api.github.com", "github.com"],
        "expires": 1_761_400_000,
    });
    time(&row("secret.sized_json", PARSE), SAMPLES, || {
        black_box(sized_json(black_box(&value)).unwrap());
    });
    time(&row("store.SecretRef.try_from", FAST), SAMPLES, || {
        black_box(CredentialRef::try_from(black_box(REFERENCE).to_owned()).unwrap());
    });
    time(&row("store.SecretRef.as_str", FAST), SAMPLES, || {
        black_box(black_box(&found).as_str());
    });
    time(&row("store.parts", FAST), SAMPLES, || {
        black_box(parts(black_box(&found)).unwrap());
    });
}
