use super::*;
use crate::learning::LearningProgress;
use crate::owner_profile::OwnerProfile;
use crate::tui::client::learning::{LearningEvent, LearningReply};
use crate::tui::screens::settings_accounts::tests::{ctx, render};
use crate::tui::screens::{SettingsSection, open};
use crate::tui::shell::Shell;

fn progress(skipped: bool) -> LearningProgress {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    let tx = db.transaction().unwrap();
    let mut progress = crate::learning::progress(&tx).unwrap();
    progress.skipped = skipped;
    progress
}

fn profile_page(width: u16, height: u16, learning: Load<LearningProgress>) -> Shell {
    let mut ctx = ctx(width, height);
    ctx.store.profile = Load::Ready(Some(OwnerProfile {
        name: "Mira Okafor".into(),
        role: "Founder".into(),
        bio: String::new(),
        preferred_stack: Vec::new(),
        working_style: Default::default(),
    }));
    ctx.store.learning = learning;
    let screen = open(&Place::Settings(SettingsSection::Profile), &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    learning_requests(&mut shell);
    shell
}

fn learning_requests(shell: &mut Shell) -> Vec<LearningQuery> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Learning(query) => Some(*query),
            _ => None,
        })
        .collect()
}

fn clears(shell: &mut Shell) -> Vec<u64> {
    learning_requests(shell)
        .into_iter()
        .map(|query| match query {
            LearningQuery::Mark {
                step: MarkStep::Skipped,
                on: false,
                generation,
            } => generation,
            other => panic!("a replay sends only the clear of the skip: {other:?}"),
        })
        .collect()
}

fn answer(generation: u64, result: Result<LearningReply, String>) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Learning(Box::new(LearningEvent {
        query: LearningQuery::Mark {
            step: MarkStep::Skipped,
            on: false,
            generation,
        },
        result,
    }))))
}

fn shows(rows: &[String], text: &str) -> bool {
    rows.iter().any(|row| row.contains(text))
}

fn profile() -> Option<Place> {
    Some(Place::Settings(SettingsSection::Profile))
}

#[test]
fn profile_shows_both_entries_under_the_form_at_120x36_and_in_the_keys_overlay() {
    let mut shell = profile_page(120, 36, Load::Ready(progress(true)));
    let rows = render(&mut shell, 120, 36);
    assert!(shows(&rows, "[ Save changes ]"), "{rows:#?}");
    assert!(shows(&rows, TITLE), "{rows:#?}");
    assert!(shows(&rows, BODY), "{rows:#?}");
    assert!(
        shows(&rows, "g  Show the learning guide again"),
        "{rows:#?}"
    );
    assert!(shows(&rows, "f  Run first-run setup again"), "{rows:#?}");
    let footer = rows.last().unwrap();
    assert!(
        !footer.contains("again"),
        "the help line keeps its keys: {footer}"
    );
    shell.key(Key::Char('?'));
    let rows = render(&mut shell, 120, 36);
    assert!(shows(&rows, GUIDE_KEY), "{rows:#?}");
    assert!(shows(&rows, FIRST_RUN_KEY), "{rows:#?}");
    assert!(
        !shows(&rows, "again…"),
        "both fit the Keys column: {rows:#?}"
    );

    let mut older = profile_page(120, 36, Load::NotReady);
    let rows = render(&mut older, 120, 36);
    assert!(
        !shows(&rows, TITLE),
        "no section without the learning read: {rows:#?}"
    );
}

#[test]
fn show_the_learning_guide_again_clears_the_skip_and_opens_home() {
    let mut shell = profile_page(120, 36, Load::Ready(progress(true)));
    shell.key(Key::Char('g'));
    let sent = clears(&mut shell);
    assert_eq!(sent.len(), 1, "{sent:?}");
    shell.key(Key::Char('g'));
    shell.key(Key::Char('f'));
    assert!(clears(&mut shell).is_empty(), "one clear at a time");
    assert_eq!(shell.place(), profile());

    shell.arrive(answer(sent[0] + 1, Ok(LearningReply::Marked)));
    assert_eq!(
        shell.place(),
        profile(),
        "another clear's answer is not its own"
    );
    shell.arrive(answer(sent[0], Ok(LearningReply::Marked)));
    assert_eq!(shell.place(), Some(Place::Home));
    assert_eq!(shell.depth(), 1);
    assert!(
        learning_requests(&mut shell).contains(&LearningQuery::Progress),
        "Home reads the guide again"
    );
}

#[test]
fn run_first_run_setup_again_clears_the_skip_and_starts_first_run_with_the_saved_profile() {
    let mut shell = profile_page(120, 36, Load::Ready(progress(true)));
    shell.key(Key::Char('f'));
    let sent = clears(&mut shell);
    assert_eq!(sent.len(), 1, "{sent:?}");
    shell.arrive(answer(sent[0], Ok(LearningReply::Marked)));
    assert_eq!(shell.place(), Some(Place::Welcome));
    assert_eq!(shell.depth(), 2, "Home at the root, first run on top");
    let rows = render(&mut shell, 120, 36);
    assert!(shows(&rows, "Welcome to CrazyAgent"), "{rows:#?}");
    assert!(shows(&rows, "Mira Okafor"), "{rows:#?}");
    shell.key(Key::Esc);
    shell.key(Key::Esc);
    assert_eq!(
        shell.place(),
        Some(Place::Welcome),
        "Esc does not leave first run"
    );
}

#[test]
fn a_refused_clear_stays_on_profile_with_its_reason_and_an_engine_without_marks_still_runs_first_run()
 {
    let mut shell = profile_page(120, 36, Load::Ready(progress(true)));
    for (key, code, said) in [
        ('g', "owner_busy", GUIDE_FAILED),
        ('g', "not_ready", NOT_READY),
        ('f', "owner_busy", FIRST_RUN_FAILED),
    ] {
        shell.key(Key::Char(key));
        let sent = clears(&mut shell);
        assert_eq!(sent.len(), 1, "{key} {code}");
        shell.arrive(answer(sent[0], Err(code.into())));
        assert_eq!(shell.place(), profile(), "{key} {code}");
        let rows = render(&mut shell, 120, 36);
        assert!(shows(&rows, said), "{key} {code}: {rows:#?}");
    }
    shell.key(Key::Char('f'));
    let sent = clears(&mut shell);
    shell.arrive(answer(sent[0], Err("not_ready".into())));
    assert_eq!(shell.place(), Some(Place::Welcome));
}

#[test]
fn g_and_f_type_into_a_focused_field_instead_of_replaying() {
    let mut shell = profile_page(120, 36, Load::Ready(progress(true)));
    shell.key(Key::Tab);
    shell.key(Key::End);
    shell.key(Key::Char('g'));
    shell.key(Key::Char('f'));
    assert!(clears(&mut shell).is_empty());
    let rows = render(&mut shell, 120, 36);
    assert!(shows(&rows, "Mira Okafor"), "{rows:#?}");
    assert!(shows(&rows, "gf"), "{rows:#?}");
}
