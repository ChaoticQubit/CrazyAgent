use super::new_project::{Control, Reply, Wizard};
use super::new_project_frames::compose;
use super::new_project_publish::*;
use super::new_project_tests::{control, key, type_text, wizard};
use super::new_project_view::{Look, wizard_screen};
use super::settings_accounts::tests::ctx;
use super::{Place, open};
use crate::github::{GitHubMethod, GitHubOwner, GitHubOwners, GitHubPublishPlan, GitHubStatus};
use crate::tui::client::OwnerEvent;
use crate::tui::client::intake::{IntakeEvent, IntakeQuery, IntakeView};
use crate::tui::ctx::Load;
use crate::tui::keys::Key;
use crate::tui::run::owner_arrival;
use crate::tui::seam::Request;
use crate::tui::shell::Shell;
use crossterm::event::KeyCode;

fn owners() -> GitHubOwners {
    GitHubOwners {
        owners: vec![
            GitHubOwner {
                login: "me".into(),
                personal: true,
            },
            GitHubOwner {
                login: "acme".into(),
                personal: false,
            },
        ],
        problem: None,
    }
}

fn signed_in(name: &str, about: &str) -> Wizard {
    let mut wizard = wizard();
    type_text(&mut wizard, name);
    wizard.key(key(KeyCode::Tab));
    type_text(&mut wizard, about);
    wizard.publish.set_github(GitHub::SignedIn);
    wizard.publish.set_owners(owners());
    wizard
}

fn plan_of(wizard: &Wizard) -> Option<GitHubPublishPlan> {
    wizard
        .publish
        .plan(wizard.name.value(), wizard.about.value())
}

fn screen_text(wizard: &Wizard) -> String {
    let look = Look::plain();
    compose(&wizard_screen(wizard, 80, 24, &look), 80, 24, "", &look)
}

fn status(signed_in: bool) -> GitHubStatus {
    GitHubStatus {
        signed_in,
        login: signed_in.then(|| "me".to_owned()),
        method: if signed_in {
            GitHubMethod::Token
        } else {
            GitHubMethod::None
        },
        waiting: false,
        problem: None,
    }
}

fn intake_queries(shell: &mut Shell) -> Vec<IntakeQuery> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Intake(job) => Some(job.query()),
            _ => None,
        })
        .collect()
}

fn answer(shell: &mut Shell, query: IntakeQuery, result: IntakeView) {
    shell.arrive(owner_arrival(OwnerEvent::Intake(Box::new(IntakeEvent {
        query,
        result: Ok(result),
    }))));
}

#[test]
fn a_new_wizard_publishes_on_github_private_without_the_docs_folder() {
    let wizard = wizard();
    assert!(wizard.publish.on);
    assert!(wizard.publish.private);
    assert!(!wizard.publish.docs);
    assert_eq!(wizard.publish.github, GitHub::Checking);
    assert_eq!(wizard.publish.panel, None);
    assert_eq!(
        wizard.controls(),
        [
            Control::Name,
            Control::About,
            Control::Publish,
            Control::Back,
            Control::Continue
        ]
    );
    assert_eq!(PUBLISH_TOGGLE, "Publish this project on GitHub");
    assert_eq!(
        PUBLISH_LINE,
        "Standard secure setup; agents can propose changes later."
    );
    assert_eq!(
        PUBLISH_DOCS,
        "Also publish the docs folder (plans, research)"
    );
    assert_eq!(SIGN_IN_WAIT, "Sign in to GitHub, or turn publishing off.");
}

#[test]
fn space_turns_publishing_off_and_on_and_enter_signed_out_asks_to_sign_in() {
    let mut wizard = wizard();
    type_text(&mut wizard, "Tidepool");
    wizard.focus_on(Control::Publish);
    assert_eq!(wizard.focused(), Control::Publish);
    assert!(!wizard.typing);
    assert_eq!(wizard.key(key(KeyCode::Char(' '))), Reply::Handled);
    assert!(!wizard.publish.on);
    assert_eq!(wizard.key(key(KeyCode::Char(' '))), Reply::Handled);
    assert!(wizard.publish.on);
    wizard.publish.set_github(GitHub::SignedOut);
    assert_eq!(wizard.key(key(KeyCode::Enter)), Reply::SignInGitHub);
    assert_eq!(wizard.publish.panel, None);
    assert_eq!(plan_of(&wizard), None);
    assert_eq!(wizard.publish.problem("Tidepool", ""), None);
}

#[test]
fn signed_in_the_personal_account_is_picked_and_the_name_and_description_follow_about() {
    let wizard = signed_in("My Project", "A  tool");
    assert_eq!(wizard.publish.shown_owner(), Some("me"));
    assert_eq!(wizard.publish.shown_name("My Project"), "My-Project");
    assert_eq!(wizard.publish.shown_description("A  tool"), "A tool");
    assert_eq!(
        plan_of(&wizard),
        Some(GitHubPublishPlan {
            owner: "me".into(),
            name: "My-Project".into(),
            description: "A tool".into(),
            private: true,
            docs: false,
        })
    );
    let mut off = signed_in("My Project", "A tool");
    off.publish.on = false;
    assert_eq!(plan_of(&off), None);
    assert_eq!(off.publish.problem("My Project", "A tool"), None);
}

#[test]
fn the_panel_sets_visibility_account_name_and_docs_and_an_edited_name_stays() {
    let mut wizard = signed_in("Tidepool", "Buoys");
    wizard.focus_on(Control::Publish);
    assert_eq!(wizard.key(key(KeyCode::Enter)), Reply::Handled);
    assert_eq!(wizard.publish.panel, Some(PanelRow::Visibility));
    wizard.key(key(KeyCode::Char(' ')));
    assert!(!wizard.publish.private);
    wizard.key(key(KeyCode::Down));
    assert_eq!(wizard.publish.panel, Some(PanelRow::Account));
    wizard.key(key(KeyCode::Right));
    assert_eq!(wizard.publish.shown_owner(), Some("acme"));
    wizard.key(key(KeyCode::Down));
    assert_eq!(wizard.publish.panel, Some(PanelRow::Name));
    assert!(wizard.typing);
    wizard.key(control('u'));
    type_text(&mut wizard, "site");
    wizard.key(key(KeyCode::Down));
    assert_eq!(wizard.publish.panel, Some(PanelRow::Description));
    wizard.key(key(KeyCode::Down));
    assert_eq!(wizard.publish.panel, Some(PanelRow::Docs));
    assert!(!wizard.typing);
    wizard.key(key(KeyCode::Char(' ')));
    assert!(wizard.publish.docs);
    wizard.key(key(KeyCode::Down));
    assert_eq!(wizard.publish.panel, Some(PanelRow::Done));
    assert_eq!(wizard.key(key(KeyCode::Enter)), Reply::Handled);
    assert_eq!(wizard.publish.panel, None);
    assert_eq!(wizard.focused(), Control::Publish);
    wizard.name.set("Renamed");
    assert_eq!(wizard.publish.shown_name("Renamed"), "site");
    assert_eq!(
        plan_of(&wizard),
        Some(GitHubPublishPlan {
            owner: "acme".into(),
            name: "site".into(),
            description: "Buoys".into(),
            private: false,
            docs: true,
        })
    );
}

#[test]
fn the_block_names_the_row_a_publish_start_would_refuse() {
    let mut wizard = signed_in("Tidepool", "");
    wizard.focus_on(Control::Publish);
    wizard.key(key(KeyCode::Enter));
    wizard.key(key(KeyCode::Down));
    wizard.key(key(KeyCode::Down));
    wizard.key(control('u'));
    type_text(&mut wizard, "bad name");
    assert!(matches!(
        wizard.publish.problem("Tidepool", ""),
        Some((PanelRow::Name, text)) if !text.is_empty()
    ));
    assert_eq!(plan_of(&wizard), None);
    let mut lonely = signed_in("Tidepool", "");
    lonely.publish.set_owners(GitHubOwners {
        owners: Vec::new(),
        problem: Some("no organizations".into()),
    });
    assert!(matches!(
        lonely.publish.problem("Tidepool", ""),
        Some((PanelRow::Account, _))
    ));
    assert_eq!(plan_of(&lonely), None);
}

#[test]
fn about_shows_the_publish_row_and_the_panel_its_fields() {
    let mut wizard = signed_in("Tidepool", "Buoys");
    let about = screen_text(&wizard);
    assert!(about.contains(&format!("[x] {PUBLISH_TOGGLE}")), "{about}");
    assert!(about.contains("Private, me/Tidepool"), "{about}");
    assert!(about.contains(PUBLISH_LINE), "{about}");
    wizard.focus_on(Control::Publish);
    wizard.key(key(KeyCode::Enter));
    let panel = screen_text(&wizard);
    for words in [
        "Visibility",
        "(*) Private",
        "Account",
        "Repository",
        PUBLISH_DOCS,
    ] {
        assert!(panel.contains(words), "{words}\n{panel}");
    }
    let mut out = signed_in("Tidepool", "Buoys");
    out.publish.set_github(GitHub::SignedOut);
    let shown = screen_text(&out);
    assert!(shown.contains("Sign in to GitHub"), "{shown}");
}

#[test]
fn the_page_reads_the_sign_in_then_the_accounts_and_sign_in_opens_the_github_tab() {
    let mut first = ctx(80, 24);
    first.store.pools = Load::Ready(Vec::new());
    let screen = open(&Place::NewProject, &mut first);
    let mut shell = Shell::with(first, screen);
    assert_eq!(intake_queries(&mut shell), [IntakeQuery::GitHubStatus]);
    answer(
        &mut shell,
        IntakeQuery::GitHubStatus,
        IntakeView::GitHub(status(true)),
    );
    assert_eq!(intake_queries(&mut shell), [IntakeQuery::GitHubOwners]);
    answer(
        &mut shell,
        IntakeQuery::GitHubOwners,
        IntakeView::Owners(owners()),
    );
    assert!(intake_queries(&mut shell).is_empty());

    let mut second = ctx(80, 24);
    second.store.pools = Load::Ready(Vec::new());
    let screen = open(&Place::NewProject, &mut second);
    let mut shell = Shell::with(second, screen);
    assert_eq!(intake_queries(&mut shell), [IntakeQuery::GitHubStatus]);
    answer(
        &mut shell,
        IntakeQuery::GitHubStatus,
        IntakeView::GitHub(status(false)),
    );
    assert!(intake_queries(&mut shell).is_empty());
    for character in "Tidepool".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Tab);
    shell.key(Key::Tab);
    shell.key(Key::Enter);
    assert_eq!(shell.place(), Some(Place::CloneRepo));
}
