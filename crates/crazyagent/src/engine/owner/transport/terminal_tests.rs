use super::*;
use crate::team::master::{OwnerTyping, owner_key_ms};

#[test]
fn an_owner_prompt_counts_as_an_owner_key_as_typed_keys_do() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("engine.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    crate::team::initialize(&connection).unwrap();
    let typing = OwnerTyping::new(database.clone());
    let requests = [
        (7, TerminalRequest::Prompt { text: "2".into() }),
        (
            8,
            TerminalRequest::Input {
                bytes: b"2\r".to_vec(),
            },
        ),
        (
            9,
            TerminalRequest::Resize {
                cols: 100,
                rows: 30,
            },
        ),
        (10, TerminalRequest::Detach),
    ];
    let at = crate::team::now_ms();
    for (agent, request) in &requests {
        if let Some(keys) = owner_keys(request) {
            typing.record(*agent, keys, at);
        }
    }
    let keyed = |agent: i64| owner_key_ms(&connection, Some(&typing), agent).unwrap();
    assert_eq!(
        keyed(7),
        Some(at),
        "a prompt the owner sends from the terminal panel answers a box on that screen as the owner"
    );
    assert_eq!(keyed(8), Some(at));
    assert_eq!((keyed(9), keyed(10)), (None, None));
}
