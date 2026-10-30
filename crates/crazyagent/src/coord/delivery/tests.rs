use super::*;
use crate::coord::{BODY_BYTES_MAX, Message};

#[test]
fn a_note_line_says_it_takes_no_reply_within_the_head_bound() {
    let connection = Connection::open_in_memory().unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    crate::team::initialize(&connection).unwrap();
    let project = ProjectId::new("lines").unwrap();
    connection
        .execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms) VALUES (?1,'sub','codex',0,'working',1,1)",
            [project.as_str()],
        )
        .unwrap();
    let agent = connection.last_insert_rowid();
    assert!(
        crate::team::names::assign_role(&connection, &project, agent, &"a".repeat(64))
            .unwrap()
            .is_ok()
    );
    let message = Message {
        id: MessageId::MAX,
        project: project.clone(),
        from: Party::Agent(agent),
        to: Party::Owner,
        kind: MessageKind::Note,
        question: None,
        work: None,
        reply_to: None,
        body: "y".repeat(BODY_BYTES_MAX),
        receipt: Receipt::Sent,
        created: 1,
    };
    let noted = line(&connection, &message, NEXT_TURN_BYTES_MAX).unwrap();
    let head = noted.strip_suffix(message.body.as_str()).unwrap();
    assert!(head.len() <= HEAD_BYTES_MAX, "{head}");
    assert!(head.starts_with(&format!("[crazyagent msg {}] from ", MessageId::MAX)));
    assert!(head.ends_with(": note, it takes no reply: "), "{head}");
    let prompt = Message {
        kind: MessageKind::Prompt,
        ..message
    };
    let asked = line(&connection, &prompt, NEXT_TURN_BYTES_MAX).unwrap();
    assert!(asked.contains(": work for you, answer with update_work: "));
}
