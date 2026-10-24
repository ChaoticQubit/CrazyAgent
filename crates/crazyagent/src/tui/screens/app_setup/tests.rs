use super::*;
use crate::tui::screens::settings_accounts::tests::{ctx, render};
use crate::tui::shell::{Shell, targets};

#[test]
fn first_run_setup_continues_to_home() {
    let mut ctx = ctx(80, 24);
    let mut setup = AppSetup::new(true);
    assert_eq!(setup.crumbs(), ["Setup"]);
    assert_eq!(setup.place(), Some(Place::Setup { first_run: true }));
    let Flow::Replace(next) = setup.key(&Key::Enter, &mut ctx) else {
        panic!("first run continues to Home")
    };
    assert_eq!(next.place(), Some(Place::Home));
}

#[test]
fn settings_setup_opens_from_go_to_and_esc_goes_back() {
    let mut ctx = ctx(80, 24);
    assert!(
        targets(&ctx)
            .iter()
            .any(|target| target.label == "Settings / Setup"
                && target.kind == "Settings"
                && target.place == Place::Setup { first_run: false })
    );
    let mut setup = open(&Place::Setup { first_run: false }, &mut ctx);
    assert_eq!(setup.crumbs(), ["Settings", "Setup"]);
    assert!(matches!(setup.key(&Key::Enter, &mut ctx), Flow::Ignored));
    assert!(matches!(setup.key(&Key::Esc, &mut ctx), Flow::Ignored));
}

#[test]
fn setup_without_engine_support_says_not_ready_yet() {
    let mut shell = Shell::with(ctx(80, 24), Box::new(AppSetup::new(true)));
    let rows = render(&mut shell, 80, 24);
    assert!(rows.iter().any(|row| row.contains("Not ready yet.")));
    assert!(rows.iter().any(|row| row.contains("Enter Continue")));
}
