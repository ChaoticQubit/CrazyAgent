use super::*;
use crate::coord::{BODY_BYTES_MAX, MessageId, MessageKind};

fn mailbox() -> (tempfile::TempDir, Mailbox) {
    let root = tempfile::Builder::new()
        .prefix("ca-hook-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let folder = Mailbox::open(root.path()).unwrap();
    (root, folder)
}

fn post(folder: &Mailbox, id: MessageId, kind: MessageKind, body: &str) -> String {
    let letter = Letter {
        id,
        to: LEAD_TAG.to_owned(),
        from: "Ada".to_owned(),
        kind,
        question: None,
        work: None,
        reply_to: None,
        body: body.to_owned(),
    };
    let stem = relay::letter_stem(id).unwrap();
    relay::put(
        folder,
        &format!("{stem}{INBOX}"),
        &serde_json::to_vec(&letter).unwrap(),
    )
    .unwrap();
    stem
}

#[test]
fn delivered_letters_say_which_take_a_reply_and_which_take_a_note() {
    let (root, folder) = mailbox();
    let note = post(&folder, 1, MessageKind::Note, "Overlap on calc.py.");
    let prompt = post(&folder, 2, MessageKind::Prompt, "Write the docs.");
    let text = deliver(&folder, LEAD_TAG).unwrap();
    assert_eq!(
        text,
        "CrazyAgent team messages for you (2). Answer a question or work for you with update_work kind reply and its id in reply_to. A note takes no reply; to answer one, send kind note with to.\n\n[crazyagent msg 1] from Ada (note, it takes no reply):\nOverlap on calc.py.\n\n[crazyagent msg 2] from Ada (work for you):\nWrite the docs."
    );
    for stem in [&note, &prompt] {
        assert!(root.path().join(format!("{stem}{SHOWN}")).is_file());
        assert!(!root.path().join(format!("{stem}{INBOX}")).exists());
    }
    assert_eq!(deliver(&folder, LEAD_TAG), None);
}

#[test]
fn a_full_inbox_is_handed_over_within_the_context_budget() {
    let (root, folder) = mailbox();
    let body = "x".repeat(BODY_BYTES_MAX);
    for id in 1..=INBOX_MAX as MessageId {
        post(&folder, id, MessageKind::Note, &body);
    }
    let text = deliver(&folder, LEAD_TAG).unwrap();
    assert!(text.chars().count() <= CONTEXT_CHARS_MAX);
    assert!(
        text.starts_with("CrazyAgent team messages for you (2). "),
        "{}",
        &text[..120]
    );
    let waiting = std::fs::read_dir(root.path())
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_str()
                .unwrap()
                .ends_with(INBOX)
        })
        .count();
    assert_eq!(waiting, INBOX_MAX - 2);
}
