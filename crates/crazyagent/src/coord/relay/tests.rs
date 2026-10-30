use super::*;

fn letter(
    kind: MessageKind,
    question: Option<(QuestionFor, Severity)>,
    reply_to: Option<MessageId>,
) -> Letter {
    Letter {
        id: 42,
        to: LEAD_TAG.to_owned(),
        from: "Linus (@sub-fn)".to_owned(),
        kind,
        question,
        work: Some(4),
        reply_to,
        body: "Append sub(a, b) only.".to_owned(),
    }
}

#[test]
fn a_note_says_it_takes_no_reply_and_every_letter_keeps_its_mark() {
    let note = letter(MessageKind::Note, None, None).text();
    assert_eq!(
        note,
        "[crazyagent msg 42] from Linus (@sub-fn) (note, it takes no reply) on work 4:\nAppend sub(a, b) only."
    );
    assert_eq!(
        letter(MessageKind::Prompt, None, None).text(),
        "[crazyagent msg 42] from Linus (@sub-fn) (work for you) on work 4:\nAppend sub(a, b) only."
    );
    assert_eq!(
        letter(MessageKind::Reply, None, Some(7)).text(),
        "[crazyagent msg 42] from Linus (@sub-fn) (reply to msg 7) on work 4:\nAppend sub(a, b) only."
    );
    assert_eq!(
        letter(
            MessageKind::Question,
            Some((QuestionFor::Master, Severity::High)),
            None
        )
        .text(),
        "[crazyagent msg 42] from Linus (@sub-fn) (question for the master, high severity) on work 4:\nAppend sub(a, b) only."
    );
    for kind in [
        MessageKind::Note,
        MessageKind::Prompt,
        MessageKind::Reply,
        MessageKind::Question,
    ] {
        let text = letter(kind, None, Some(7)).text();
        assert!(text.starts_with(MARK), "{text}");
        assert_eq!(marks(&text), vec![42], "{text}");
    }
}
