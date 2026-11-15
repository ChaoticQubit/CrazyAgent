use super::*;
use crate::keep_awake::{Cap, Grace, KeepAwakeAction, KeepAwakeView};
use crate::notifications::NoticeCounts;
use crate::protocol::ProjectId;
use crate::tui::client::keep_awake::Picks;
use crate::tui::render_tests::{check, ready, render, shell_at};
use crate::tui::screens::keep_awake::tests::{resting, working};
use std::cell::RefCell;
use std::rc::Rc;

const STATUS: &str = "3 projects, 3 agents working";
const LONG: &str = "Awake while 3 agents run";
const SHORT: &str = "Awake: 3 agents";
const COUNTS: &str = "2 unread, 1 pending";

/// A screen with a status of its own that records the letters it is given, to stand in
/// for any screen under the shell's header and global keys.
struct Page {
    crumbs: Vec<String>,
    typing: bool,
    keeps_w: bool,
    took: Rc<RefCell<Vec<char>>>,
}

impl Page {
    fn new() -> Self {
        Self {
            crumbs: vec!["Tidepool Docs".into()],
            typing: false,
            keeps_w: false,
            took: Rc::default(),
        }
    }
}

impl Screen for Page {
    fn crumbs(&self) -> Vec<String> {
        self.crumbs.clone()
    }

    fn status(&self, _ctx: &Ctx) -> Line<'static> {
        Line::raw(STATUS)
    }

    fn hints(&self, _ctx: &Ctx) -> Vec<Hint> {
        vec![hint("Esc", "Back")]
    }

    fn typing(&self) -> bool {
        self.typing
    }

    fn keeps_w(&self) -> bool {
        self.keeps_w
    }

    fn key(&mut self, key: &Key, _ctx: &mut Ctx) -> Flow {
        match key {
            Key::Char(letter) => {
                self.took.borrow_mut().push(*letter);
                Flow::Handled
            }
            _ => Flow::Ignored,
        }
    }

    fn draw(&mut self, _frame: &mut Frame, _area: Rect, _ctx: &Ctx) -> Option<Position> {
        None
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Usage)
    }
}

fn on(page: Page) -> Shell {
    Shell::with(ready(120, 36), Box::new(page))
}

fn read(result: Result<KeepAwakeView, &str>) -> Arrival {
    answer(
        KeepAwakeQuery::Read,
        result.map(|view| KeepAwakeReply::View(Box::new(view))),
    )
}

fn answer(query: KeepAwakeQuery, result: Result<KeepAwakeReply, &str>) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::KeepAwake(Box::new(KeepAwakeEvent {
        query,
        result: result.map_err(str::to_owned),
    }))))
}

fn counted(shell: &mut Shell) {
    shell.ctx.store.notices = Some(NoticeCounts {
        unread: 2,
        pending: 1,
    });
}

fn header_at(shell: &mut Shell, width: u16) -> String {
    render(shell, width, 30).into_iter().next().unwrap()
}

fn keep_awake_requests(shell: &mut Shell) -> Vec<KeepAwakeQuery> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::KeepAwake(query) => Some(*query),
            _ => None,
        })
        .collect()
}

/// Checks that the parts stand in the header in this order, each once. Inputs: the header
/// row and the parts. Output: none; panics otherwise.
fn in_order(header: &str, parts: &[&str]) {
    let mut from = 0;
    for part in parts {
        assert_eq!(header.matches(part).count(), 1, "{part:?} once: {header}");
        let at = header[from..]
            .find(part)
            .unwrap_or_else(|| panic!("{part:?} out of order: {header}"));
        from += at + part.len();
    }
}

#[test]
fn the_header_shows_keep_awake_before_the_notice_counts_on_every_screen() {
    for (width, height) in [(120, 36), (80, 24)] {
        let mut shell = Shell::with(ready(width, height), Box::new(Page::new()));
        shell.arrive(read(Ok(working(3))));
        counted(&mut shell);
        check(
            &format!("header-keep-awake-{width}"),
            &render(&mut shell, width, height),
            &[],
        );
    }
    let project = ProjectId::new("project-tidepool").unwrap();
    let places = [
        Place::Home,
        Place::Usage,
        Place::Notifications,
        Place::Settings(SettingsSection::Profile),
        Place::KeepAwake,
        Place::KeepAwakeSettings,
        Place::Project {
            id: project.clone(),
            name: "Tidepool".into(),
            tab: ProjectTab::Overview,
        },
        Place::Team {
            id: project,
            name: "Tidepool".into(),
        },
    ];
    for place in places {
        let mut shell = shell_at(ready(160, 30), place.clone());
        let before = header_at(&mut shell, 160);
        assert!(!before.contains("wake:"), "{place:?}: {before}");
        assert!(!before.contains("Awake"), "{place:?}: {before}");
        shell.arrive(read(Ok(working(3))));
        counted(&mut shell);
        in_order(&header_at(&mut shell, 160), &[LONG, COUNTS]);
        shell.arrive(read(Ok(resting())));
        in_order(
            &header_at(&mut shell, 160),
            &["Keep awake: auto, nothing running", COUNTS],
        );
    }
}

#[test]
fn the_screen_status_stands_before_keep_awake_and_the_counts_at_the_right_end() {
    let mut shell = on(Page::new());
    shell.arrive(read(Ok(working(3))));
    counted(&mut shell);
    let header = header_at(&mut shell, 120);
    in_order(&header, &["Tidepool Docs", STATUS, LONG, COUNTS]);
    assert!(header.ends_with(&format!("{COUNTS} ")), "{header}");
    shell.ctx.store.notices = None;
    let header = header_at(&mut shell, 120);
    assert!(header.ends_with(&format!("{STATUS}  {LONG} ")), "{header}");
}

#[test]
fn narrow_header_drops_the_screen_status_first_and_never_the_counts() {
    let mut shell = on(Page::new());
    shell.arrive(read(Ok(working(3))));
    counted(&mut shell);
    in_order(&header_at(&mut shell, 120), &[STATUS, LONG, COUNTS]);

    let header = header_at(&mut shell, 100);
    in_order(&header, &["Tidepool Docs", LONG, COUNTS]);
    assert!(
        !header.contains(STATUS),
        "the screen status goes first: {header}"
    );

    let header = header_at(&mut shell, 80);
    in_order(&header, &["Tidepool Docs", SHORT, COUNTS]);
    assert!(!header.contains(STATUS), "{header}");
    assert!(!header.contains(LONG), "then the short form: {header}");

    let mut deep = Page::new();
    deep.crumbs = vec![
        "Tidepool Docs and Guides".into(),
        "Settings".into(),
        "Agent access".into(),
    ];
    let mut shell = on(deep);
    shell.arrive(read(Ok(working(3))));
    counted(&mut shell);
    let header = header_at(&mut shell, 80);
    in_order(&header, &["Agent access", SHORT, COUNTS]);
    assert!(
        !header.contains("Tidepool Docs and Guides"),
        "only then the crumbs shorten: {header}"
    );
}

#[test]
fn before_the_first_read_the_header_is_built_as_before() {
    let mut shell = on(Page::new());
    counted(&mut shell);
    let header = header_at(&mut shell, 100);
    in_order(&header, &["Tidepool Docs", STATUS, COUNTS]);
    assert!(!header.contains("wake"), "{header}");
}

#[test]
fn the_narrow_rule_keeps_three_cells_between_the_crumbs_and_the_status() {
    let mut shell = on(Page::new());
    shell.arrive(read(Ok(working(3))));
    counted(&mut shell);
    let whole = format!("Tidepool Docs   {STATUS}  {LONG}  {COUNTS} ");
    let header = header_at(&mut shell, 115);
    assert!(
        header.ends_with(&whole),
        "everything, three cells away: {header}"
    );
    let header = header_at(&mut shell, 114);
    assert!(!header.contains(STATUS), "two cells are too few: {header}");
    in_order(&header, &["Tidepool Docs", LONG, COUNTS]);
}

#[test]
fn shift_w_opens_keep_awake_and_types_into_a_focused_field() {
    for place in [Place::Home, Place::Usage] {
        let mut shell = shell_at(ready(120, 36), place.clone());
        let depth = shell.depth();
        shell.key(Key::Char('W'));
        assert_eq!(shell.place(), Some(Place::KeepAwake), "{place:?}");
        assert_eq!(shell.depth(), depth + 1, "{place:?}");
        shell.key(Key::Char('W'));
        assert_eq!(
            shell.depth(),
            depth + 1,
            "W on the screen opens no second one"
        );
        shell.key(Key::Esc);
        assert_eq!(shell.place(), Some(place.clone()), "Esc returns");
    }

    let mut typing = Page::new();
    typing.typing = true;
    let took = typing.took.clone();
    let mut shell = on(typing);
    shell.key(Key::Char('W'));
    assert_eq!(*took.borrow(), vec!['W'], "W types into the field");
    assert_eq!(shell.place(), Some(Place::Usage));

    let mut keeps = Page::new();
    keeps.keeps_w = true;
    let took = keeps.took.clone();
    let mut shell = on(keeps);
    shell.key(Key::Char('W'));
    assert_eq!(
        *took.borrow(),
        vec!['W'],
        "a screen with its own W keeps it"
    );
    assert_eq!(shell.place(), Some(Place::Usage));

    let plain = Page::new();
    let took = plain.took.clone();
    let mut shell = on(plain);
    shell.key(Key::Char('w'));
    assert_eq!(*took.borrow(), vec!['w'], "lower case w is the screen's");
    assert_eq!(shell.place(), Some(Place::Usage));
}

#[test]
fn shift_w_waits_for_an_open_overlay_and_stays_with_the_team_settings() {
    let mut shell = shell_at(ready(120, 36), Place::Home);
    shell.key(Key::Ctrl('k'));
    shell.key(Key::Char('W'));
    assert_eq!(shell.place(), Some(Place::Home), "W types into Go to");
    assert!(matches!(shell.overlay, Some(Overlay::GoTo(_))));
    shell.key(Key::Esc);
    shell.key(Key::Char('?'));
    shell.key(Key::Char('W'));
    assert_eq!(shell.place(), Some(Place::Home), "the Keys overlay keeps W");

    let team = Place::TeamSettings {
        id: ProjectId::new("project-tidepool").unwrap(),
        name: "Tidepool".into(),
    };
    let mut shell = shell_at(ready(120, 36), team.clone());
    shell.key(Key::Char('W'));
    assert_eq!(
        shell.place(),
        Some(team),
        "W is Terminals per window on the Team settings"
    );

    let mut shell = shell_at(ready(120, 36), Place::Welcome);
    shell.key(Key::Char('W'));
    assert_eq!(shell.place(), Some(Place::Welcome), "not before setup");
}

#[test]
fn go_to_and_the_keys_overlay_list_keep_awake() {
    let ctx = ready(120, 40);
    let labels: Vec<String> = targets(&ctx)
        .into_iter()
        .map(|target| target.label)
        .collect();
    assert!(labels.contains(&"Keep awake".to_owned()), "{labels:?}");
    assert!(
        labels.contains(&"Settings / Keep awake".to_owned()),
        "{labels:?}"
    );

    let mut shell = shell_at(ready(120, 40), Place::Home);
    shell.key(Key::Ctrl('k'));
    for character in "settings / keep".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Enter);
    assert_eq!(shell.place(), Some(Place::KeepAwakeSettings));

    let mut shell = shell_at(ready(120, 40), Place::Home);
    shell.key(Key::Ctrl('k'));
    for character in "keep awake".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Enter);
    assert_eq!(shell.place(), Some(Place::KeepAwake));

    let mut shell = shell_at(ready(120, 40), Place::Home);
    shell.key(Key::Char('?'));
    let shown = render(&mut shell, 120, 40).join("\n");
    assert!(shown.contains("W            Keep awake"), "{shown}");
}

#[test]
fn show_and_every_tick_read_keep_awake_once() {
    let mut connecting = ready(120, 36);
    connecting.link = Link::Connecting;
    let mut shell = Shell::new(connecting);
    assert_eq!(
        keep_awake_requests(&mut shell),
        Vec::new(),
        "not before the link"
    );
    shell.arrive(Arrival::Connected(None));
    assert_eq!(keep_awake_requests(&mut shell), vec![KeepAwakeQuery::Read]);

    let mut shell = shell_at(ready(120, 36), Place::Home);
    keep_awake_requests(&mut shell);
    shell.tick();
    shell.tick();
    assert_eq!(
        keep_awake_requests(&mut shell),
        vec![KeepAwakeQuery::Read],
        "a tick reads the view, and a read already queued is not queued twice"
    );
}

#[test]
fn a_read_keeps_the_view_a_failed_read_keeps_the_last_and_a_change_reads_again() {
    let mut shell = on(Page::new());
    assert_eq!(shell.ctx.store.keep_awake, None);
    shell.arrive(read(Ok(working(3))));
    assert_eq!(shell.ctx.store.keep_awake, Some(working(3)));
    shell.arrive(read(Err("owner_busy")));
    assert_eq!(
        shell.ctx.store.keep_awake,
        Some(working(3)),
        "a failed read keeps the last view"
    );
    keep_awake_requests(&mut shell);

    let act = KeepAwakeQuery::Act {
        action: KeepAwakeAction::End,
        generation: 7,
    };
    shell.arrive(answer(act.clone(), Ok(KeepAwakeReply::Done)));
    assert_eq!(keep_awake_requests(&mut shell), vec![KeepAwakeQuery::Read]);
    shell.arrive(answer(act, Err("keep_awake_not_holding")));
    assert_eq!(
        keep_awake_requests(&mut shell),
        vec![KeepAwakeQuery::Read],
        "a refused change was made on a stale view"
    );

    let picks = Picks {
        grace: Some(Grace::Min30),
        cap: Some(Cap::H4),
        ..Picks::default()
    };
    let save = KeepAwakeQuery::Save {
        picks,
        generation: 8,
    };
    shell.arrive(answer(save.clone(), Err("owner_busy")));
    assert_eq!(
        shell.ctx.store.keep_awake,
        Some(working(3)),
        "a refused save changes nothing shown"
    );
    shell.arrive(answer(save, Ok(KeepAwakeReply::Done)));
    let mut want = working(3);
    want.settings.grace = Grace::Min30;
    want.settings.cap = Cap::H4;
    assert_eq!(
        shell.ctx.store.keep_awake,
        Some(want),
        "a save shows its picks until the read lands"
    );
    assert_eq!(keep_awake_requests(&mut shell), vec![KeepAwakeQuery::Read]);
}

#[test]
fn losing_the_engine_takes_keep_awake_out_of_the_header() {
    let mut shell = on(Page::new());
    shell.arrive(read(Ok(working(3))));
    assert!(header_at(&mut shell, 120).contains(LONG));
    shell.arrive(Arrival::Disconnected("trusted_connection_lost".into()));
    assert_eq!(shell.ctx.store.keep_awake, None);
    let header = header_at(&mut shell, 120);
    assert!(!header.contains("Awake"), "{header}");
    keep_awake_requests(&mut shell);
    shell.arrive(Arrival::Connected(None));
    assert_eq!(
        keep_awake_requests(&mut shell),
        vec![KeepAwakeQuery::Read],
        "the view is read again once the engine is back"
    );
}

#[test]
fn the_header_draws_each_tone_in_its_color() {
    let ctx = ready(120, 36);
    let theme = &ctx.theme;
    let style = |view: KeepAwakeView| {
        let line = with_keep_awake(Some(&view), Line::default(), theme);
        assert_eq!(line.spans.len(), 1);
        line.spans[0].style
    };
    assert_eq!(style(working(3)), theme.success);
    assert_eq!(style(resting()), theme.muted);
    let mut limited = working(3);
    limited.lid_support = crate::keep_awake::LidSupport::IdleOnlyOnBattery;
    assert_eq!(style(limited), theme.warning);
    let mut ended = resting();
    ended.state.ended = Some(crate::keep_awake::EndReason::Heat);
    assert_eq!(style(ended), theme.error);
    let unread = with_keep_awake(None, Line::raw(STATUS), theme);
    assert_eq!(unread.width(), STATUS.len(), "no view, no segment");
    let short = with_keep_awake_short(Some(&working(3)), Line::default(), theme);
    assert_eq!(crate::tui::text::plain(&short), SHORT);
}
