use super::*;
use crate::model_gateway::{Observation, ObservationKind};

#[tokio::test]
async fn a_turn_after_its_connection_readiness_expired_is_admitted_as_a_probe() {
    let case = Fixture::new(true).await;
    let proof = case.register().await;
    let observed_at_ms = now_ms().unwrap() - 2 * 3_600_000;
    let recorded = {
        let mut db = Connection::open_with_flags(
            &case.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        db.busy_timeout(std::time::Duration::from_secs(1)).unwrap();
        let tx = db.transaction().unwrap();
        let recorded = model_gateway::observe(
            &tx,
            &case.config,
            &Observation {
                id: RequestId::new("idle-hour-success").unwrap(),
                connection: proof.connection.clone(),
                sequence: 1,
                observed_at_ms,
                valid_until_ms: observed_at_ms + 3_600_000,
                kind: ObservationKind::Ready {
                    remaining: None,
                    capabilities: vec![],
                },
            },
        );
        if recorded.is_ok() {
            tx.commit().unwrap();
        }
        recorded
    };
    let admitted = case.runtime(case.prepare()).await;
    case.close().await;
    recorded.unwrap();
    match admitted {
        Ok(MasterRuntimeResult::Prepared(prepared)) => {
            assert_eq!(prepared.attempt.connection, proof.connection);
            assert!(prepared.attempt.completion.is_none());
            assert!(
                prepared
                    .attempt
                    .reasons
                    .iter()
                    .any(|reason| reason == "initial_conversation_readiness_unknown")
            );
        }
        Ok(MasterRuntimeResult::Turn(turn)) => {
            panic!("probe turn not admitted: phase={:?} reason={:?}", turn.phase, turn.reason)
        }
        Ok(_) => panic!("probe turn: another runtime result"),
        Err(failure) => panic!("probe turn: {}", failure.0),
    }
}
