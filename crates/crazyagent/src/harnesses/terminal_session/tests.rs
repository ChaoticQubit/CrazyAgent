use super::*;

fn unframed(parts: &[String]) -> String {
    let joined = parts.concat();
    assert!(joined.starts_with(PASTE_START), "{joined:?}");
    assert!(joined.ends_with(PASTE_END), "{joined:?}");
    joined[PASTE_START.len()..joined.len() - PASTE_END.len()].to_owned()
}

#[test]
fn a_prompt_is_one_bracketed_paste() {
    let parts = paste("Fix the build\nthen run\ttests");
    assert_eq!(
        parts,
        vec!["\x1b[200~Fix the build\nthen run\ttests\x1b[201~".to_owned()]
    );
}

#[test]
fn control_characters_cannot_end_the_paste_or_press_keys() {
    let parts = paste("a\x1b[201~\r\nb\rc\x03d\x7fe\u{9b}f\u{0}g");
    assert_eq!(unframed(&parts), "a[201~\nb\ncdefg");
    assert_eq!(parts.concat().matches(PASTE_END).count(), 1);
}

#[test]
fn a_long_prompt_is_split_into_host_inputs_on_character_boundaries() {
    let prompt = "é".repeat(INPUT_BYTES_MAX);
    let parts = paste(&prompt);
    assert!(parts.len() > 2);
    assert!(parts.iter().all(|part| part.len() <= INPUT_BYTES_MAX));
    assert_eq!(unframed(&parts), prompt);
}

#[test]
fn an_empty_prompt_is_still_one_paste() {
    assert_eq!(paste(""), vec!["\x1b[200~\x1b[201~".to_owned()]);
}

#[cfg(target_os = "macos")]
mod own_turns {
    use super::super::{TerminalSession, TranscriptEvent};
    use crate::harnesses::interactive::claude_project_folder;
    use crate::harnesses::launch::{AccountBinding, LaunchEvent, LaunchLimits, LaunchRequest};
    use crate::terminal::tmux::{SOCKET, Tmux, installed};
    use crate::terminal::{AgentId, FOLDER, Host};
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const STAND_IN: &str = "#!/bin/sh\n/bin/stty raw -echo\nprintf '\\033[?2004hstand-in ready\\n'\nexec /bin/cat > \"$HOME/typed\"\n";

    /// One test's tmux server, ended when the test ends.
    struct Server {
        tmux: PathBuf,
        socket: PathBuf,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = crate::spawn_gate::status(
                Command::new(&self.tmux)
                    .arg("-S")
                    .arg(&self.socket)
                    .arg("kill-server")
                    .env_clear()
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            );
        }
    }

    /// A shell stand-in for Claude Code running in a real tmux pane, with the session record
    /// the session reads, which the test writes as the app would.
    struct Pane {
        session: TerminalSession,
        record: PathBuf,
        home: PathBuf,
        _server: Server,
        _root: tempfile::TempDir,
    }

    fn private(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn start(name: &str) -> Pane {
        start_at(name, false)
    }

    /// Starts a stand-in pane under the always-allow sandbox profile, or at Full access when
    /// `unconfined`. Takes the agent name and the access. Returns the pane.
    fn start_at(name: &str, unconfined: bool) -> Pane {
        start_with(name, unconfined, STAND_IN)
    }

    /// Starts a pane that runs `script` as Claude Code, under the always-allow sandbox
    /// profile, or at Full access when `unconfined`. Takes the agent name, the access and
    /// the script. Returns the pane.
    fn start_with(name: &str, unconfined: bool, script: &str) -> Pane {
        let root = tempfile::Builder::new()
            .prefix("ca-own-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let base = root.path().canonicalize().unwrap();
        private(&base);
        let tmux = installed(&base).expect("tmux must be installed on this machine");
        let folder = base.join(FOLDER);
        let server = Server {
            tmux: tmux.clone(),
            socket: folder.join(SOCKET),
        };
        let host: Arc<dyn Host> = Arc::new(Tmux::open(Some(tmux), &folder));
        let (home, work, bin) = (base.join("home"), base.join("work"), base.join("bin"));
        for path in [&home, &work, &bin] {
            private(path);
        }
        let executable = bin.join("claude");
        std::fs::write(&executable, script).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let profile = base.join("agent.sb");
        std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
        let request = LaunchRequest {
            harness: "claude-code".into(),
            executable,
            working_dir: work.clone(),
            account: AccountBinding::Url {
                home: home.clone(),
                base_url: "http://127.0.0.1:9/v1".into(),
                key: None,
            },
            model: None,
            tools: true,
            sandbox_profile: (!unconfined).then_some(profile),
            unconfined,
            git_identity: None,
            limits: LaunchLimits::default(),
        };
        let mut session =
            TerminalSession::start(host, AgentId::new(name).unwrap(), request, None).unwrap();
        let id = match session.next_event(Duration::from_secs(1)).unwrap() {
            Some(LaunchEvent::Ready {
                native_session: Some(id),
            }) => id,
            other => panic!("expected Ready with the session id, got {other:?}"),
        };
        let record = home
            .join("projects")
            .join(claude_project_folder(&work).unwrap())
            .join(format!("{id}.jsonl"));
        private(record.parent().unwrap());
        Pane {
            session,
            record,
            home,
            _server: server,
            _root: root,
        }
    }

    impl Pane {
        fn write(&self, lines: &[&str]) {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.record)
                .unwrap();
            for line in lines {
                writeln!(file, "{line}").unwrap();
            }
        }

        fn own_facts(&mut self) -> Vec<TranscriptEvent> {
            std::iter::from_fn(|| self.session.next_own_fact()).collect()
        }
    }

    fn user(text: &str) -> String {
        format!(r#"{{"type":"user","message":{{"content":"{text}"}}}}"#)
    }

    fn reply(text: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{text}"}}],"stop_reason":"end_turn"}}}}"#
        )
    }

    const TOOL: &str =
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#;

    fn text(text: &str) -> TranscriptEvent {
        TranscriptEvent::Text {
            text: format!("{text}\n"),
        }
    }

    const DONE: TranscriptEvent = TranscriptEvent::TurnFinished { success: true };

    /// SETSID-L2 r1 L1: a Full access pane follows its lineage once `FOLLOWER_PACE` has passed
    /// since its last follow, not only after `FOLLOW_EVERY`, so beside a follower's 1 s reads
    /// its stop's own read stays within the gap a stop checks and a clean stop reads checked.
    #[test]
    fn a_full_access_pane_follows_its_lineage_every_follower_pace() {
        use crate::harnesses::leftovers::FOLLOWER_PACE;

        let mut pane = start_at("follow-pace-1", true);
        assert!(
            matches!(pane.session.lineage, Some(Ok(_))),
            "a Full access pane has a lineage to follow"
        );
        let due = Instant::now()
            .checked_sub(FOLLOWER_PACE + Duration::from_millis(50))
            .unwrap();
        pane.session.followed = due;
        pane.session.next_event(Duration::from_millis(100)).unwrap();
        assert!(
            pane.session.followed > due,
            "a pane one follower pace past its last follow did not follow"
        );
    }

    #[test]
    fn own_turn_facts_kept_between_typed_turns() {
        let mut pane = start("own-turns-1");
        pane.session.follow_own_turns();

        pane.write(&[&user("A teammate wrote"), TOOL, &reply("Own one")]);
        assert_eq!(
            pane.session.next_event(Duration::from_millis(500)).unwrap(),
            None,
            "a turn the app starts by itself is never an engine turn's event"
        );
        assert_eq!(
            pane.own_facts(),
            vec![TranscriptEvent::TurnStarted, text("Own one"), DONE],
            "its facts are kept, tool use left out"
        );

        pane.session.send("typed prompt").unwrap();
        let pasted = b"\x1b[200~typed prompt\x1b[201~\r";
        let until = Instant::now() + Duration::from_secs(15);
        while !std::fs::read(pane.home.join("typed"))
            .unwrap_or_default()
            .ends_with(pasted)
        {
            assert!(Instant::now() < until, "the prompt was never typed");
            std::thread::sleep(Duration::from_millis(50));
        }
        pane.write(&[&user("typed prompt"), TOOL, &reply("Typed reply")]);
        let mut events = Vec::new();
        while !matches!(events.last(), Some(LaunchEvent::TurnFinished { .. })) {
            match pane.session.next_event(Duration::from_secs(10)).unwrap() {
                Some(event) => events.push(event),
                None => panic!("the typed turn never finished: {events:?}"),
            }
        }
        assert_eq!(
            events,
            vec![
                LaunchEvent::ToolUse {
                    name: "Bash".into()
                },
                LaunchEvent::Text {
                    text: "Typed reply\n".into()
                },
                LaunchEvent::TurnFinished { success: true },
            ]
        );
        assert!(
            pane.own_facts().is_empty(),
            "an engine turn's facts come from next_event only"
        );

        pane.write(&[&user("Another teammate wrote"), &reply("Own two")]);
        assert_eq!(
            pane.session.next_event(Duration::from_millis(500)).unwrap(),
            None
        );
        assert_eq!(
            pane.own_facts(),
            vec![TranscriptEvent::TurnStarted, text("Own two"), DONE]
        );
        assert_eq!(pane.session.stop().unwrap(), None);
    }

    #[test]
    fn without_follow_own_turns_facts_outside_a_typed_turn_are_dropped() {
        let mut pane = start("own-turns-2");
        pane.write(&[&user("A teammate wrote"), &reply("Own one")]);
        assert_eq!(
            pane.session.next_event(Duration::from_millis(500)).unwrap(),
            None
        );
        pane.session.follow_own_turns();
        assert!(
            pane.own_facts().is_empty(),
            "facts read before following own turns are not kept"
        );
        assert_eq!(pane.session.stop().unwrap(), None);
    }

    #[test]
    #[should_panic(expected = "assertion failed: !self.own")]
    fn a_session_follows_its_own_turns_or_every_turn_never_both() {
        let mut pane = start("own-turns-3");
        pane.session.follow_own_turns();
        pane.session.follow_every_turn();
    }

    const RULE: &str = "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}";
    const READS: &str = "exec /bin/cat > \"$HOME/typed\"";
    const DRAWS: &str = "while :; do printf '.'; /bin/sleep 0.2; done";

    /// A stand-in for Claude Code that draws a screen once and then runs a command. Takes
    /// the screen as a `printf` format and the command. Returns the script.
    fn showing(screen: &str, then: &str) -> String {
        format!("#!/bin/sh\n/bin/stty raw -echo\nprintf '\\033[?2004h{screen}'\n{then}\n")
    }

    /// Claude Code's idle input prompt as a `printf` format: the prompt row between two
    /// rules and its footer. Takes nothing. Returns the format.
    fn idle_screen() -> String {
        format!("{RULE}\\r\\n> \\r\\n{RULE}\\r\\n  ? for shortcuts\\r\\n")
    }

    /// Waits until the session says its open turn is stale. Takes the pane. Panics when it
    /// does not within 20 seconds.
    fn stale(pane: &mut Pane) {
        let until = Instant::now() + Duration::from_secs(20);
        while !pane.session.turn_stale() {
            assert!(
                Instant::now() < until,
                "the open turn never counted as stale"
            );
            assert_eq!(
                pane.session.next_fact(Duration::from_millis(100)).unwrap(),
                None
            );
        }
    }

    /// OB-1: in a session that follows every turn, an open turn whose record gets no byte
    /// for the launch's `stale_turn`, on a pane whose app waits, is said stale once and is
    /// open no more. The model at work in it starts it again as a turn, which can go stale
    /// again and ends like any other; a turn that ended is never stale.
    #[test]
    fn an_open_turn_with_a_silent_record_on_a_waiting_pane_is_said_stale_once() {
        let mut pane = start_with("stale-turn-1", false, &showing(&idle_screen(), READS));
        pane.session.follow_every_turn();
        pane.session.limits.stale_turn = Duration::from_secs(4);
        assert!(!pane.session.turn_stale(), "no turn is open");
        pane.write(&[&user("go")]);
        assert_eq!(
            pane.session.next_fact(Duration::from_secs(5)).unwrap(),
            Some(TranscriptEvent::TurnStarted)
        );
        assert!(!pane.session.turn_stale(), "the record just grew");
        stale(&mut pane);
        assert!(!pane.session.turn_stale(), "a stale turn is said once");
        std::thread::sleep(Duration::from_secs(5));
        assert_eq!(
            pane.session.next_fact(Duration::from_millis(100)).unwrap(),
            None
        );
        assert!(!pane.session.turn_stale(), "a dropped turn is open no more");

        pane.write(&[TOOL]);
        assert_eq!(
            pane.session.next_fact(Duration::from_secs(5)).unwrap(),
            Some(TranscriptEvent::TurnStarted),
            "the model at work in the dropped turn starts it again"
        );
        assert!(
            !pane.session.turn_stale(),
            "a record that grew starts the wait again"
        );
        stale(&mut pane);

        pane.write(&[&reply("done")]);
        assert_eq!(
            pane.session.next_fact(Duration::from_secs(5)).unwrap(),
            Some(TranscriptEvent::TurnStarted)
        );
        assert_eq!(
            pane.session.next_fact(Duration::from_secs(5)).unwrap(),
            Some(text("done"))
        );
        assert_eq!(
            pane.session.next_fact(Duration::from_secs(5)).unwrap(),
            Some(DONE)
        );
        std::thread::sleep(Duration::from_secs(5));
        assert_eq!(
            pane.session.next_fact(Duration::from_millis(100)).unwrap(),
            None
        );
        assert!(!pane.session.turn_stale(), "a turn that ended is not stale");
        assert_eq!(pane.session.stop().unwrap(), None);
    }

    /// Owner 11-11, no output cap: a live session whose record grows far past the old 8 MiB,
    /// in one turn and again across turns, stays up: every fact of both turns arrives and no
    /// limit ends the pane.
    #[test]
    fn a_live_session_reading_far_past_8_mib_in_one_turn_and_across_turns_stays_up() {
        let mut pane = start("no-cap-1");
        pane.session.follow_every_turn();
        let said = "x".repeat(64 * 1024);
        let part = format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{said}"}}]}}}}"#
        );
        for turn in 0..2 {
            pane.write(&[&user("go")]);
            for _ in 0..160 {
                pane.write(&[&part]);
            }
            pane.write(&[&reply("done")]);
            let mut texts = 0;
            let until = Instant::now() + Duration::from_secs(60);
            loop {
                assert!(Instant::now() < until, "turn {turn} never ended");
                match pane.session.next_fact(Duration::from_secs(1)).unwrap() {
                    Some(TranscriptEvent::Text { .. }) => texts += 1,
                    Some(TranscriptEvent::TurnFinished { success }) => {
                        assert!(success);
                        break;
                    }
                    _ => {}
                }
            }
            assert_eq!(texts, 161, "turn {turn}");
        }
        assert!(pane.session.reader.bytes() > 20 * 1024 * 1024);
        assert_eq!(pane.session.stop().unwrap(), None);
    }

    /// Owner 11-11 item 5: a live master's pane drops the launch's run time, so the 24 h
    /// launch deadline never ends it; a pane that keeps the deadline still ends at it.
    #[test]
    fn a_live_pane_with_its_deadline_cleared_outlives_the_launch_run_time() {
        let mut kept = start("deadline-kept");
        kept.session.follow_every_turn();
        kept.session.deadline = Some(Instant::now() + Duration::from_millis(500));
        let ended = (0..40)
            .find_map(|_| kept.session.next_fact(Duration::from_millis(100)).err())
            .map(|problem| problem.0);
        assert_eq!(ended.as_deref(), Some("launch_deadline"));

        let mut live = start("deadline-cleared");
        live.session.follow_every_turn();
        live.session.deadline = Some(Instant::now() + Duration::from_millis(500));
        live.session.clear_deadline();
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until {
            assert_eq!(
                live.session.next_fact(Duration::from_millis(100)).unwrap(),
                None
            );
        }
        live.write(&[&user("go")]);
        assert_eq!(
            live.session.next_fact(Duration::from_secs(5)).unwrap(),
            Some(TranscriptEvent::TurnStarted)
        );
        assert_eq!(live.session.stop().unwrap(), None);
    }

    /// Checks that an open turn with a silent record never counts as stale on a pane that
    /// runs `script`, for more than twice the `stale_turn` it is given. Takes the agent name
    /// and the script.
    fn never_stale(name: &str, script: &str) {
        let mut pane = start_with(name, false, script);
        pane.session.follow_every_turn();
        pane.session.limits.stale_turn = Duration::from_secs(4);
        pane.write(&[&user("go")]);
        assert_eq!(
            pane.session.next_fact(Duration::from_secs(5)).unwrap(),
            Some(TranscriptEvent::TurnStarted)
        );
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
            assert!(!pane.session.turn_stale(), "the open turn was dropped");
            assert_eq!(
                pane.session.next_fact(Duration::from_millis(100)).unwrap(),
                None
            );
        }
    }

    /// OB-1 r1 (OB1-F2): a turn whose record is silent for longer than `stale_turn` is never
    /// stale while its pane still draws, as an app at work on a silent tool does, though the
    /// idle prompt is on its screen.
    #[test]
    fn an_open_turn_is_never_stale_while_its_pane_still_draws() {
        never_stale("stale-turn-2", &showing(&idle_screen(), DRAWS));
    }

    /// OB-1 r1 (OB1-F2): a turn is never stale while a marked numbered choice is on its
    /// still screen, though the idle prompt is on it too.
    #[test]
    fn an_open_turn_is_never_stale_while_a_question_is_on_its_screen() {
        let asked = format!(
            " Do you want to proceed?\\r\\n \u{276f} 1. Yes\\r\\n   2. No\\r\\n\\r\\n{}",
            idle_screen()
        );
        never_stale("stale-turn-3", &showing(&asked, READS));
    }

    /// OB1R2-F1: a turn with a silent record goes stale on a still screen whatever it shows
    /// besides a question: a dialog the engine cannot read, or no prompt at all. Its status
    /// must clear; holding engine input for such a screen is its caller's part.
    #[test]
    fn an_open_turn_goes_stale_on_a_still_screen_that_shows_no_idle_prompt() {
        let dialog =
            format!("{RULE}\\r\\n Tell the agent what to do differently:\\r\\n{RULE}\\r\\n");
        for (name, script) in [
            ("stale-turn-4", showing(&dialog, READS)),
            ("stale-turn-5", STAND_IN.to_owned()),
        ] {
            let mut pane = start_with(name, false, &script);
            pane.session.follow_every_turn();
            pane.session.limits.stale_turn = Duration::from_secs(4);
            pane.write(&[&user("go")]);
            assert_eq!(
                pane.session.next_fact(Duration::from_secs(5)).unwrap(),
                Some(TranscriptEvent::TurnStarted)
            );
            stale(&mut pane);
            assert!(
                !pane.session.took_none(),
                "{name}: the engine started this pane"
            );
        }
    }

    const BACKGROUND_USE: &str = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_bg","name":"Bash","input":{"command":"sleep 40","run_in_background":true}}],"stop_reason":"tool_use"}}"#;
    const BACKGROUND_STARTED: &str = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_bg","content":"Command running in background with ID: bgcheck1."}]},"toolUseResult":{"backgroundTaskId":"bgcheck1"}}"#;
    const BACKGROUND_NOTICE: &str = r#"{"type":"user","message":{"content":"<task-notification>\n<task-id>bgcheck1</task-id>\n<status>completed</status>\n</task-notification>"}}"#;

    /// N-E9 (OP-26): a turn the app starts by itself that ends while a command it started in
    /// the background still runs is held, as an engine-typed turn is: its end is not given,
    /// the app's turn on the command's end notification continues it, and its end is given
    /// once that turn ends. A later turn of its own with no such command is not held.
    #[test]
    fn an_own_turn_held_for_its_background_command_ends_with_its_notice_turn() {
        let mut pane = start("own-turns-4");
        pane.session.follow_own_turns();

        pane.write(&[
            &user("A teammate wrote"),
            BACKGROUND_USE,
            BACKGROUND_STARTED,
            &reply("Waiting for the check."),
        ]);
        assert_eq!(
            pane.session.next_event(Duration::from_millis(500)).unwrap(),
            None
        );
        assert_eq!(
            pane.own_facts(),
            vec![TranscriptEvent::TurnStarted, text("Waiting for the check.")],
            "the turn's end is held while its background command runs"
        );

        pane.write(&[BACKGROUND_NOTICE, &reply("Check passed.")]);
        assert_eq!(
            pane.session.next_event(Duration::from_millis(500)).unwrap(),
            None
        );
        assert_eq!(
            pane.own_facts(),
            vec![text("Check passed."), DONE],
            "the notice turn continues the held turn and ends it"
        );

        pane.write(&[&user("Another teammate wrote"), &reply("Own two")]);
        assert_eq!(
            pane.session.next_event(Duration::from_millis(500)).unwrap(),
            None
        );
        assert_eq!(
            pane.own_facts(),
            vec![TranscriptEvent::TurnStarted, text("Own two"), DONE]
        );
        assert_eq!(pane.session.stop().unwrap(), None);
    }

    const RATE_LIMITED: &str = r#"{"type":"assistant","isApiErrorMessage":true,"error":"rate_limit","message":{"content":[{"type":"text","text":"API Error"}]}}"#;

    /// Types a prompt into the pane and waits until the stand-in has read it. Takes the
    /// pane and the prompt.
    fn type_turn(pane: &mut Pane, prompt: &str) {
        pane.session.send(prompt).unwrap();
        let pasted = format!("\x1b[200~{prompt}\x1b[201~\r");
        let until = Instant::now() + Duration::from_secs(15);
        while !std::fs::read(pane.home.join("typed"))
            .unwrap_or_default()
            .ends_with(pasted.as_bytes())
        {
            assert!(Instant::now() < until, "the prompt was never typed");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Reads the session's events until none comes for half a second. Takes the pane.
    /// Returns the events.
    fn events(pane: &mut Pane) -> Vec<LaunchEvent> {
        let mut events = Vec::new();
        while let Some(event) = pane.session.next_event(Duration::from_millis(500)).unwrap() {
            events.push(event);
        }
        events
    }

    /// Whether the events hold a turn's end. Takes the events. Returns the answer.
    fn ended(events: &[LaunchEvent]) -> bool {
        events
            .iter()
            .any(|event| matches!(event, LaunchEvent::TurnFinished { .. }))
    }

    /// Starts an engine turn that ends held for its background command. Takes the pane.
    fn held_turn(pane: &mut Pane) {
        type_turn(pane, "Run the check.");
        pane.write(&[
            &user("Run the check."),
            BACKGROUND_USE,
            BACKGROUND_STARTED,
            &reply("Waiting for the check."),
        ]);
        let first = events(pane);
        assert!(!ended(&first), "{first:?}");
        assert!(pane.session.held(), "{first:?}");
    }

    /// HB-F2B: a message typed into an agent held for its background command starts a turn
    /// of its own. When that turn fails, the held task's work still stands: its failure and
    /// its rate limit are not given, the hold goes on, and the notice turn ends the task.
    #[test]
    fn a_turn_typed_over_a_hold_that_fails_goes_back_to_the_hold() {
        let mut pane = start("over-hold-1");
        held_turn(&mut pane);

        pane.write(&[&user("[crazyagent msg 1] Status?"), RATE_LIMITED]);
        let typed = events(&mut pane);
        assert!(!ended(&typed), "{typed:?}");
        assert!(
            !typed
                .iter()
                .any(|event| matches!(event, LaunchEvent::TurnError { .. })),
            "{typed:?}"
        );
        assert!(pane.session.held(), "the hold goes on");

        pane.write(&[BACKGROUND_NOTICE, &reply("Check passed.")]);
        let last = events(&mut pane);
        assert!(
            last.contains(&LaunchEvent::TurnFinished { success: true }),
            "{last:?}"
        );
        assert!(!pane.session.held());
        assert_eq!(pane.session.stop().unwrap(), None);
    }

    /// The owner's Interrupt of a turn typed over a hold still ends that turn and its task,
    /// though a background command runs.
    #[test]
    fn interrupting_a_turn_typed_over_a_hold_still_ends_it() {
        let mut pane = start("over-hold-2");
        held_turn(&mut pane);

        pane.write(&[&user("[crazyagent msg 1] Status?")]);
        let started = events(&mut pane);
        assert!(!ended(&started), "{started:?}");
        assert!(!pane.session.held());
        pane.session.interrupt().unwrap();
        pane.write(&[&user("[Request interrupted by user]")]);
        assert_eq!(
            events(&mut pane),
            vec![
                LaunchEvent::Interrupted,
                LaunchEvent::TurnFinished { success: false },
            ]
        );
        assert_eq!(pane.session.stop().unwrap(), None);
    }

    /// A turn that fails before it was ever held ends as it did, though a background
    /// command still runs: only a turn typed over a hold goes back to the hold.
    #[test]
    fn a_turn_that_fails_before_any_hold_still_ends() {
        let mut pane = start("over-hold-3");
        type_turn(&mut pane, "Run the check.");
        pane.write(&[
            &user("Run the check."),
            BACKGROUND_USE,
            BACKGROUND_STARTED,
            RATE_LIMITED,
        ]);
        let failed = events(&mut pane);
        assert!(
            failed.contains(&LaunchEvent::TurnError { rate_limited: true }),
            "{failed:?}"
        );
        assert!(
            failed.contains(&LaunchEvent::TurnFinished { success: false }),
            "{failed:?}"
        );
        assert_eq!(pane.session.stop().unwrap(), None);
    }

    /// HB-F2B (TEAM-14-F1): an interrupt pressed in the agent's own terminal, in a turn typed
    /// over a hold, is not given: the engine asked for none, the held work stands, and the
    /// notice turn ends the task.
    #[test]
    fn an_interrupted_line_in_a_turn_typed_over_a_hold_goes_back_to_the_hold() {
        let mut pane = start("over-hold-4");
        held_turn(&mut pane);

        pane.write(&[
            &user("[crazyagent msg 1] Status?"),
            &user("[Request interrupted by user]"),
        ]);
        let typed = events(&mut pane);
        assert!(!ended(&typed), "{typed:?}");
        assert!(!typed.contains(&LaunchEvent::Interrupted), "{typed:?}");
        assert!(pane.session.held(), "the hold goes on");

        pane.write(&[BACKGROUND_NOTICE, &reply("Check passed.")]);
        let last = events(&mut pane);
        assert!(
            last.contains(&LaunchEvent::TurnFinished { success: true }),
            "{last:?}"
        );
        assert_eq!(pane.session.stop().unwrap(), None);
    }

    /// HB-F2B (TEAM-14-L2): the held command can end inside a turn typed over the hold. When
    /// that turn then fails no notice turn is left to decide the task, so it ends as the
    /// held turn did: well, with no failure given.
    #[test]
    fn a_turn_typed_over_a_hold_that_fails_after_its_command_ended_ends_as_the_held_turn_did() {
        let mut pane = start("over-hold-5");
        held_turn(&mut pane);

        pane.write(&[
            &user("[crazyagent msg 1] Status?"),
            BACKGROUND_NOTICE,
            RATE_LIMITED,
        ]);
        assert_eq!(
            events(&mut pane),
            vec![LaunchEvent::TurnFinished { success: true }]
        );
        assert!(!pane.session.held());
        assert_eq!(pane.session.stop().unwrap(), None);
    }

    /// DOCS-LOWS-2-L1: an Esc pressed in the agent's own terminal, in a turn typed over the
    /// hold after the held command ended, is not given either. No notice turn is left, so the
    /// task ends as the held turn did: well, with no interrupt.
    #[test]
    fn an_interrupt_in_a_turn_typed_over_a_hold_after_its_command_ended_ends_as_the_held_turn_did()
    {
        let mut pane = start("over-hold-9");
        held_turn(&mut pane);

        pane.write(&[
            &user("[crazyagent msg 1] Status?"),
            BACKGROUND_NOTICE,
            &user("[Request interrupted by user]"),
        ]);
        assert_eq!(
            events(&mut pane),
            vec![LaunchEvent::TurnFinished { success: true }]
        );
        assert!(!pane.session.held());
        assert_eq!(pane.session.stop().unwrap(), None);
    }

    /// The notice turn starts once the last command ended, so it is no turn typed over the
    /// hold: it decides the task, and its failure and rate limit are given.
    #[test]
    fn a_notice_turn_that_fails_ends_its_task_as_failed() {
        let mut pane = start("over-hold-6");
        held_turn(&mut pane);

        pane.write(&[BACKGROUND_NOTICE, RATE_LIMITED]);
        let last = events(&mut pane);
        assert!(
            last.contains(&LaunchEvent::TurnError { rate_limited: true }),
            "{last:?}"
        );
        assert!(
            last.contains(&LaunchEvent::TurnFinished { success: false }),
            "{last:?}"
        );
        assert!(!pane.session.held());
        assert_eq!(pane.session.stop().unwrap(), None);
    }

    /// Leaves a turn held for two background commands, `bgcheck1` and `bgcheck2`. Input: the
    /// pane. Output: the second command's notice line.
    fn held_for_two(pane: &mut Pane) -> String {
        let second = |line: &str| {
            line.replace("toolu_bg", "toolu_bg2")
                .replace("bgcheck1", "bgcheck2")
        };
        type_turn(pane, "Run the checks.");
        pane.write(&[
            &user("Run the checks."),
            BACKGROUND_USE,
            BACKGROUND_STARTED,
            &second(BACKGROUND_USE),
            &second(BACKGROUND_STARTED),
            &reply("Waiting for the checks."),
        ]);
        let first = events(pane);
        assert!(!ended(&first), "{first:?}");
        assert!(pane.session.held(), "{first:?}");
        second(BACKGROUND_NOTICE)
    }

    /// DOCS-LOWS-2-F1: a turn that follows a command's notice is a notice turn even while a
    /// second command runs, so when the second ends inside it and it fails, the task fails
    /// and its rate limit is given.
    #[test]
    fn a_notice_turn_that_fails_after_the_second_command_ended_in_it_ends_its_task_as_failed() {
        let mut pane = start("over-hold-7");
        let second = held_for_two(&mut pane);

        pane.write(&[BACKGROUND_NOTICE, &second, RATE_LIMITED]);
        let last = events(&mut pane);
        assert!(
            last.contains(&LaunchEvent::TurnError { rate_limited: true }),
            "{last:?}"
        );
        assert!(
            last.contains(&LaunchEvent::TurnFinished { success: false }),
            "{last:?}"
        );
        assert!(!pane.session.held());
        assert_eq!(pane.session.stop().unwrap(), None);
    }

    /// A notice turn that fails while another command still runs is no turn typed over the
    /// hold either: its failure is given and it ends the task.
    #[test]
    fn a_notice_turn_that_fails_while_a_second_command_runs_ends_its_task_as_failed() {
        let mut pane = start("over-hold-8");
        held_for_two(&mut pane);

        pane.write(&[BACKGROUND_NOTICE, RATE_LIMITED]);
        let last = events(&mut pane);
        assert!(
            last.contains(&LaunchEvent::TurnError { rate_limited: true }),
            "{last:?}"
        );
        assert!(
            last.contains(&LaunchEvent::TurnFinished { success: false }),
            "{last:?}"
        );
        assert!(!pane.session.held());
    }
}

/// Makes a real FIFO at `path` with the system `mkfifo`. Input: the path. Output: none.
#[cfg(unix)]
fn fifo(path: &std::path::Path) {
    let made = crate::spawn_gate::status(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(path)
            .env_clear()
            .stdin(std::process::Stdio::null()),
    )
    .unwrap();
    assert!(made.success());
}

/// Opens the FIFO at `path` for writing without waiting and closes it, so a reader a
/// failing test left blocked on it ends. Input: the path. Output: none.
#[cfg(unix)]
fn release_fifo(path: &std::path::Path) {
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
        .open(path);
}

/// A private folder for one key file test: under the canonical `/tmp` with mode 0700 on
/// unix, in the system temp folder elsewhere.
fn key_folder() -> tempfile::TempDir {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let folder = tempfile::Builder::new()
            .prefix("ca-key-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        folder
    }
    #[cfg(not(unix))]
    {
        tempfile::Builder::new()
            .prefix("ca-key-")
            .tempdir()
            .unwrap()
    }
}

/// The longest key the engine writes: a relay token.
fn longest_key() -> String {
    format!(
        "{}{}",
        crate::model_gateway::relay::TOKEN_PREFIX,
        "a".repeat(64)
    )
}

/// WINB-L1 with rv-secreview F1: off macOS no agent pane runs, whatever its unconfined flag,
/// since that flag is also set for Full access agents; each refusal names the system.
#[test]
fn every_agent_pane_is_refused_off_macos_with_a_code_naming_the_system() {
    assert_eq!(
        agent_pane_unavailable("windows"),
        Some("launch_unavailable_on_windows")
    );
    assert_eq!(
        agent_pane_unavailable("linux"),
        Some("launch_unavailable_on_linux")
    );
    assert_eq!(
        agent_pane_unavailable("freebsd"),
        Some("launch_unavailable")
    );
    assert_eq!(agent_pane_unavailable("macos"), None);
}

#[test]
fn dropping_a_key_file_that_still_holds_its_key_removes_it() {
    let folder = key_folder();
    let path = folder.path().join(interactive::KEY_FILE);
    std::fs::write(&path, longest_key()).unwrap();
    drop(KeyFile::new(path.clone(), longest_key().as_bytes()));
    assert!(
        !path.exists(),
        "a key file that still held its key was kept"
    );
}

#[test]
fn dropping_a_key_file_whose_path_now_holds_another_key_keeps_it() {
    let folder = key_folder();
    let path = folder.path().join(interactive::KEY_FILE);
    let other = format!("{}b", &longest_key()[..longest_key().len() - 1]);
    std::fs::write(&path, &other).unwrap();
    drop(KeyFile::new(path.clone(), longest_key().as_bytes()));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), other);
}

#[cfg(unix)]
#[test]
fn dropping_a_key_file_whose_path_now_holds_a_fifo_keeps_it_and_never_blocks() {
    let folder = key_folder();
    let path = folder.path().join(interactive::KEY_FILE);
    fifo(&path);
    let key = KeyFile::new(path.clone(), longest_key().as_bytes());
    let (sent, got) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(key);
        let _ = sent.send(());
    });
    let dropped = got.recv_timeout(Duration::from_secs(5));
    release_fifo(&path);
    assert_eq!(
        dropped,
        Ok(()),
        "dropping a key file whose path holds a FIFO blocked for 5 s"
    );
    assert!(
        std::os::unix::fs::FileTypeExt::is_fifo(
            &std::fs::symlink_metadata(&path).unwrap().file_type()
        ),
        "the FIFO at the key path was removed"
    );
}

#[test]
#[should_panic(expected = "assertion failed: content.len() as u64 <= KEY_FILE_BYTES_MAX")]
fn a_key_file_guard_never_holds_more_bytes_than_a_key_file_is_read_back_with() {
    let folder = key_folder();
    let longer = format!("{}a", longest_key());
    let _ = KeyFile::new(folder.path().join(interactive::KEY_FILE), longer.as_bytes());
}

#[test]
fn dropping_a_key_file_whose_path_now_holds_more_than_a_key_keeps_it() {
    let folder = key_folder();
    let path = folder.path().join(interactive::KEY_FILE);
    std::fs::File::create(&path)
        .unwrap()
        .set_len(16 << 20)
        .unwrap();
    let started = Instant::now();
    drop(KeyFile::new(path.clone(), longest_key().as_bytes()));
    let took = started.elapsed();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 16 << 20);
    assert!(took < Duration::from_secs(1), "the drop took {took:?}");
}
