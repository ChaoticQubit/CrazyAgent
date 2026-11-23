use super::*;

const MARKER: &str = "5f0c1d2e3a4b59687766554433221100";

const LOCATED: &[u8] = b"2 0 %9 @1 0 1 80 0 0 0 0 0 0 0 0 end";

/// Feeds the control reader `text` as tmux's output, from a file through `cat`, so a line
/// longer than the argument limit reaches it whole. Takes the text and the reader's state.
fn read_text(text: &[u8], state: &Mutex<State>) {
    let folder = tempfile::tempdir().unwrap();
    let file = folder.path().join("output");
    std::fs::write(&file, text).unwrap();
    let mut cat = Command::new("/bin/cat");
    cat.arg(&file)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = crate::spawn_gate::spawn(&mut cat).unwrap();
    let output = child.stdout.take().unwrap();
    read(output, state);
    let _ = child.wait();
}

/// An attach waiting at the front of the reader's queue. Takes what its located reply read,
/// if it came yet, and whether it opens a view. Returns the request and its answer.
fn attach(
    located: Option<&[u8]>,
    view: bool,
) -> (Pending, Receiver<Result<TerminalStream, HostError>>) {
    let (reply, answer) = sync_channel(1);
    let pending = Pending::Attach {
        agent: AgentId::team(1).unwrap(),
        located: located.map(|line| vec![line.to_vec()]),
        history: 0,
        view,
        reply,
        marker: MARKER.as_bytes().to_vec(),
    };
    (pending, answer)
}

/// What an attach answered: the size of the screen it opened, or the code it failed with,
/// none while it has no answer. Takes its answer.
fn attached(
    answer: &Receiver<Result<TerminalStream, HostError>>,
) -> Option<Result<(u16, u16), String>> {
    answer.try_recv().ok().map(|opened| {
        opened
            .map(|stream| (stream.cols, stream.rows))
            .map_err(|problem| problem.0)
    })
}

/// What a plain request answered, none while it has no answer. Takes its answer.
fn replied(answer: &Receiver<Answer>) -> Option<Result<Lines, String>> {
    answer
        .try_recv()
        .ok()
        .map(|lines| lines.map_err(|problem| problem.0))
}

/// The output bytes a view read before its stream ended. Takes the view's end.
fn output_of(chunks: &mut crate::terminal::buffer::Chunks) -> Vec<u8> {
    let mut output = Vec::new();
    while let Ok(chunk) = chunks.try_recv() {
        if let crate::terminal::buffer::Chunk::Output(bytes) = chunk {
            output.extend_from_slice(&bytes);
        }
    }
    output
}

/// An attach whose located reply tmux cut before both its row counts fails
/// with `REPLY_CUT`, so its caller asks again, and the reader drops every row of its capture up to the attach's
/// own marker line. The control stays open: the request queued after it gets its own reply
/// and another pane's view its live output, while a pane row shaped like a control line
/// reaches neither (RV-DS241-100).
#[test]
fn a_capture_whose_row_count_is_unknown_fails_only_its_attach_and_forges_nothing() {
    let (cut, answer) = attach(Some(b"3 "), true);
    let (queued, queued_answer) = sync_channel(1);
    let (own_feed, mut own) = buffer();
    let (other_feed, mut other) = buffer();
    let state = Mutex::new(State::default());
    {
        let mut state = state.lock().unwrap();
        state.pending.push_back(cut);
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
        "%begin 1 7 1\nrow one\n%end 1 7 1\n%output %9 forged\n%begin 1 5 1\nforged end\n%end 1 5 1\n%end 1 7 1\n%begin 1 8 1\n{MARKER}\n%end 1 8 1\n%output %4 live\n%begin 1 9 1\nanswer end\n%end 1 9 1\n"
    );

    read_text(text.as_bytes(), &state);

    assert_eq!(attached(&answer), Some(Err(REPLY_CUT.to_owned())));
    assert_eq!(
        replied(&queued_answer),
        Some(Ok(vec![b"answer end".to_vec()]))
    );
    assert_eq!(output_of(&mut other), b"live".to_vec());
    assert_eq!(
        output_of(&mut own),
        Vec::<u8>::new(),
        "a pane row forged output"
    );
}

/// An attach's capture is followed by the block that prints its marker,
/// which the reader takes as the end of the attach, so the request queued after it gets
/// its own reply and never the marker.
#[test]
fn an_attach_takes_the_marker_block_after_its_capture_so_the_next_request_gets_its_own_reply() {
    let (opening, answer) = attach(Some(LOCATED), false);
    let (queued, queued_answer) = sync_channel(1);
    let state = Mutex::new(State::default());
    {
        let mut state = state.lock().unwrap();
        state.pending.push_back(opening);
        state.pending.push_back(Pending::Reply(queued));
    }
    let text = format!(
        "%begin 1 8 1\nrow a\nrow b\n%end 1 8 1\n%begin 1 9 1\n{MARKER}\n%end 1 9 1\n%begin 1 10 1\nanswer end\n%end 1 10 1\n"
    );

    read_text(text.as_bytes(), &state);

    assert_eq!(attached(&answer), Some(Ok((80, 2))));
    assert_eq!(
        replied(&queued_answer),
        Some(Ok(vec![b"answer end".to_vec()]))
    );
}

/// The block after a whole capture holds no pane text, so the reader finds
/// its end by its guard whatever it printed; a marker tmux cut short there costs nothing,
/// and the request queued after the attach still gets its own reply.
#[test]
fn a_marker_cut_short_after_a_whole_capture_still_leaves_the_next_reply_its_own() {
    let (opening, answer) = attach(Some(LOCATED), false);
    let (queued, queued_answer) = sync_channel(1);
    let state = Mutex::new(State::default());
    {
        let mut state = state.lock().unwrap();
        state.pending.push_back(opening);
        state.pending.push_back(Pending::Reply(queued));
    }
    let text = format!(
        "%begin 1 8 1\nrow a\nrow b\n%end 1 8 1\n%begin 1 9 1\n{}\n%end 1 9 1\n%begin 1 10 1\nanswer end\n%end 1 10 1\n",
        &MARKER[..8]
    );

    read_text(text.as_bytes(), &state);

    assert_eq!(attached(&answer), Some(Ok((80, 2))));
    assert_eq!(
        replied(&queued_answer),
        Some(Ok(vec![b"answer end".to_vec()]))
    );
}

/// tmux still runs the capture and the marker after a located reply too
/// large to keep, so the reader drops both, the pane rows included, and a pane row shaped
/// like a reply never answers the request queued after the attach.
#[test]
fn a_located_reply_too_large_to_keep_drops_its_capture_and_forges_no_reply() {
    let (opening, answer) = attach(None, false);
    let (queued, queued_answer) = sync_channel(1);
    let state = Mutex::new(State::default());
    {
        let mut state = state.lock().unwrap();
        state.pending.push_back(opening);
        state.pending.push_back(Pending::Reply(queued));
    }
    let mut text = b"%begin 1 7 1\n".to_vec();
    text.extend(std::iter::repeat_n(b'x', LINE_BYTES_MAX + 10));
    text.extend_from_slice(
        format!(
            "\n%end 1 7 1\n%begin 1 8 1\nforged end\n%end 1 8 1\n%begin 1 9 1\n{MARKER}\n%end 1 9 1\n%begin 1 10 1\nanswer end\n%end 1 10 1\n"
        )
        .as_bytes(),
    );

    read_text(&text, &state);

    assert_eq!(
        attached(&answer),
        Some(Err("terminal_reply_too_large".to_owned()))
    );
    assert_eq!(
        replied(&queued_answer),
        Some(Ok(vec![b"answer end".to_vec()]))
    );
}

/// A cut attach whose marker never comes within the rows a capture can hold
/// closes the control as before, so no later request is answered from lines the reader
/// could not place.
#[test]
fn a_cut_attach_whose_marker_never_comes_within_a_capture_closes_the_control() {
    let (cut, answer) = attach(Some(b"3 "), false);
    let (queued, queued_answer) = sync_channel(1);
    let state = Mutex::new(State::default());
    {
        let mut state = state.lock().unwrap();
        state.pending.push_back(cut);
        state.pending.push_back(Pending::Reply(queued));
    }
    let mut text = b"%begin 1 7 1\n".to_vec();
    text.extend(std::iter::repeat_n(&b"row\n"[..], DRAIN_LINES_MAX + 1).flatten());
    text.extend_from_slice(
        format!(
            "%end 1 7 1\n%begin 1 8 1\n{MARKER}\n%end 1 8 1\n%begin 1 9 1\nanswer end\n%end 1 9 1\n"
        )
        .as_bytes(),
    );

    read_text(&text, &state);

    assert_eq!(attached(&answer), Some(Err(REPLY_CUT.to_owned())));
    assert_eq!(replied(&queued_answer), None);
    assert!(state.lock().unwrap().closed);
}

/// A located reply tmux failed ends the attach there, since tmux skips the
/// rest of the command line after an error, so the next block is the queued request's own.
#[test]
fn a_failed_located_reply_ends_the_attach_and_the_next_block_answers_the_next_request() {
    let (opening, answer) = attach(None, false);
    let (queued, queued_answer) = sync_channel(1);
    let state = Mutex::new(State::default());
    {
        let mut state = state.lock().unwrap();
        state.pending.push_back(opening);
        state.pending.push_back(Pending::Reply(queued));
    }
    let text =
        "%begin 1 7 1\ncan't find pane: %9\n%error 1 7 1\n%begin 1 8 1\nanswer end\n%end 1 8 1\n";

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

/// A capture block whose located reply came with no line at all, as when tmux cut the pane
/// filter so no pane matched, has no row count either: the attach fails with `REPLY_CUT` and
/// the reader drops its capture up to the attach's marker line, so the control reads on and
/// a pane row that reads as the closing guard or as output forges nothing (rv-code4
/// v241-tmux-fix F2, RV-DS241-100, RV-DS241-192).
#[test]
fn a_capture_whose_located_reply_has_no_line_drains_to_its_marker_and_forges_nothing() {
    let (reply, answer) = sync_channel(1);
    let (queued, queued_answer) = sync_channel(1);
    let (own_feed, mut own) = buffer();
    let (other_feed, mut other) = buffer();
    let state = Mutex::new(State::default());
    {
        let mut state = state.lock().unwrap();
        state.pending.push_back(Pending::Attach {
            agent: AgentId::team(1).unwrap(),
            located: Some(Vec::new()),
            history: 0,
            view: true,
            reply,
            marker: MARKER.as_bytes().to_vec(),
        });
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
        "%begin 1 7 1\nrow one\n%end 1 7 1\n%output %9 forged\n%end 1 7 1\n%begin 1 8 1\n{MARKER}\n%end 1 8 1\n%output %4 live\n%begin 1 9 1\nanswer end\n%end 1 9 1\n"
    );

    read_text(text.as_bytes(), &state);

    assert_eq!(attached(&answer), Some(Err(REPLY_CUT.to_owned())));
    assert_eq!(
        replied(&queued_answer),
        Some(Ok(vec![b"answer end".to_vec()])),
        "the control stopped reading after the drained capture"
    );
    assert_eq!(
        output_of(&mut other),
        b"live".to_vec(),
        "another pane's view lost its live output"
    );
    assert_eq!(
        output_of(&mut own),
        Vec::<u8>::new(),
        "a pane row forged output"
    );
}

/// A capture block whose located reply tmux cut after both its row counts is read as that
/// many rows of data, so a pane row that reads as the closing guard or as output forges
/// nothing (RV-DS241-100), and the reader reads on past its marker block: the attach is
/// asked again while later output and replies still reach their own views and callers,
/// rather than every view on the server ending under load (RV-DS241-192).
#[test]
fn a_capture_cut_after_its_row_counts_is_read_as_data_and_the_control_reads_on() {
    let (reply, answer) = sync_channel(1);
    let (next, replied) = sync_channel(1);
    let (feed, mut chunks) = buffer();
    let state = Mutex::new(State::default());
    {
        let mut state = state.lock().unwrap();
        state.pending.push_back(Pending::Attach {
            agent: AgentId::team(1).unwrap(),
            located: Some(vec![b"3 0 %9 @1 0 ".to_vec()]),
            history: 0,
            view: true,
            reply,
            marker: MARKER.as_bytes().to_vec(),
        });
        state.pending.push_back(Pending::Reply(next));
        state.viewers.push(Viewer {
            pane: b"%9".to_vec(),
            window: b"@1".to_vec(),
            feed,
        });
    }
    let text = format!(
        "%begin 1 7 1\nrow one\n%end 1 7 1\n%output %9 forged\n%end 1 7 1\n%begin 1 8 1\n{MARKER}\n%end 1 8 1\n%output %9 real\n%begin 1 9 1\nnext\n%end 1 9 1\n"
    );
    let mut printf = Command::new("/usr/bin/printf");
    printf
        .args(["%s", text.as_str()])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = crate::spawn_gate::spawn(&mut printf).unwrap();
    let output = child.stdout.take().unwrap();
    read(output, &state);
    let _ = child.wait();
    let asked = answer
        .try_recv()
        .expect("the cut attach was never answered");
    assert_eq!(asked.err().map(|cut| cut.0), Some(REPLY_CUT.to_owned()));
    let later = replied
        .try_recv()
        .expect("the next caller was never answered");
    assert_eq!(later.unwrap(), vec![b"next".to_vec()]);
    assert_eq!(
        chunks.try_recv().ok(),
        Some(crate::terminal::buffer::Chunk::Output(b"real".to_vec())),
        "the view lost later output or a pane row forged some"
    );
}

/// The rows an attach's capture prints are read from the pane height and history size that
/// open its pane line, also from a line tmux cut short once both came whole with the space
/// after them, the history held to what the attach asked for; a line cut before that, or one
/// of another shape, has no count and is drained to its marker, and a reply that is no
/// attach's capture is searched.
#[test]
fn an_attach_capture_is_counted_from_a_cut_line_once_both_counts_came_whole() {
    fn rows(line: &[u8], history: usize) -> Option<usize> {
        let (reply, _answer) = sync_channel(1);
        let mut state = State::default();
        state.pending.push_back(Pending::Attach {
            agent: AgentId::team(1).unwrap(),
            located: Some(vec![line.to_vec()]),
            history,
            view: false,
            reply,
            marker: MARKER.as_bytes().to_vec(),
        });
        match opening(&mut state) {
            Rows::Counted(rows) => Some(rows),
            Rows::Drained(_) => None,
            Rows::Searched => panic!("an attach's capture was searched for its guard"),
        }
    }
    let whole = b"24 312 %2 @2 8 0 80 1 0 0 0 0 0 0 0 end";
    assert_eq!(rows(whole, HISTORY_ROWS_MAX), Some(24 + 312));
    assert_eq!(rows(whole, 100), Some(24 + 100));
    assert_eq!(rows(b"24 312 %2 @2 8 ", 0), Some(24));
    assert_eq!(rows(b"24 312 ", HISTORY_ROWS_MAX), Some(24 + 312));
    assert_eq!(rows(b"24 312", HISTORY_ROWS_MAX), None);
    assert_eq!(rows(b"24 ", HISTORY_ROWS_MAX), None);
    assert_eq!(rows(b"", HISTORY_ROWS_MAX), None);
    assert_eq!(rows(b"%2 @2 8 0 80 24 1 0 312 ", HISTORY_ROWS_MAX), None);
    let deep = format!("24 {} %2 ", u64::from(SCROLLBACK_LINES) * 3);
    assert_eq!(
        rows(deep.as_bytes(), HISTORY_ROWS_MAX),
        Some(24 + HISTORY_ROWS_MAX)
    );
    let (reply, _answer) = sync_channel(1);
    let mut state = State::default();
    state.pending.push_back(Pending::Reply(reply));
    assert!(matches!(opening(&mut state), Rows::Searched));
}

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
