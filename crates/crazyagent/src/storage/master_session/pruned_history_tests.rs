use super::*;

const FIRST_REPLY: &str = "The project is a coding agent engine.";
const NEXT: &str = "What should we build next?";

#[tokio::test]
async fn a_conversation_past_the_prune_bound_still_reads_its_turns_and_starts_a_new_one() {
    let case = Fixture::new(true).await;
    case.register().await;
    case.reply_with(FIRST_REPLY, Vec::new(), 1).await;
    for n in 0..=crate::conversation::ENTRIES_MAX {
        owner_apply(
            &case.storage,
            &case.started,
            &format!("filler-{n}"),
            OwnerMutation::Conversation {
                mutation: ConversationMutation::AppendMessage {
                    project_id: case.session.project_id.clone(),
                    text: format!("filler {n}"),
                },
            },
        )
        .await;
    }
    let input = signed(case.turn.input_cursor.value()).unwrap();
    let db = case.db();
    let kept: Option<String> = db
        .query_row(
            "SELECT text FROM conversation_entries WHERE event_cursor=?1",
            [input],
            |row| row.get(0),
        )
        .optional()
        .unwrap();
    assert_eq!(
        kept.as_deref(),
        Some(PROMPT),
        "the first turn's message outlives pruning"
    );
    let pruned: bool = db
        .query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM conversation_entries WHERE project=?1 AND text='filler 0')",
            [case.session.project_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        pruned,
        "the conversation passed its bound and dropped its oldest plain message"
    );
    drop(db);

    let (_, turns) = case.view().await;
    assert_eq!(turns.len(), 1, "the turn history still reads");
    assert_eq!(turns[0].phase, MasterTurnPhase::Succeeded);
    assert_eq!(
        case.result(case.submit("after-prune", NEXT)).await,
        OwnerResult::Applied
    );
    let (_, turns) = case.view().await;
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1].phase, MasterTurnPhase::Queued);
    let MasterRuntimeResult::Prepared(prepared) = case
        .runtime(MasterRuntimeCommand::Prepare {
            host: case.started.host.clone(),
            project_id: case.session.project_id.clone(),
            session_id: case.session.session_id.clone(),
            turn_id: turns[1].id.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("the new turn prepares past the prune bound");
    };
    assert_eq!(prepared.message, NEXT);
    assert!(
        prepared
            .text
            .contains(&format!("Owner: {PROMPT}\nYou: {FIRST_REPLY}")),
        "the new turn's prompt carries the earlier turn: {}",
        prepared.text
    );
    drop(prepared);

    let writer = Connection::open(&case.path).unwrap();
    writer
        .busy_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    assert_eq!(
        writer
            .execute(
                "DELETE FROM conversation_entries WHERE event_cursor=?1",
                [input],
            )
            .unwrap(),
        1,
        "an older engine pruned the first turn's message"
    );
    drop(writer);
    let (_, turns) = case.view().await;
    assert_eq!(
        turns.len(),
        2,
        "turns whose message an older engine pruned still read"
    );
    let mut db = case.db();
    let tx = db.transaction().unwrap();
    let text = prompt(&tx, &case.session, &turns[1]).unwrap();
    drop(tx);
    assert!(
        text.contains(&format!("Owner: {INPUT_GONE}\nYou: {FIRST_REPLY}")),
        "the earlier turn keeps its place with a plain line: {text}"
    );
    case.close().await;
}

/// OB-6, Claude Code to the one-turn master: a turn run in the session's live terminal is
/// part of the one conversation, so the next turn the one-turn master runs carries it in
/// its history after the engine-run turn it followed, and the store holds the divider of
/// that switch before the new turn.
#[tokio::test]
async fn the_next_one_turn_prompt_carries_what_was_said_in_the_live_terminal() {
    let case = Fixture::new(true).await;
    case.register().await;
    case.reply_with(FIRST_REPLY, Vec::new(), 1).await;
    let writer = Connection::open(&case.path).unwrap();
    writer
        .busy_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let (project, session) = (&case.session.project_id, case.session.session_id.as_str());
    crate::master_sessions::mark_owner_turn(&writer, project, session, "claude-code").unwrap();
    crate::master_sessions::mark_owner_reply(
        &writer,
        project,
        session,
        "claude-code",
        "Footer fixed",
    )
    .unwrap();
    drop(writer);

    assert_eq!(
        case.result(case.submit("after-terminal", NEXT)).await,
        OwnerResult::Applied
    );

    let (_, turns) = case.view().await;
    let mut db = case.db();
    let tx = db.transaction().unwrap();
    let text = prompt(&tx, &case.session, &turns[1]).unwrap();
    let stored: Vec<String> = tx
        .prepare(
            "SELECT speaker || '/' || mode FROM session_messages WHERE session=?1 ORDER BY seq",
        )
        .unwrap()
        .query_map([session], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    drop(tx);
    let engine_turn = text
        .find(&format!("Owner: {PROMPT}\nYou: {FIRST_REPLY}"))
        .expect("the engine-run turn is in the history");
    let terminal_turn = text
        .find("Owner, in the Claude Code terminal: (The typed words were not kept.)\nYou, in the Claude Code terminal: Footer fixed")
        .expect("the terminal turn is in the history");
    assert!(engine_turn < terminal_turn, "{text}");
    assert_eq!(
        stored,
        [
            "owner/one_turn",
            "master/one_turn",
            "owner/live",
            "master/live",
            "switch/one_turn",
            "owner/one_turn",
        ]
    );
    case.close().await;
}
