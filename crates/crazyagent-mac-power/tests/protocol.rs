#![cfg(target_os = "macos")]

use crazyagent_mac_power::protocol::{
    LidDisplay, REQUEST_BYTES_MAX, Refusal, Reply, Request, encode_request, read_request,
    write_reply,
};
use std::io::Cursor;

fn read(bytes: &[u8]) -> Result<Request, Refusal> {
    read_request(&mut Cursor::new(bytes.to_vec()))
}

/// A hold's text with its lease and display ask and `low_power` false, no newline.
fn hold_text(lease_s: u16, lid_display: &str) -> Vec<u8> {
    let text = format!(
        "{{\"version\":1,\"call\":\"hold\",\"lease_s\":{lease_s},\
         \"lid_display\":\"{lid_display}\",\"low_power\":false}}"
    );
    text.into_bytes()
}

/// One request line: the text and its newline.
fn lined(text: &[u8]) -> Vec<u8> {
    let mut line = text.to_vec();
    line.push(b'\n');
    line
}

#[test]
fn a_hold_names_its_lid_display_and_no_other_call_does() {
    let unnamed = br#"{"version":1,"call":"hold","lease_s":120}"#;
    assert_eq!(read(&lined(unnamed)), Err(Refusal::Malformed), "Hold names it (Task 12)");
    let null = br#"{"version":1,"call":"hold","lease_s":120,"lid_display":null}"#;
    assert_eq!(read(&lined(null)), Err(Refusal::Malformed));
    let unknown = br#"{"version":1,"call":"hold","lease_s":120,"lid_display":"dim"}"#;
    assert_eq!(read(&lined(unknown)), Err(Refusal::Malformed));
    let asks = [
        (&br#""sleep""#[..], LidDisplay::Sleep),
        (&br#""leave_on""#[..], LidDisplay::LeaveOn),
    ];
    for (word, lid_display) in asks {
        let mut text = br#"{"version":1,"call":"hold","lease_s":120,"lid_display":"#.to_vec();
        text.extend_from_slice(word);
        text.extend_from_slice(br#","low_power":false}"#);
        let held = Request::Hold {
            lease_s: 120,
            lid_display,
            low_power: false,
        };
        assert_eq!(read(&lined(&text)), Ok(held));
    }
    for call in ["release", "read", "forget"] {
        let text = format!(r#"{{"version":1,"call":"{call}","lid_display":"sleep"}}"#);
        assert_eq!(read(&lined(text.as_bytes())), Err(Refusal::Malformed), "{call}");
    }
}

#[test]
fn a_request_over_one_kibibyte_or_with_an_unknown_field_is_refused() {
    let hold = hold_text(120, "leave_on");
    let mut padded = hold.to_vec();
    padded.resize(REQUEST_BYTES_MAX - 1, b' ');
    padded.push(b'\n');
    assert_eq!(padded.len(), REQUEST_BYTES_MAX);
    let held = Request::Hold {
        lease_s: 120,
        lid_display: LidDisplay::LeaveOn,
        low_power: false,
    };
    assert_eq!(read(&padded), Ok(held));

    let mut over = hold.to_vec();
    over.resize(REQUEST_BYTES_MAX, b' ');
    over.push(b'\n');
    assert_eq!(read(&over), Err(Refusal::TooLong));

    let unknown = b"{\"version\":1,\"call\":\"read\",\"path\":\"/etc\"}\n";
    assert_eq!(read(unknown), Err(Refusal::Malformed));
    let no_newline = br#"{"version":1,"call":"read"}"#;
    assert_eq!(read(no_newline), Err(Refusal::Malformed));
    let unknown_call = b"{\"version\":1,\"call\":\"sleep_now\"}\n";
    assert_eq!(read(unknown_call), Err(Refusal::Malformed));
}

#[test]
fn a_request_with_another_version_is_refused() {
    assert_eq!(
        read(b"{\"version\":2,\"call\":\"read\"}\n"),
        Err(Refusal::Version)
    );
    assert_eq!(
        read(b"{\"version\":0,\"call\":\"read\"}\n"),
        Err(Refusal::Version)
    );
    assert_eq!(read(b"{\"call\":\"read\"}\n"), Err(Refusal::Malformed));
    assert_eq!(
        read(b"{\"version\":1,\"call\":\"read\"}\n"),
        Ok(Request::Read)
    );
}

#[test]
fn a_hold_lease_outside_thirty_to_three_hundred_seconds_is_refused() {
    let short = hold_text(29, "sleep");
    assert_eq!(read(&lined(&short)), Err(Refusal::LeaseOutOfRange));
    let long = hold_text(301, "sleep");
    assert_eq!(read(&lined(&long)), Err(Refusal::LeaseOutOfRange));
    let missing = br#"{"version":1,"call":"hold","lid_display":"sleep"}"#;
    assert_eq!(read(&lined(missing)), Err(Refusal::LeaseOutOfRange));
    let on_release = b"{\"version\":1,\"call\":\"release\",\"lease_s\":30}\n";
    assert_eq!(read(on_release), Err(Refusal::Malformed));
    let edges = [
        (
            30,
            lined(&hold_text(30, "sleep")),
        ),
        (
            300,
            lined(&hold_text(300, "sleep")),
        ),
    ];
    for (lease_s, bytes) in edges {
        let held = Request::Hold {
            lease_s,
            lid_display: LidDisplay::Sleep,
            low_power: false,
        };
        assert_eq!(read(&bytes), Ok(held));
    }
}

#[test]
fn every_request_the_client_encodes_reads_back_as_itself() {
    let requests = [
        Request::Hold {
            lease_s: 120,
            lid_display: LidDisplay::Sleep,
            low_power: false,
        },
        Request::Hold {
            lease_s: 120,
            lid_display: LidDisplay::LeaveOn,
            low_power: false,
        },
        Request::Hold {
            lease_s: 300,
            lid_display: LidDisplay::Sleep,
            low_power: true,
        },
        Request::LowPower { lease_s: 30 },
        Request::Release,
        Request::Read,
        Request::Forget,
    ];
    for request in requests {
        let bytes = encode_request(&request);
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert!(bytes.len() <= REQUEST_BYTES_MAX);
        assert_eq!(read(&bytes), Ok(request));
    }
}

#[test]
fn a_reply_is_one_json_line_with_its_kind() {
    let mut written = Vec::new();
    write_reply(
        &mut written,
        &Reply::Refused {
            reason: Refusal::HoldersFull,
        },
    )
    .unwrap();
    assert_eq!(
        written,
        b"{\"reply\":\"refused\",\"reason\":\"holders_full\"}\n".to_vec()
    );
    let mut held = Vec::new();
    write_reply(&mut held, &Reply::Held { lease_left_s: 120 }).unwrap();
    assert_eq!(
        held,
        b"{\"reply\":\"held\",\"lease_left_s\":120}\n".to_vec()
    );
}

#[test]
fn low_power_rides_only_hold_and_lease_s_only_hold_and_lowpower() {
    let asked =
        br#"{"version":1,"call":"hold","lease_s":120,"lid_display":"sleep","low_power":true}"#;
    let held = Request::Hold {
        lease_s: 120,
        lid_display: LidDisplay::Sleep,
        low_power: true,
    };
    assert_eq!(read(&lined(asked)), Ok(held));
    let unsaid = br#"{"version":1,"call":"hold","lease_s":120,"lid_display":"sleep"}"#;
    assert_eq!(
        read(&lined(unsaid)),
        Err(Refusal::Malformed),
        "low_power is required on Hold, so builds that differ fail closed"
    );
    let power_only = br#"{"version":1,"call":"low_power","lease_s":120}"#;
    assert_eq!(
        read(&lined(power_only)),
        Ok(Request::LowPower { lease_s: 120 })
    );
    let malformed: [&[u8]; 5] = [
        br#"{"version":1,"call":"low_power","lease_s":120,"lid_display":"sleep"}"#,
        br#"{"version":1,"call":"low_power","lease_s":120,"low_power":true}"#,
        br#"{"version":1,"call":"read","low_power":true}"#,
        br#"{"version":1,"call":"release","low_power":false}"#,
        br#"{"version":1,"call":"forget","low_power":false}"#,
    ];
    for text in malformed {
        let shown = String::from_utf8_lossy(text);
        assert_eq!(read(&lined(text)), Err(Refusal::Malformed), "{shown}");
    }
    let outside: [&[u8]; 3] = [
        br#"{"version":1,"call":"low_power","lease_s":29}"#,
        br#"{"version":1,"call":"low_power","lease_s":301}"#,
        br#"{"version":1,"call":"low_power"}"#,
    ];
    for text in outside {
        let shown = String::from_utf8_lossy(text);
        assert_eq!(read(&lined(text)), Err(Refusal::LeaseOutOfRange), "{shown}");
    }
}
