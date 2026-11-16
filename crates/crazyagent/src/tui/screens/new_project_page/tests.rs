use super::*;
use crate::tui::glyphs::UNICODE;
use crate::tui::theme::{Appearance, Colors, Depth};

fn named(name: &str) -> Wizard {
    let mut wizard = Wizard::new(Vec::new(), None);
    wizard.name.set(name);
    wizard
}

fn refused_page(name: &str) -> NewProjectPage {
    NewProjectPage {
        wizard: named(name),
        view: View::default(),
        creation: None,
        checking: false,
        asked: false,
    }
}

#[test]
fn a_refusal_past_the_kept_draft_bound_keeps_no_draft_and_says_so() {
    let mut ctx = Ctx::new(
        Depth::NoColor,
        Colors::Dark,
        Appearance::Dark,
        &UNICODE,
        "/private/tmp/engine.sock".into(),
    );
    KEPT.with_borrow_mut(|kept| {
        kept.extend((1..DRAFTS_MAX).map(|draft| named(&format!("Draft {draft}"))));
    });

    let mut below = refused_page("Tidepool");
    below.refused(&mut ctx);
    assert_eq!(
        ctx.message().map(|message| message.rest.as_str()),
        Some("Tidepool is kept as a draft. New project opens your earlier drafts first.")
    );
    drop(below);
    assert_eq!(KEPT.with_borrow(VecDeque::len), DRAFTS_MAX);
    assert_eq!(kept_draft().as_deref(), Some("Draft 1"));

    let mut past = refused_page("Harbor");
    past.refused(&mut ctx);
    let said = format!("{DRAFTS_MAX} drafts are already kept. Create Harbor again in New project.");
    assert_eq!(
        ctx.message().map(|message| message.rest.as_str()),
        Some(said.as_str())
    );
    drop(past);
    assert_eq!(KEPT.with_borrow(VecDeque::len), DRAFTS_MAX);
    assert!(KEPT.with_borrow(|kept| kept.back().is_some_and(|w| w.name.value() == "Tidepool")));
    KEPT.with_borrow_mut(VecDeque::clear);
}

/// The title and detail the wizard shows for one failure code. Takes the code. Returns them.
fn said(code: &str) -> (String, String) {
    let mut wizard = named("Tidepool");
    wizard.failed(code);
    match &wizard.phase {
        Phase::Failed { title, detail, .. } => (title.clone(), detail.clone()),
        _ => panic!("{code} leaves the wizard failed"),
    }
}

/// DEC-177 step 3b: a New Project job that stopped, found its folder taken or cannot copy
/// documents on this computer says so in New project's words, never the add folder's.
#[test]
fn a_stopped_or_refused_new_project_job_is_said_in_new_project_words() {
    let (title, detail) = said("home_add_cancelled");
    assert_eq!(title, "Setting up the project stopped.");
    assert!(detail.starts_with("Nothing was kept."), "{detail}");
    assert!(!detail.contains("Adding the folder"), "{detail}");

    let (title, detail) = said("home_destination_exists");
    assert_eq!(title, "Could not create the folder.");
    assert!(
        detail.starts_with("A folder for this project already exists."),
        "{detail}"
    );

    let (title, detail) = said("project_folder_unavailable");
    assert_eq!(title, "Could not copy a document.");
    assert!(detail.contains("without documents"), "{detail}");
}
