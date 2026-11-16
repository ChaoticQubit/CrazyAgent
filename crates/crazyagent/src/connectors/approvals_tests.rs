use super::*;
use crate::connectors::tests::{Case, case};
use crate::notifications::{NoticeFilter, NoticeQuery, PAGE_MAX};
use serde_json::json;

const PROJECT: &str = "alpha";
const OTHER: &str = "beta";
const LAUNCH: &str = "0123456789abcdef0123456789abcdef";
const NOW: i64 = 1_763_200_000_000;
const MINUTE: i64 = 60 * 1000;
const KEY: &str = "sk-live-0123456789abcdef";

fn write(launch: &str, project: &str, connector: &str) -> PendingWrite {
    PendingWrite {
        launch: launch.to_owned(),
        project: project.to_owned(),
        agent: "Agent 1".to_owned(),
        connector: connector.to_owned(),
        title: "Board".to_owned(),
        tool: "create_issue".to_owned(),
        preview: "create_issue: title=\"Fix login\"".to_owned(),
        arguments: "{\"title\":\"Fix login\"}".to_owned(),
        hidden: false,
    }
}

/// One write of `board` in `alpha` that waits ten minutes from `NOW`. Output: its id.
fn asked(case: &Case) -> i64 {
    ask(
        &case.db,
        &write(LAUNCH, PROJECT, "board"),
        Wait::Minutes10,
        NOW,
    )
    .unwrap()
}

/// The owner's pending notices after one reconcile: each one's kind name, text and
/// target as the clients read it.
fn pending(case: &mut Case) -> Vec<(String, String, Value)> {
    crate::notifications::initialize(&case.db).unwrap();
    crate::notifications::reconcile::reconcile(
        &mut case.db,
        &crate::config::ModelAccess::default(),
        crate::semantic::now_ms(),
    )
    .unwrap();
    crate::notifications::page(
        &case.db,
        &NoticeQuery {
            filter: NoticeFilter::Pending,
            project_id: None,
            kind: None,
            fold: false,
            before: None,
            after: None,
            limit: PAGE_MAX,
        },
    )
    .unwrap()
    .items
    .into_iter()
    .map(|item| {
        (
            item.kind.name().to_owned(),
            item.text,
            serde_json::to_value(&item.target).unwrap(),
        )
    })
    .collect()
}

/// DEC-168 Task 12: the preview names the tool and the argument keys with short values;
/// a value holding the connector's key, a long text, hidden and control characters never
/// reach it, and it never passes 480 bytes, however many or how wide the arguments are.
#[test]
fn the_preview_never_holds_a_secret_or_more_than_480_bytes() {
    let secret = SecretString::new(KEY.as_bytes().to_vec()).unwrap();
    let mut arguments = Map::new();
    arguments.insert("title".into(), json!("Fix the login page"));
    arguments.insert("token".into(), json!(format!("Bearer {KEY}")));
    arguments.insert("body".into(), json!("x".repeat(5000)));
    arguments.insert("count".into(), json!(3));
    arguments.insert("labels".into(), json!(["a", "b"]));
    arguments.insert("note\u{200b}".into(), json!("hidden\u{202e}text\u{7}"));
    let shown = preview("create_issue", &arguments, &[secret]);
    assert!(shown.len() <= PREVIEW_BYTES_MAX, "{}", shown.len());
    assert!(shown.starts_with("create_issue"), "{shown}");
    assert!(shown.contains("title"), "{shown}");
    assert!(shown.contains("Fix the login page"), "{shown}");
    assert!(shown.contains("token"), "{shown}");
    assert!(!shown.contains(KEY), "{shown}");
    assert!(!shown.contains(&"x".repeat(100)), "{shown}");
    let hidden = ['\u{200b}', '\u{202e}', '\u{7}'];
    assert!(
        !shown.chars().any(|found| hidden.contains(&found)),
        "{shown}"
    );

    let many: Map<String, Value> = (0..200)
        .map(|n| (format!("key{n:03}"), json!("value")))
        .collect();
    let shown = preview("create_issue", &many, &[]);
    assert!(shown.len() <= PREVIEW_BYTES_MAX, "{}", shown.len());
    assert!(shown.starts_with("create_issue"), "{shown}");
    let wide: Map<String, Value> = (0..100)
        .map(|n| (format!("k{n}"), json!("é".repeat(10))))
        .collect();
    let shown = preview("t", &wide, &[]);
    assert!(shown.len() <= PREVIEW_BYTES_MAX, "{}", shown.len());
    assert!(shown.starts_with('t'), "{shown}");
}

/// DEC-168 addendum 14 decision 6: an approval is answered once; a second answer and an
/// Allow after its wait ran out change nothing and leave no standing allow, and an id
/// that names no approval is refused.
#[test]
fn a_second_answer_and_an_allow_after_expiry_change_nothing() {
    let case = case();
    let id = asked(&case);
    assert_eq!(state(&case.db, id).unwrap(), Some(State::Waiting));
    answer(
        &case.db,
        id,
        Answer::Deny,
        WritePolicy::AskEveryTime,
        NOW + MINUTE,
    )
    .unwrap();
    assert_eq!(state(&case.db, id).unwrap(), Some(State::Denied));
    let again = answer(
        &case.db,
        id,
        Answer::Allow,
        WritePolicy::AskEveryTime,
        NOW + 2 * MINUTE,
    );
    assert_eq!(again.unwrap_err().0, "connector_approval_closed");
    assert_eq!(state(&case.db, id).unwrap(), Some(State::Denied));

    let late = asked(&case);
    assert_eq!(expire(&case.db, NOW + 10 * MINUTE).unwrap(), 1);
    assert_eq!(state(&case.db, late).unwrap(), Some(State::Expired));
    let allowed = answer(
        &case.db,
        late,
        Answer::Allow,
        WritePolicy::AskOncePerProject,
        NOW + 11 * MINUTE,
    );
    assert_eq!(allowed.unwrap_err().0, "connector_approval_closed");
    assert_eq!(state(&case.db, late).unwrap(), Some(State::Expired));
    assert!(!writes_allowed(&case.db, PROJECT, "board").unwrap());
    let unknown = answer(
        &case.db,
        late + 100,
        Answer::Allow,
        WritePolicy::AskEveryTime,
        NOW,
    );
    assert_eq!(unknown.unwrap_err().0, "connector_approval_unknown");
    assert_eq!(state(&case.db, late + 100).unwrap(), None);
}

/// DEC-168 Task 12: an unanswered write expires exactly at the wait setting it was asked
/// under, for every option of that setting, and its answer time is that moment.
#[test]
fn an_unanswered_write_expires_at_the_wait_setting() {
    let case = case();
    for wait in Wait::ALL {
        let id = ask(&case.db, &write(LAUNCH, PROJECT, "board"), wait, NOW).unwrap();
        let view = get(&case.db, id).unwrap().unwrap();
        assert_eq!(view.created_ms, NOW);
        assert_eq!(view.expires_ms, NOW + i64::from(wait.minutes()) * MINUTE);
        assert_eq!(expire(&case.db, view.expires_ms - 1).unwrap(), 0);
        assert_eq!(state(&case.db, id).unwrap(), Some(State::Waiting));
        assert_eq!(expire(&case.db, view.expires_ms).unwrap(), 1);
        let view = get(&case.db, id).unwrap().unwrap();
        assert_eq!(view.state, State::Expired);
        assert_eq!(view.answered_ms, Some(view.expires_ms));
    }
}

/// DEC-168 addendum 14 decision 6: at most `WAITING_PER_LAUNCH_MAX` writes of one launch
/// and `WAITING_MAX` in all wait at once, so an agent cannot flood the notification
/// center; an answer frees a place.
#[test]
fn waiting_approvals_are_capped_per_launch_and_in_all() {
    let case = case();
    let ids: Vec<i64> = (0..WAITING_PER_LAUNCH_MAX).map(|_| asked(&case)).collect();
    let full = ask(
        &case.db,
        &write(LAUNCH, PROJECT, "board"),
        Wait::Minutes10,
        NOW,
    );
    assert_eq!(full.unwrap_err().0, "connector_writes_waiting_full");
    answer(
        &case.db,
        ids[0],
        Answer::Deny,
        WritePolicy::AskEveryTime,
        NOW,
    )
    .unwrap();
    asked(&case);
    assert_eq!(
        expire_waiting(&case.db, NOW).unwrap(),
        WAITING_PER_LAUNCH_MAX
    );

    for n in 0..WAITING_MAX {
        let launch = format!("{n:032x}");
        ask(
            &case.db,
            &write(&launch, PROJECT, "board"),
            Wait::Minutes10,
            NOW,
        )
        .unwrap();
    }
    let over = format!("{WAITING_MAX:032x}");
    let full = ask(
        &case.db,
        &write(&over, PROJECT, "board"),
        Wait::Minutes10,
        NOW,
    );
    assert_eq!(full.unwrap_err().0, "connector_writes_waiting_full");
    let listed = waiting(&case.db, WAITING_MAX + 1).unwrap().unwrap();
    assert_eq!(listed.len(), WAITING_MAX);
}

/// DEC-168 Task 12: a waiting write raises one Action notice in its project that opens
/// its approval page and names only the connector, the tool and the project, never an
/// argument; answering it resolves the notice.
#[test]
fn answering_resolves_the_action_notice() {
    let mut case = case();
    let id = asked(&case);
    let name: Option<String> = case
        .db
        .query_row(
            "SELECT json_extract(body, '$.spec.name') FROM projects WHERE id=?1",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap();
    let name = name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| PROJECT.to_owned());
    let notices = pending(&mut case);
    assert_eq!(notices.len(), 1, "{notices:?}");
    let (kind, text, target) = &notices[0];
    assert_eq!(kind, "connector_write");
    let said = format!("Board wants to run create_issue in {name}.");
    assert_eq!(text, &said);
    assert!(!text.contains("Fix login"), "{text}");
    assert_eq!(
        target,
        &json!({"page": "connector_approval", "project_id": PROJECT, "id": id})
    );
    answer(
        &case.db,
        id,
        Answer::Allow,
        WritePolicy::AskEveryTime,
        NOW + MINUTE,
    )
    .unwrap();
    assert!(pending(&mut case).is_empty());
}

/// DEC-168 Task 12: an Allow under Ask once per project and connector leaves a standing
/// allow for that project and connector only; an Allow under Ask every time and a Deny
/// leave none, and Revoke takes it back.
#[test]
fn ask_once_records_an_allowed_write_per_project_and_revoke_removes_it() {
    let case = case();
    let id = asked(&case);
    answer(&case.db, id, Answer::Allow, WritePolicy::AskEveryTime, NOW).unwrap();
    assert!(
        !writes_allowed(&case.db, PROJECT, "board").unwrap(),
        "Ask every time keeps no standing allow"
    );
    let id = asked(&case);
    answer(
        &case.db,
        id,
        Answer::Allow,
        WritePolicy::AskOncePerProject,
        NOW,
    )
    .unwrap();
    assert!(writes_allowed(&case.db, PROJECT, "board").unwrap());
    assert!(!writes_allowed(&case.db, OTHER, "board").unwrap());
    assert!(!writes_allowed(&case.db, PROJECT, "desk").unwrap());
    assert_eq!(
        allowed_writes(&case.db, PROJECT).unwrap(),
        AllowedWrites {
            project: PROJECT.to_owned(),
            connectors: vec!["board".to_owned()],
        }
    );
    let other = ask(
        &case.db,
        &write(LAUNCH, OTHER, "board"),
        Wait::Minutes10,
        NOW,
    )
    .unwrap();
    answer(
        &case.db,
        other,
        Answer::Deny,
        WritePolicy::AskOncePerProject,
        NOW,
    )
    .unwrap();
    assert!(
        !writes_allowed(&case.db, OTHER, "board").unwrap(),
        "a Deny keeps no standing allow"
    );
    assert!(revoke_writes(&case.db, PROJECT, "board").unwrap());
    assert!(!revoke_writes(&case.db, PROJECT, "board").unwrap());
    assert!(!writes_allowed(&case.db, PROJECT, "board").unwrap());
    let left = allowed_writes(&case.db, PROJECT).unwrap();
    assert!(left.connectors.is_empty());
}

/// DEC-168 Task 12, sec r1 F2: the owner reads every argument a write would send, nested
/// values and long texts whole, hidden characters escaped and any value holding one of the
/// connector's secret forms withheld; arguments past the bound are refused.
#[test]
fn the_full_arguments_show_every_value_and_withhold_secret_forms() {
    let secret = SecretString::new(KEY.as_bytes().to_vec()).unwrap();
    let long = "Notes for the team ".repeat(20);
    let fields = json!({
        "title": "Weekly\u{200b} summary",
        "description": long,
        "extra": {"note": format!("key {KEY}"), "items": [1, 2, 3]},
    });
    let fields = fields.as_object().unwrap().clone();
    let text = arguments(&fields, std::slice::from_ref(&secret)).unwrap();
    assert!(!text.is_empty(), "the arguments are shown");
    assert!(!text.contains(KEY), "{text}");
    let shown: Value = serde_json::from_str(&text).unwrap_or_else(|_| panic!("{text:?}"));
    assert_eq!(shown["title"], json!("Weekly\\u{200B} summary"));
    assert_eq!(shown["description"], json!(long));
    assert_eq!(shown["extra"]["items"], json!([1, 2, 3]));
    assert_eq!(shown["extra"]["note"], json!("[withheld]"));
    let huge = json!({"body": "x".repeat(ARGUMENTS_BYTES_MAX)});
    assert_eq!(arguments(huge.as_object().unwrap(), &[]), None);
}

/// DEC-168 Task 12, sec r2 F2: the full arguments leave out no character the vendor gets. A
/// tag-block payload, a zero-width joiner, a variation selector and a carriage return each
/// show as a `\u{XXXX}` escape, in names too, so two names stay two; line breaks and tabs
/// stay as written, and the write is marked as holding hidden characters.
#[test]
fn the_full_arguments_show_every_hidden_character_escaped_and_mark_the_write() {
    let fields = json!({
        "body": "Looks good to me\u{e0041}\u{e0042}",
        "note": "line one\r\nline two\tend",
        "note\u{200d}": "ok \u{1f44d}\u{fe0f}",
    });
    let fields = fields.as_object().unwrap().clone();
    let text = arguments(&fields, &[]).unwrap();
    let shown: Value = serde_json::from_str(&text).unwrap_or_else(|_| panic!("{text:?}"));
    let body = json!("Looks good to me\\u{E0041}\\u{E0042}");
    assert_eq!(shown["body"], body, "{text}");
    assert_eq!(shown["note"], json!("line one\\u{000D}\nline two\tend"));
    assert_eq!(shown["note\\u{200D}"], json!("ok \u{1f44d}\\u{FE0F}"));
    assert_eq!(shown.as_object().unwrap().len(), 3, "{text}");
    assert!(hidden(&fields));
    let ordinary = json!({"title": "Fix login", "body": "two\nlines\tand a tab"});
    assert!(!hidden(ordinary.as_object().unwrap()));
    let tagged = json!({"extra": [{"sign\u{e0041}": "x"}]});
    assert!(hidden(tagged.as_object().unwrap()));
}

/// DEC-168 Task 12, sec r1 F2: a waiting approval keeps the write's full arguments for the
/// owner's page, and drops them once it is answered or ends.
#[test]
fn a_waiting_approval_shows_its_full_arguments_until_it_is_answered() {
    let case = case();
    let id = asked(&case);
    let view = get(&case.db, id).unwrap().unwrap();
    assert_eq!(view.arguments, "{\"title\":\"Fix login\"}");
    let policy = WritePolicy::AskEveryTime;
    answer(&case.db, id, Answer::Deny, policy, NOW + MINUTE).unwrap();
    assert_eq!(get(&case.db, id).unwrap().unwrap().arguments, "");
    let later = asked(&case);
    expire(&case.db, NOW + 11 * MINUTE).unwrap();
    assert_eq!(get(&case.db, later).unwrap().unwrap().arguments, "");
}
