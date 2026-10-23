use super::*;
use super::super::claude_code::Input;
use crate::engine::owner::{ClaudeAuthChallenge, ClaudeAuthInput};

const UNIT_URL: &str =
    "https://claude.com/cai/oauth/authorize?state=synthetic%2Bstate&code_challenge=unit";
const UNIT_CODE: &str = "SYNTHETIC-CODE#SYNTHETIC-STATE";
const UNIT_GENERATION: u64 = 7;

fn unit_query() -> NativeAuthQuery {
    NativeAuthQuery {
        request_id: request("unit-claude-code-submission"),
        operation_id: request("unit-claude-original-operation"),
        scope: NativeAuthScope {
            expected_database_instance: "a".repeat(64),
            connection: reference("unit-claude-connection"),
            expected_config_id: ConfigId::new("unit-claude-config").unwrap(),
            expected_source_hash: "b".repeat(64),
        },
    }
}

fn unit_challenge(accepts_code: bool) -> ClaudeAuthChallenge {
    ClaudeAuthChallenge::new(UNIT_URL.into(), accepts_code).unwrap()
}

fn unit_input() -> Input {
    let mut input = Input::default();
    input
        .observe(unit_query(), UNIT_GENERATION, unit_challenge(true))
        .unwrap();
    input
}

fn type_code(input: &mut Input) {
    for character in UNIT_CODE.chars() {
        assert!(input.reduce(&UiEvent::Character(character)).unwrap());
    }
}

#[test]
fn unit_claude_browser_url_does_not_enable_an_unobserved_code_prompt() {
    let mut input = Input::default();
    assert!(!input.reduce(&UiEvent::Character('x')).unwrap());
    assert!(!input.reduce(&UiEvent::Submit).unwrap());
    assert!(input.request().is_none());
    assert!(!input.render().contains("[hidden input]"));

    let challenge = unit_challenge(false);
    assert_eq!(challenge.url(), UNIT_URL);
    assert!(!challenge.accepts_code());
    assert_eq!(challenge.prompt_id(), None);
    input
        .observe(unit_query(), UNIT_GENERATION, challenge)
        .unwrap();
    assert!(input.render().contains(UNIT_URL));
    assert!(!input.render().contains("Claude fallback code"));
    assert!(!input.reduce(&UiEvent::Character('x')).unwrap());
    assert!(!input.reduce(&UiEvent::Submit).unwrap());
    assert!(input.request().is_none());

    input
        .observe(unit_query(), UNIT_GENERATION, unit_challenge(true))
        .unwrap();
    assert!(input.render().contains("Claude fallback code"));
    assert!(input.render().contains("[empty]"));
    let _ = input.reduce(&UiEvent::Submit);
    assert!(input.request().is_none());
    let mut invalid = Input::default();
    assert!(invalid.observe(unit_query(), 0, unit_challenge(true)).is_err());
    assert!(invalid.request().is_none());
    assert!(ClaudeAuthChallenge::new("https://console.anthropic.com/oauth".into(), true).is_err());
}

#[test]
fn unit_claude_code_moves_once_with_the_original_operation_and_never_replays() {
    let mut input = unit_input();
    type_code(&mut input);
    let rendered = input.render();
    assert!(rendered.contains(UNIT_URL));
    assert!(rendered.contains("[hidden input]"));
    assert!(!rendered.contains(UNIT_CODE));
    assert!(!rendered.contains("SYNTHETIC-CODE"));
    assert!(!rendered.contains("SYNTHETIC-STATE"));

    input.reduce(&UiEvent::Submit).unwrap();
    let _ = input.reduce(&UiEvent::Submit);
    let submission = input.request().expect("one move-only code submission");
    let request: ClaudeAuthInput = submission.request.clone();
    assert_eq!(request.query, unit_query());
    assert_eq!(request.generation, UNIT_GENERATION);
    assert_eq!(request.prompt_id, unit_challenge(true).prompt_id().unwrap());
    assert_eq!(
        format!("{:?}", submission.secret),
        "AccountSecret([REDACTED])"
    );
    let metadata = serde_json::to_string(&request).unwrap();
    assert!(!metadata.contains(UNIT_CODE));
    assert!(!metadata.contains("SYNTHETIC-CODE"));
    assert!(!metadata.contains("SYNTHETIC-STATE"));
    assert!(input.request().is_none());
    drop(submission);

    input.complete(
        &request,
        Err("native_auth_claude_input_outcome_unknown".into()),
    );
    let mut reobserved = unit_query();
    reobserved.request_id = super::request("unit-claude-later-read");
    input
        .observe(reobserved, UNIT_GENERATION, unit_challenge(true))
        .unwrap();
    let _ = input.reduce(&UiEvent::Character('x'));
    let _ = input.reduce(&UiEvent::Submit);
    assert!(input.request().is_none());
    assert!(!input.render().contains(UNIT_CODE));
    assert!(!input.render().contains("[hidden input]"));
}

#[test]
fn unit_escape_focus_and_clear_dispose_code_without_replaying_a_queued_secret() {
    for event in [UiEvent::Cancel, UiEvent::FocusNext, UiEvent::FocusPrevious] {
        let mut input = unit_input();
        type_code(&mut input);
        input.reduce(&event).unwrap();
        assert!(input.request().is_none());
        assert!(input.render().is_empty());
        input
            .observe(unit_query(), UNIT_GENERATION, unit_challenge(true))
            .unwrap();
        assert!(input.render().contains("[empty]"));
        let _ = input.reduce(&UiEvent::Submit);
        assert!(input.request().is_none());
    }

    let mut input = unit_input();
    type_code(&mut input);
    input.clear();
    assert!(input.request().is_none());
    assert!(input.render().is_empty());
    input
        .observe(unit_query(), UNIT_GENERATION, unit_challenge(true))
        .unwrap();
    assert!(input.render().contains("[empty]"));
    let _ = input.reduce(&UiEvent::Submit);
    assert!(input.request().is_none());
    type_code(&mut input);
    input.reduce(&UiEvent::Submit).unwrap();
    input.clear();
    assert!(input.request().is_none());
    assert!(input.render().is_empty());
    input
        .observe(unit_query(), UNIT_GENERATION, unit_challenge(true))
        .unwrap();
    let _ = input.reduce(&UiEvent::Submit);
    assert!(input.request().is_none());
    assert!(!input.render().contains(UNIT_CODE));
}

#[tokio::test]
async fn real_unobserved_claude_panel_preserves_the_draft_and_codex_device_route() {
    run(|case| async move {
        let mut ui = Ui::new(&case).await;
        ui.app.reduce(UiEvent::FocusPrevious).unwrap();
        ui.app.reduce(UiEvent::FocusPrevious).unwrap();
        for character in "retained brief outside Claude input".chars() {
            ui.app.reduce(UiEvent::Character(character)).unwrap();
        }
        ui.app.reduce(UiEvent::FocusNext).unwrap();
        ui.app.reduce(UiEvent::FocusNext).unwrap();
        ui.connections(1).await;
        let before = case.counts();
        ui.key(KeyCode::F(2));
        let inspected = ui.auth().await;
        assert!(matches!(&inspected.result, Ok(Output::Inspected(_))));
        ui.apply(inspected);
        assert!(ui.render(120, 40).contains("Claude official sign-in"));
        assert!(!ui.render(120, 40).contains("Claude fallback code"));
        ui.key(KeyCode::Char('x'));
        ui.key(KeyCode::Backspace);
        assert!(ui.app.model_access_request().unwrap().is_none());
        assert_eq!(ui.app.draft(), "retained brief outside Claude input");
        ui.key(KeyCode::Esc);
        assert_eq!(case.counts(), before);

        let mut codex = Ui::new(&case).await;
        codex.connections(0).await;
        codex.key(KeyCode::F(2));
        let inspected = codex.auth().await;
        assert_eq!(inspected.job.context.connection.id.as_str(), "a-codex");
        assert!(matches!(&inspected.result, Ok(Output::Inspected(_))));
        codex.apply(inspected);
        let rendered = codex.render(120, 40);
        assert!(rendered.contains("Codex browser sign-in"));
        assert!(rendered.contains("Codex device-code sign-in"));
        assert!(!rendered.contains("Claude fallback code"));
        codex.key(KeyCode::Down);
        codex.key(KeyCode::Enter);
        let model_access::Job::NativeAuth(job) =
            codex.app.model_access_request().unwrap().unwrap()
        else {
            panic!("existing Codex route must retain its native-auth job");
        };
        assert!(matches!(
            &job.command,
            Command::Start(NativeAuthStart {
                method: NativeAuthMethod::CodexDeviceCode,
                ..
            })
        ));
        drop(job);
        assert_eq!(case.counts(), before);
        assert_eq!(ui.app.draft(), "retained brief outside Claude input");
    })
    .await;
}
