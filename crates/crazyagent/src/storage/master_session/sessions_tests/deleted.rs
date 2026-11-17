use super::*;

#[tokio::test]
async fn deleting_a_session_keeps_the_learning_tick_its_owner_messages_gave() {
    let case = Case::new().await;
    assert_eq!(case.open("open-first").await, OwnerResult::Applied);
    let first = case.shown().await;
    assert_eq!(
        case.submit("first-line", &first.session_id, "Write the import guide")
            .await,
        OwnerResult::Applied
    );
    assert_eq!(case.renew("renew-first", true).await, OwnerResult::Applied);
    let master_done = |case: &Case| {
        let mut db = case.db();
        let tx = db.transaction().unwrap();
        crate::learning::progress(&tx)
            .unwrap()
            .steps
            .iter()
            .any(|step| step.id == crate::learning::StepId::Master && step.done())
    };
    assert!(master_done(&case));
    assert_eq!(
        case.delete("delete-first", &first.session_id).await,
        OwnerResult::Applied
    );
    let messages: i64 = case
        .db()
        .query_row(
            "SELECT COUNT(*) FROM conversation_entries WHERE kind='message'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(messages, 0, "the deleted session's messages are gone");
    assert!(master_done(&case), "the tick outlives the messages");
    let kept: i64 = case
        .db()
        .query_row(
            "SELECT COUNT(*) FROM learning_marks WHERE step='master'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kept, 1, "the delete kept the master step as a mark");
    case.close().await;
}
