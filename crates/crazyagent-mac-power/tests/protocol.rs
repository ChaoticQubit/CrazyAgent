#![cfg(target_os = "macos")]

use crazyagent_mac_power::protocol::{
    LidDisplay, REQUEST_BYTES_MAX, Refusal, Reply, Request, encode_request, read_request,
    write_reply,
};
use std::io::Cursor;

fn read(bytes: &[u8]) -> Result<Request, Refusal> {
    read_request(&mut Cursor::new(bytes.to_vec()))
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
        text.push(b'}');
        let held = Request::Hold {
            lease_s: 120,
            lid_display,
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
    let hold = br#"{"version":1,"call":"hold","lease_s":120,"lid_display":"leave_on"}"#;
    let mut padded = hold.to_vec();
    padded.resize(REQUEST_BYTES_MAX - 1, b' ');
    padded.push(b'\n');
    assert_eq!(padded.len(), REQUEST_BYTES_MAX);
    let held = Request::Hold {
        lease_s: 120,
        lid_display: LidDisplay::LeaveOn,
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
    let short = br#"{"version":1,"call":"hold","lease_s":29,"lid_display":"sleep"}"#;
    assert_eq!(read(&lined(short)), Err(Refusal::LeaseOutOfRange));
    let long = br#"{"version":1,"call":"hold","lease_s":301,"lid_display":"sleep"}"#;
    assert_eq!(read(&lined(long)), Err(Refusal::LeaseOutOfRange));
    let missing = br#"{"version":1,"call":"hold","lid_display":"sleep"}"#;
    assert_eq!(read(&lined(missing)), Err(Refusal::LeaseOutOfRange));
    let on_release = b"{\"version\":1,\"call\":\"release\",\"lease_s\":30}\n";
    assert_eq!(read(on_release), Err(Refusal::Malformed));
    let edges = [
        (
            30,
            lined(br#"{"version":1,"call":"hold","lease_s":30,"lid_display":"sleep"}"#),
        ),
        (
            300,
            lined(br#"{"version":1,"call":"hold","lease_s":300,"lid_display":"sleep"}"#),
        ),
    ];
    for (lease_s, bytes) in edges {
        let held = Request::Hold {
            lease_s,
            lid_display: LidDisplay::Sleep,
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
        },
        Request::Hold {
            lease_s: 120,
            lid_display: LidDisplay::LeaveOn,
        },
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
