use super::*;

/// V243-NATIVE-INPUT-PIN: a pinned input-only view types, pastes and sizes only its pane,
/// never by name, and ends as `terminal_ended` at its first send once that pane is no longer
/// its agent's own. Its prompt has its own row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pinned_input_only_view_sends_only_to_its_pane_and_ends_once_it_cannot() {
    let agent = AgentId::team(1).unwrap();
    let at = PaneAt {
        pane: "%3".into(),
        serial: 7,
    };
    let recording = Recording::new(vec![agent.clone()]);
    recording.own_is(Ok(at.clone()));
    let (queue, mut asked) = tokio::sync::mpsc::channel(8);
    let running = {
        let host: Arc<dyn Host> = recording.clone();
        let (agent, pinned) = (agent.clone(), Some(at.clone()));
        tokio::spawn(async move {
            let (mut ours, _theirs) = tokio::io::duplex(1 << 16);
            let view = View {
                seat: seated(&host, &agent, pinned.clone()),
                agent,
                cols: 80,
                rows: 24,
                viewer: None,
                pinned,
            };
            let viewing = Counted::allowing(usize::MAX);
            pump(&mut ours, view, &mut asked, host, &viewing).await
        })
    };
    for request in [
        TerminalRequest::Input {
            bytes: b"a".to_vec(),
        },
        TerminalRequest::Paste {
            bytes: b"p".to_vec(),
            last: true,
        },
        TerminalRequest::Resize { cols: 90, rows: 30 },
    ] {
        queue.send(request).await.unwrap();
    }
    let landed = tokio::time::timeout(WAIT, async {
        loop {
            let count = recording.at_pane.lock().unwrap().len();
            if count >= 3 {
                return count;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert_eq!(
        landed,
        Ok(3),
        "the key, paste and resize reached the pinned pane"
    );
    assert_eq!(
        *recording.at_pane.lock().unwrap(),
        vec![b"a".to_vec(), b"p".to_vec(), b"90x30".to_vec()]
    );

    recording.own_is(Ok(PaneAt {
        pane: "%4".into(),
        serial: 7,
    }));
    queue
        .send(TerminalRequest::Input {
            bytes: b"b".to_vec(),
        })
        .await
        .unwrap();
    let reason = tokio::time::timeout(WAIT, running).await.unwrap().unwrap();
    assert_eq!(reason, "terminal_ended");
    assert!(
        recording.typed.lock().unwrap().is_empty(),
        "nothing was typed by name"
    );
    assert!(
        recording.pasted.lock().unwrap().is_empty(),
        "nothing was pasted by name"
    );
    assert_eq!(
        recording.at_pane.lock().unwrap().len(),
        3,
        "the key after the move never landed"
    );
}
