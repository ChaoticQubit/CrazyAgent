use super::TEXT_BYTES_MAX;

/// The byte bound each agent output write enforces before it stores a text, by table and
/// writer, as traced at chore/v250 e4fd9fb49 (v2.5 L2b, DEC 250-OWNER-TEXT). Each table's
/// text goes to `owner_texts::keep` once its aggregate is on the log, so each bound must fit
/// the store's cap.
const OUTPUT_BOUNDS: [(&str, usize); 7] = [
    (
        "agent_texts: storage/agent_text.rs save",
        crate::agent_home::TEXT_BYTES_MAX,
    ),
    (
        "team_agent_messages: team/agent_messages.rs record",
        crate::team::agent_messages::SAID_BYTES_MAX,
    ),
    (
        "team_messages: team.rs append_message",
        crate::team::MESSAGE_BYTES_MAX * 2,
    ),
    (
        "session_messages: master_sessions.rs append and bind_typed",
        crate::master_sessions::MESSAGE_BYTES_MAX,
    ),
    (
        "master_turns: storage/master_session.rs save_turn and queue_turn",
        crate::master_session::METADATA_BYTES_MAX,
    ),
    (
        "conversation_entries: conversation.rs apply",
        crate::conversation::TEXT_BYTES_MAX,
    ),
    (
        "agent_runs.last_text: agents.rs text",
        crate::agents::LAST_TEXT_BYTES_MAX,
    ),
];

/// V2.5 L2b: every agent output write's bound fits the shared text store, so no output a
/// write accepts is refused as `owner_text_too_large` once its aggregate keeps it there.
#[test]
fn every_agent_output_bound_fits_the_shared_text_store() {
    for (writer, bound) in OUTPUT_BOUNDS {
        assert!(bound > 0, "{writer}");
        assert!(
            bound <= TEXT_BYTES_MAX,
            "{writer}: {bound} bytes, the store keeps at most {TEXT_BYTES_MAX}"
        );
    }
}
