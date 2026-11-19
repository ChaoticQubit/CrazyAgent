use super::*;

#[test]
fn where_the_intent_store_has_no_private_folder_judge_it_opens_as_no_store_and_makes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let opened = Intents::open(root.path());
    assert!(
        opened.is_ok(),
        "the intent store refuses to open, so storage cannot open: {:?}",
        opened.err()
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
