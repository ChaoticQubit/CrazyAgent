use super::transcript::Transcript;
use super::types::ClaudeAuthChallenge;
use super::verified_status;
use std::path::Path;

#[test]
fn claude_login_transcript_requires_an_actual_complete_official_url() {
    let mut transcript = Transcript::default();
    assert!(
        transcript
            .push(b"If the browser didn't open, visit: https://claude.com/cai/")
            .unwrap()
            .is_none()
    );
    let challenge = transcript
        .push(b"oauth/authorize?state=synthetic-parser-data\n")
        .unwrap()
        .unwrap();
    assert_eq!(
        challenge.url(),
        "https://claude.com/cai/oauth/authorize?state=synthetic-parser-data"
    );
    assert!(!challenge.accepts_code());
    let challenge = transcript
        .push(b"Paste code here if prompted > ")
        .unwrap()
        .unwrap();
    assert!(challenge.accepts_code());
    assert!(transcript.push(b"Login successful.\n").unwrap().is_none());
}

#[test]
fn claude_login_transcript_rejects_console_and_untrusted_destinations() {
    for url in [
        "https://platform.claude.com/oauth/authorize?state=synthetic",
        "https://claude.com.attacker.invalid/cai/oauth/authorize?state=synthetic",
        "http://claude.com/cai/oauth/authorize?state=synthetic",
        "https://claude.com/cai/oauth/authorize?state=synthetic#fragment",
    ] {
        assert!(ClaudeAuthChallenge::new(url.into(), false).is_err());
    }
}

#[test]
fn claude_login_transcript_is_bounded_and_strips_only_terminal_presentation() {
    let mut transcript = Transcript::default();
    let text = b"\x1b[32mIf the browser didn't open, visit: \x1b]8;;https://claude.com/cai/oauth/authorize?state=synthetic\x1b\\https://claude.com/cai/oauth/authorize?state=synthetic\x1b]8;;\x1b\\\x1b[0m\n";
    let mut found = None;
    for byte in text {
        if let Some(challenge) = transcript.push(&[*byte]).unwrap() {
            found = Some(challenge);
        }
    }
    assert_eq!(
        found.unwrap().url(),
        "https://claude.com/cai/oauth/authorize?state=synthetic"
    );
    assert!(transcript.push(&vec![b'x'; 65537]).is_err());
}

#[test]
fn claude_status_exit_must_agree_with_real_decoder_fields() {
    let home = Path::new("/private/tmp/claude-status-synthetic-input");
    let positive = br#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","configDirectory":"/private/tmp/claude-status-synthetic-input"}"#;
    let negative = br#"{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty","configDirectory":"/private/tmp/claude-status-synthetic-input"}"#;
    assert!(verified_status(positive, home, Some(0)).unwrap().is_some());
    assert!(verified_status(negative, home, Some(1)).unwrap().is_none());
    assert!(verified_status(positive, home, Some(1)).is_err());
    assert!(verified_status(negative, home, Some(0)).is_err());
    assert!(verified_status(positive, home, None).is_err());
}

#[test]
fn claude_fallback_input_is_move_only_bounded_and_requires_full_code_and_state() {
    use super::types::completion_bytes;
    use crate::engine::owner::AccountSecret;
    for value in [
        "code-only",
        "#state",
        "code#",
        "code#state#extra",
        "code with space#state",
    ] {
        assert!(completion_bytes(AccountSecret::new(value.as_bytes().to_vec()).unwrap()).is_err());
    }
    let mut bytes =
        completion_bytes(AccountSecret::new(b"synthetic-code#synthetic-state".to_vec()).unwrap())
            .unwrap();
    assert_eq!(bytes.as_slice(), b"synthetic-code#synthetic-state\n");
    bytes.fill(0);
    assert!(bytes.iter().all(|byte| *byte == 0));
}
