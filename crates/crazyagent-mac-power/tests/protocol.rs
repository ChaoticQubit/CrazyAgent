#![cfg(target_os = "macos")]

use crazyagent_mac_power::protocol::{
    REQUEST_BYTES_MAX, Refusal, Reply, Request, encode_request, read_request, write_reply,
};
use std::io::Cursor;

fn read(bytes: &[u8]) -> Result<Request, Refusal> {
    read_request(&mut Cursor::new(bytes.to_vec()))
}

#[test]
fn a_request_over_one_kibibyte_or_with_an_unknown_field_is_refused() {
    let hold = br#"{"version":1,"call":"hold","lease_s":120}"#;
    let mut padded = hold.to_vec();
    padded.resize(REQUEST_BYTES_MAX - 1, b' ');
    padded.push(b'\n');
    assert_eq!(padded.len(), REQUEST_BYTES_MAX);
    assert_eq!(read(&padded), Ok(Request::Hold { lease_s: 120 }));

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
    let short = b"{\"version\":1,\"call\":\"hold\",\"lease_s\":29}\n";
    assert_eq!(read(short), Err(Refusal::LeaseOutOfRange));
    let long = b"{\"version\":1,\"call\":\"hold\",\"lease_s\":301}\n";
    assert_eq!(read(long), Err(Refusal::LeaseOutOfRange));
    let missing = b"{\"version\":1,\"call\":\"hold\"}\n";
    assert_eq!(read(missing), Err(Refusal::LeaseOutOfRange));
    let on_release = b"{\"version\":1,\"call\":\"release\",\"lease_s\":30}\n";
    assert_eq!(read(on_release), Err(Refusal::Malformed));
    let edges = [
        (
            30,
            b"{\"version\":1,\"call\":\"hold\",\"lease_s\":30}\n".to_vec(),
        ),
        (
            300,
            b"{\"version\":1,\"call\":\"hold\",\"lease_s\":300}\n".to_vec(),
        ),
    ];
    for (lease_s, bytes) in edges {
        assert_eq!(read(&bytes), Ok(Request::Hold { lease_s }));
    }
}

#[test]
fn every_request_the_client_encodes_reads_back_as_itself() {
    let requests = [
        Request::Hold { lease_s: 120 },
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
