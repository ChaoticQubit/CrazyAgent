use super::*;

#[test]
fn claude_private_prompt_requires_observed_prompt_and_accepts_one_move_only_secret() {
    let mut slot = ClaudeInputSlot::default();
    let first = RequestId::new("claude-private-input-first").unwrap();
    let duplicate = RequestId::new("claude-private-input-duplicate").unwrap();
    assert!(slot.reserve(&first, 1).is_err());
    slot.open();
    assert!(slot.reserve(&first, 2).is_err());
    slot.reserve(&first, 1).unwrap();
    assert!(slot.reserve(&duplicate, 1).is_err());
    slot.submit(
        &first,
        AccountSecret::new(b"synthetic-code#synthetic-state".to_vec()).unwrap(),
    )
    .unwrap();
    let secret = slot.take().unwrap();
    assert_eq!(secret.into_bytes(), b"synthetic-code#synthetic-state");
    assert!(slot.take().is_none());
    assert!(slot.reserve(&duplicate, 1).is_err());
}

#[test]
fn claude_private_prompt_closure_scrubs_pending_input_and_cannot_reopen() {
    let mut slot = ClaudeInputSlot::default();
    let request = RequestId::new("claude-private-input-cancelled").unwrap();
    slot.open();
    slot.reserve(&request, 1).unwrap();
    slot.submit(
        &request,
        AccountSecret::new(b"synthetic-code#synthetic-state".to_vec()).unwrap(),
    )
    .unwrap();
    slot.close();
    assert!(slot.take().is_none());
    slot.open();
    assert!(slot.reserve(&request, 1).is_err());
}
