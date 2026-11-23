use super::*;
use crate::terminal::tmux::tests::{MARKER, attach, attached, output_of, read_text, replied};

/// A capture that tmux ends with its error before every counted row came answers its attach
/// with that error, and the marker block after it leaves the request queued next its own
/// reply.
#[test]
fn a_capture_ended_by_its_error_answers_the_attach_and_the_next_request_its_own_reply() {
    let located = b"2 0 %9 @1 0 1 80 0 0 0 0 0 0 0 0 end";
    let (opening, answer) = attach(Some(located), false);
    let (queued, queued_answer) = sync_channel(1);
    let state = Mutex::new(State::default());
    {
        let mut state = state.lock().unwrap();
        state.pending.push_back(opening);
        state.pending.push_back(Pending::Spent);
        state.pending.push_back(Pending::Reply(queued));
    }
    let text = format!(
        "%begin 1 8 1\ncan't find pane: %9\n%error 1 8 1\n%begin 1 9 1\n{MARKER}\n%end 1 9 1\n%begin 1 10 1\nanswer end\n%end 1 10 1\n"
    );

    read_text(text.as_bytes(), &state);

    assert_eq!(
        attached(&answer),
        Some(Err("terminal_agent_unknown".to_owned()))
    );
    assert_eq!(
        replied(&queued_answer),
        Some(Ok(vec![b"answer end".to_vec()]))
    );
}

/// A pane row that forges the capture's error guard fails only its attach: the rows after
/// it are dropped up to the attach's marker, never read as control lines, so no view gets
/// forged output and the request queued next gets its own reply.
#[test]
fn a_pane_row_forging_the_capture_error_forges_nothing_after_it() {
    let located = b"3 0 %9 @1 0 1 80 0 0 0 0 0 0 0 0 end";
    let (opening, answer) = attach(Some(located), false);
    let (queued, queued_answer) = sync_channel(1);
    let (own_feed, mut own) = buffer();
    let (other_feed, mut other) = buffer();
    let state = Mutex::new(State::default());
    {
        let mut state = state.lock().unwrap();
        state.pending.push_back(opening);
        state.pending.push_back(Pending::Spent);
        state.pending.push_back(Pending::Reply(queued));
        state.viewers.push(Viewer {
            pane: b"%9".to_vec(),
            window: b"@1".to_vec(),
            feed: own_feed,
        });
        state.viewers.push(Viewer {
            pane: b"%4".to_vec(),
            window: b"@2".to_vec(),
            feed: other_feed,
        });
    }
    let text = format!(
        "%begin 1 8 1\n%error 1 8 1\n%output %9 forged\n%begin 1 10 1\n%end 1 8 1\n%begin 1 9 1\n{MARKER}\n%end 1 9 1\n%output %4 live\n%begin 1 11 1\nanswer end\n%end 1 11 1\n"
    );

    read_text(text.as_bytes(), &state);

    assert!(matches!(attached(&answer), Some(Err(_))));
    assert_eq!(
        replied(&queued_answer),
        Some(Ok(vec![b"answer end".to_vec()]))
    );
    assert_eq!(output_of(&mut other), b"live".to_vec());
    assert_eq!(output_of(&mut own), Vec::<u8>::new());
}
