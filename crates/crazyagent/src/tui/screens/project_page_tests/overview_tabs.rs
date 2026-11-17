use crate::project_overview::{Apply, Competitors, Overview, Readme, TAB_BYTES_MAX, Tab, TabState};
use crate::protocol::{CredentialRef, ProjectId};
use crate::roadmap_status::Refresh;
use crate::tui::frame::{Hint, footer, hint};
use crate::tui::glyphs::UNICODE;
use crate::tui::screens::new_project_view::{Look, Row};
use crate::tui::screens::project::{OVERVIEW_TAB_LABELS, PoolInfo, ProjectReply, ProjectScreen};
use crate::tui::screens::project_publish::PROMPT_LINE;
use crate::tui::screens::project_view::project_screen;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const TAB_ROW: &str = "Quick info  Features  Competitors  Install & use  README";

fn empty() -> Overview {
    Overview {
        readme: None,
        tabs: Tab::ALL
            .into_iter()
            .map(|tab| TabState {
                tab,
                text: None,
                version: String::new(),
                edited: false,
                foreign: false,
                proposal: None,
                proposal_version: String::new(),
                blocked: None,
            })
            .collect(),
        competitors: Competitors::Missing,
        running: false,
        stale: false,
        checked_ms: None,
        failure: None,
        waiting_for_account: false,
        due_in_ms: None,
        refresh: Refresh::AfterLanding,
        apply: Apply::AlwaysAsk,
        needs_home: false,
    }
}

fn overview_with(tab: Tab, text: &str) -> Overview {
    let mut overview = empty();
    let state = overview.tabs.iter_mut().find(|state| state.tab == tab);
    state.unwrap().text = Some(text.to_owned());
    overview
}

fn page_with_overview(overview: Overview) -> ProjectScreen {
    let project = ProjectId::new("alpha").unwrap();
    let mut page = ProjectScreen::new(project, "Alpha".into(), None);
    page.overview_tabs = Some(overview);
    page
}

fn press(page: &mut ProjectScreen, code: KeyCode) -> ProjectReply {
    page.key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn frame(page: &mut ProjectScreen) -> String {
    let drawn = project_screen(page, 100, 40, &Look::plain());
    let rows: Vec<String> = drawn.rows.iter().map(Row::text).collect();
    rows.join("\n")
}

fn flat(page: &mut ProjectScreen) -> String {
    let shown = frame(page);
    shown.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn blocked(tab: Tab, code: &str) -> Overview {
    let mut overview = overview_with(tab, "Old text that must not show.\n");
    for state in &mut overview.tabs {
        if state.tab == tab || code == "not_a_folder" {
            state.text = None;
            state.blocked = Some(code.to_owned());
        }
    }
    overview
}

fn marked(label: &str) -> String {
    TAB_ROW.replace(label, &format!("[{label}]"))
}

#[test]
fn the_overview_shows_the_tab_names_and_the_picked_tabs_text() {
    let mut page = page_with_overview(overview_with(Tab::Features, "- Search papers\n"));
    assert!(frame(&mut page).contains(&marked("Quick info")));
    assert_eq!(press(&mut page, KeyCode::Right), ProjectReply::Handled);
    let shown = frame(&mut page);
    assert!(shown.contains("Quick info  [Features]  Competitors  Install & use  README"));
    assert!(shown.contains("Search papers"));
    assert_eq!(OVERVIEW_TAB_LABELS.join("  "), TAB_ROW);
}

#[test]
fn left_and_right_move_the_picked_tab_and_wrap_at_both_ends() {
    let mut overview = overview_with(Tab::QuickInfo, "Alpha reads papers.\n");
    overview.readme = Some(Readme {
        path: "README.md".into(),
        text: "# Alpha\n\nInstall with cargo.\n".into(),
        cut: true,
    });
    let mut page = page_with_overview(overview);
    assert!(frame(&mut page).contains("Alpha reads papers."));
    assert_eq!(press(&mut page, KeyCode::Left), ProjectReply::Handled);
    let readme = frame(&mut page);
    assert!(readme.contains(&marked("README")));
    assert!(readme.contains("README.md"));
    assert!(readme.contains("This is the project's own file."));
    assert!(readme.contains("Showing the first part of a long file."));
    assert!(readme.contains("Install with cargo."));
    assert!(!readme.contains("Alpha reads papers."));
    press(&mut page, KeyCode::Right);
    assert!(frame(&mut page).contains(&marked("Quick info")));
    for label in ["Features", "Competitors", "Install & use", "README"] {
        press(&mut page, KeyCode::Right);
        assert!(frame(&mut page).contains(&marked(label)), "{label}");
    }
    assert!(page.overview_tab < OVERVIEW_TAB_LABELS.len());
}

#[test]
fn a_project_without_a_readme_says_so() {
    let mut page = page_with_overview(empty());
    press(&mut page, KeyCode::Left);
    assert!(frame(&mut page).contains("This project has no README file."));
}

#[test]
fn a_waiting_proposal_points_to_the_desktop_and_only_on_its_own_tab() {
    let mut overview = overview_with(Tab::Features, "- Search papers\n");
    overview.tabs[1].proposal = Some("- Search papers\n- Share lists\n".into());
    let mut page = page_with_overview(overview);
    let line = "A refresh proposes changes. Review them in the desktop app.";
    assert!(!frame(&mut page).contains(line));
    press(&mut page, KeyCode::Right);
    let shown = frame(&mut page);
    assert!(shown.contains(line));
    assert!(shown.contains("Search papers"));
    assert!(!shown.contains("Share lists"));
}

/// RV-DS241-2: a tab whose file came with the project's files, not written by the engine
/// nor saved by the owner, says so above its text in the desktop's words (DEC-141), and
/// only on that tab; a tab the owner edited does not.
#[test]
fn a_foreign_tab_file_says_it_came_with_the_projects_files() {
    let line = concat!(
        "This tab's file came with the project's files: CrazyAgent didn't write it and ",
        "you didn't save it. Check it before you use its commands."
    );
    let mut overview = overview_with(Tab::InstallAndUse, "Run setup.sh first.\n");
    overview.tabs[3].foreign = true;
    let mut page = page_with_overview(overview.clone());
    assert!(!flat(&mut page).contains(line), "only on its own tab");
    for _ in 0..3 {
        press(&mut page, KeyCode::Right);
    }
    assert!(frame(&mut page).contains(&marked("Install & use")));
    let shown = flat(&mut page);
    assert!(shown.contains(line), "{shown}");
    assert!(shown.contains("Run setup.sh first."));
    overview.tabs[3].foreign = false;
    overview.tabs[3].edited = true;
    let mut mine = page_with_overview(overview);
    for _ in 0..3 {
        press(&mut mine, KeyCode::Right);
    }
    assert!(
        !flat(&mut mine).contains(line),
        "the owner's edit is not foreign"
    );
}

#[test]
fn a_tab_not_written_yet_says_what_will_write_it() {
    let shown = |change: fn(&mut Overview)| {
        let mut overview = empty();
        change(&mut overview);
        let mut page = page_with_overview(overview);
        press(&mut page, KeyCode::Right);
        frame(&mut page)
    };
    assert!(
        shown(|_| ()).contains("Not written yet. It is written after the next landed change set")
    );
    assert!(shown(|o| o.refresh = Refresh::RefreshOnly).contains("Not written yet. It is"));
    assert!(
        !shown(|o| o.refresh = Refresh::RefreshOnly).contains("landed change set"),
        "Refresh only never writes after a landing"
    );
    assert!(
        shown(|o| o.refresh = Refresh::Off).contains("Overview tabs are off in Project settings.")
    );
    let running = shown(|o| o.running = true);
    assert!(running.contains("Writing now"));
    assert!(!running.contains("Not written yet"));
}

#[test]
fn a_failed_run_shows_its_reason_and_keeps_the_tab() {
    let mut overview = overview_with(Tab::QuickInfo, "Alpha reads papers.\n");
    overview.failure = Some("The account is out of usage".into());
    let shown = frame(&mut page_with_overview(overview));
    assert!(shown.contains("Couldn't refresh the tabs. The account is out of usage"));
    assert!(shown.contains("Alpha reads papers."));
}

#[test]
fn a_run_waiting_for_an_account_says_why_and_never_that_the_refresh_failed() {
    let reason = crate::project_overview::NO_ACCOUNT;
    for (stale, running) in [(true, false), (false, true)] {
        let mut overview = empty();
        overview.failure = Some(reason.into());
        overview.waiting_for_account = true;
        overview.stale = stale;
        overview.running = running;
        let shown = flat(&mut page_with_overview(overview));
        assert!(shown.contains(reason), "{shown}");
        assert!(!shown.contains("Couldn't refresh the tabs"), "{shown}");
    }
    let mut ended = empty();
    ended.failure = Some(reason.into());
    ended.waiting_for_account = true;
    let shown = flat(&mut page_with_overview(ended));
    assert!(
        shown.contains(&format!("Couldn't refresh the tabs. {reason}")),
        "{shown}"
    );
}

/// An ended real failure stays a failed refresh while the next run waits to start and
/// while it writes (IW-L1, DEC-147 addendum): only the engine's wait for an account is
/// shown as a wait, in the same words as the desktop app.
#[test]
fn an_ended_failure_stays_a_failed_refresh_while_a_new_run_waits_or_writes() {
    let reason = crate::project_overview::UNFINISHED;
    for (stale, running) in [(true, false), (false, true), (true, true)] {
        let mut overview = empty();
        overview.failure = Some(reason.into());
        overview.stale = stale;
        overview.running = running;
        let shown = flat(&mut page_with_overview(overview));
        assert!(
            shown.contains(&format!("Couldn't refresh the tabs. {reason}")),
            "stale {stale} running {running}: {shown}"
        );
    }
}

#[test]
fn before_the_tabs_are_read_there_is_no_tab_row_and_the_arrows_do_nothing() {
    let project = ProjectId::new("alpha").unwrap();
    let mut page = ProjectScreen::new(project, "Alpha".into(), None);
    assert!(!frame(&mut page).contains("Install & use"));
    assert_eq!(press(&mut page, KeyCode::Right), ProjectReply::Ignored);
    assert_eq!(press(&mut page, KeyCode::PageDown), ProjectReply::Ignored);
    assert_eq!(page.overview_tab, 0);
}

#[test]
fn a_long_tab_scrolls_with_page_keys_and_says_when_more_is_below() {
    let lines: Vec<String> = (1..=120).map(|n| format!("line number {n}")).collect();
    let mut page = page_with_overview(overview_with(Tab::QuickInfo, &lines.join("\n\n")));
    let top = frame(&mut page);
    assert!(top.contains("line number 1"));
    assert!(!top.contains("line number 120"));
    assert!(top.contains("PgDn for more"));
    for _ in 0..40 {
        assert_eq!(press(&mut page, KeyCode::PageDown), ProjectReply::Handled);
    }
    let end = frame(&mut page);
    assert!(
        end.contains("line number 120"),
        "the scroll stops at the last line"
    );
    assert!(!end.contains("PgDn for more"));
    for _ in 0..40 {
        press(&mut page, KeyCode::PageUp);
    }
    assert_eq!(page.overview_scroll, 0);
    press(&mut page, KeyCode::PageDown);
    press(&mut page, KeyCode::Right);
    assert_eq!(page.overview_scroll, 0, "a tab opens at its top");
}

#[test]
fn hidden_characters_of_a_tab_show_as_visible_escapes() {
    let text = "Zebra\u{202e} reads\u{1b}[31m papers.\n";
    let shown = frame(&mut page_with_overview(overview_with(Tab::QuickInfo, text)));
    assert!(!shown.contains('\u{202e}'));
    assert!(!shown.contains('\u{1b}'));
    let escaped = format!("Zebra{}u202e reads papers.", '\\');
    assert!(shown.contains(&escaped), "{shown}");
}

#[test]
fn a_project_with_a_pool_shows_the_tab_row_beside_its_team() {
    let mut page = page_with_overview(overview_with(Tab::QuickInfo, "Zebra reads papers.\n"));
    page.pool = Some(PoolInfo {
        id: CredentialRef::new("pool-main").unwrap(),
        name: "Main".into(),
        accounts: 1,
        assigned: true,
    });
    let shown = frame(&mut page);
    assert!(shown.contains(&marked("Quick info")), "{shown}");
    assert!(shown.contains("Zebra reads papers."), "{shown}");
    assert!(shown.contains("Team"), "{shown}");
    assert!(!shown.contains("This project has no pool"), "{shown}");
    let help = project_screen(&mut page, 100, 40, &Look::plain()).help;
    assert!(help.iter().any(|(_, what)| *what == "Overview tab"));
    assert!(help.iter().any(|(_, what)| *what == "Scroll"));
}

#[test]
fn an_80_by_24_overview_keeps_the_publish_question_and_the_keys_it_had_before() {
    let text: String = (1..=12).map(|line| format!("- Point {line}\n")).collect();
    let mut overview = overview_with(Tab::QuickInfo, &text);
    overview.failure = Some(
            "The agent stopped before it wrote the tabs, so they show what they said the last time they were read. "
                .repeat(3),
        );
    let mut page = page_with_overview(overview);
    page.pool = Some(PoolInfo {
        id: CredentialRef::new("pool-main").unwrap(),
        name: "Main".into(),
        accounts: 1,
        assigned: true,
    });
    page.github_notice = true;
    let drawn = project_screen(&mut page, 80, 24, &Look::plain());
    let shown = drawn
        .rows
        .iter()
        .map(Row::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(shown.contains("Main  1 account"), "pooled: {shown}");
    let notice = shown.find(PROMPT_LINE);
    let strip = shown.find(&marked("Quick info"));
    assert!(notice.is_some(), "the question shows at 80x24: {shown}");
    assert!(strip.is_some(), "{shown}");
    assert!(
        notice < strip,
        "the question sits above the tabs, so their rows can't push it off: {shown}"
    );
    assert!(
        shown.contains("G Publish on GitHub"),
        "the body line names G: {shown}"
    );
    let hints: Vec<Hint> = drawn
        .help
        .iter()
        .map(|(key, label)| hint(key, label))
        .collect();
    let ctx = super::ctx(80, 24);
    let bar = footer(80, &hints, 0, &ctx.theme, &UNICODE);
    let bar: String = bar.spans.iter().map(|span| span.content.as_ref()).collect();
    for key in ["Enter Open", "o Open folder", "PgUp/PgDn Scroll"] {
        assert!(bar.contains(key), "{key} stays in the bar: {bar}");
    }
}

#[test]
fn the_key_help_names_the_tab_and_scroll_keys_once_the_tabs_are_read() {
    let mut page = page_with_overview(empty());
    let help = project_screen(&mut page, 100, 40, &Look::plain()).help;
    assert!(help.iter().any(|(_, what)| *what == "Overview tab"));
    assert!(help.contains(&("Shift-R", "Refresh tabs")));
    let project = ProjectId::new("alpha").unwrap();
    let mut unread = ProjectScreen::new(project, "Alpha".into(), None);
    let help = project_screen(&mut unread, 100, 40, &Look::plain()).help;
    assert!(!help.iter().any(|(_, what)| *what == "Overview tab"));
}

#[test]
fn a_blocked_tab_says_why_names_its_file_and_what_to_do() {
    let large = format!("its file is larger than {} KB", TAB_BYTES_MAX / 1024);
    let reasons = [
        ("blank", "its file is empty"),
        ("too_large", large.as_str()),
        ("not_utf8", "its file is not plain text"),
        ("something_new", "its file can't be read"),
    ];
    assert_eq!(TAB_BYTES_MAX % 1024, 0, "the bound reads as whole KB");
    for (code, why) in reasons {
        let mut page = page_with_overview(blocked(Tab::Features, code));
        press(&mut page, KeyCode::Right);
        let shown = flat(&mut page);
        let line = format!(
            "CrazyAgent can't show this tab: {why}. The file is \
                 .crazyagent/overview/features.md in the project. Fix or delete the file, \
                 then refresh the tabs."
        );
        assert!(shown.contains(&line), "{code}\n{shown}");
        assert!(!shown.contains("Not written yet"), "{code}\n{shown}");
        assert!(!shown.contains("something_new"), "{code}\n{shown}");
        press(&mut page, KeyCode::Left);
        assert!(!flat(&mut page).contains("CrazyAgent can't show this tab"));
    }
}

#[test]
fn a_link_or_folder_in_a_tab_files_place_asks_to_remove_it() {
    let mut page = page_with_overview(blocked(Tab::InstallAndUse, "not_a_file"));
    for _ in 0..3 {
        press(&mut page, KeyCode::Right);
    }
    let shown = flat(&mut page);
    let line = "CrazyAgent can't show this tab: a link or folder is where its file goes. \
                    The path is .crazyagent/overview/install-and-use.md in the project. \
                    Remove it, then refresh the tabs.";
    assert!(shown.contains(line), "{shown}");
    assert!(!shown.contains("Fix or delete the file"), "{shown}");
}

#[test]
fn a_tabs_folder_that_is_none_blocks_every_generated_tab_and_names_the_folder() {
    let mut page = page_with_overview(blocked(Tab::QuickInfo, "not_a_folder"));
    let line = "CrazyAgent can't show this tab: the folder for the tabs is a link or a file. \
                    The folder is .crazyagent/overview in the project. \
                    Remove it, then refresh the tabs.";
    for _ in 0..Tab::ALL.len() {
        let shown = flat(&mut page);
        assert!(shown.contains(line), "{shown}");
        assert!(!shown.contains(".md in the project"), "{shown}");
        press(&mut page, KeyCode::Right);
    }
    assert!(flat(&mut page).contains("This project has no README file."));
}

#[test]
fn a_project_outside_the_crazyagent_folder_says_the_tabs_need_it_there_and_keeps_the_readme() {
    let mut overview = empty();
    overview.needs_home = true;
    overview.readme = Some(Readme {
        path: "README.md".into(),
        text: "Zebra reads papers.\n".into(),
        cut: false,
    });
    let mut page = page_with_overview(overview);
    let line = "Overview tabs need this project in the CrazyAgent folder. \
                    Move it there in the desktop app, in Project settings.";
    for _ in 0..Tab::ALL.len() {
        let shown = flat(&mut page);
        assert!(shown.contains(line), "{shown}");
        assert!(!shown.contains("Not written yet"), "{shown}");
        press(&mut page, KeyCode::Right);
    }
    let readme = flat(&mut page);
    assert!(readme.contains("Zebra reads papers."), "{readme}");
    assert!(
        !readme.contains("Overview tabs need this project"),
        "{readme}"
    );
}
