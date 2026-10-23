use super::clock;
use super::ctx::{Ctx, Link, Load};
use super::frame::Message;
use super::glyphs::{ASCII, Glyphs, UNICODE};
use super::keys::Key;
use super::screens::new_project_page::kept_draft;
use super::screens::{Flow, Place, ProjectTab, Screen, SettingsSection, open};
use super::seam::{ProjectCard, ResearchCounts};
use super::shell::Shell;
use super::theme::{Appearance, Colors, Depth};
use crate::owner_profile::{AnswerDetail, Initiative, OwnerProfile, WorkingStyle};
use crate::pools::{MemberKind, MemberStatus, PoolMember, PoolSummary};
use crate::project_setup::Platform;
use crate::protocol::{CredentialRef, ProjectId};
use ratatui::Terminal;
use ratatui::backend::{Backend, TestBackend};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Color;

const DAY_MS: i64 = 86_400_000;

fn ctx(width: u16, height: u16) -> Ctx {
    let mut ctx = Ctx::new(
        Depth::NoColor,
        Colors::Dark,
        Appearance::Dark,
        &UNICODE,
        "/Users/mira/.crazyagent/engine.sock".into(),
    );
    ctx.home = Some("/Users/mira".into());
    ctx.width = width;
    ctx.height = height;
    ctx.tick = 4;
    ctx
}

fn ready(width: u16, height: u16) -> Ctx {
    let mut ctx = ctx(width, height);
    ctx.link = Link::Ready;
    ctx
}

fn render(shell: &mut Shell, width: u16, height: u16) -> Vec<String> {
    let away = Position::new(u16::MAX, u16::MAX);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.backend_mut().set_cursor_position(away).unwrap();
    terminal.draw(|frame| shell.draw(frame)).unwrap();
    let cursor = terminal.backend_mut().get_cursor_position().ok();
    let shown = cursor != Some(away);
    let buffer = terminal.backend().buffer().clone();
    let mut rows = Vec::new();
    for y in 0..height {
        let mut line = String::new();
        let mut skip = 0;
        for x in 0..width {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let symbol = buffer[(x, y)].symbol();
            if shown && cursor == Some(Position::new(x, y)) {
                line.push('█');
            } else {
                line.push_str(symbol);
            }
            skip = super::text::width(symbol).saturating_sub(1);
        }
        rows.push(line);
    }
    rows
}

fn fixture(name: &str, swaps: &[(&str, &str)]) -> Vec<String> {
    let path = format!("{}/src/tui/frames/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing fixture {path}"));
    text.lines()
        .map(|row| {
            let mut row = row.to_owned();
            for (from, to) in swaps {
                let from_width = super::text::width(from);
                let to_width = super::text::width(to);
                if row.contains(from) && to_width <= from_width {
                    row = row.replacen(
                        from,
                        &format!("{to}{}", " ".repeat(from_width - to_width)),
                        1,
                    );
                }
            }
            row
        })
        .collect()
}

fn save(name: &str, rows: &[String]) {
    if let Ok(folder) = std::env::var("TUI_FRAMES_OUT") {
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(format!("{folder}/{name}.txt"), rows.join("\n") + "\n").unwrap();
    }
}

fn check(name: &str, actual: &[String], swaps: &[(&str, &str)]) {
    save(name, actual);
    let expected = fixture(name, swaps);
    let wrong: Vec<String> = expected
        .iter()
        .zip(actual)
        .enumerate()
        .filter(|(_, (want, got))| want != got)
        .map(|(row, (want, got))| format!("row {row}\n want |{want}|\n got  |{got}|"))
        .collect();
    assert_eq!(expected.len(), actual.len(), "{name}: row count");
    assert!(wrong.is_empty(), "{name} differs:\n{}", wrong.join("\n"));
}

fn chars(row: &str, from: usize, to: usize) -> String {
    row.chars().skip(from).take(to - from).collect()
}

fn check_modal(name: &str, actual: &[String], title: &str) {
    save(name, actual);
    let expected = fixture(name, &[]);
    let top = expected
        .iter()
        .position(|row| row.contains(&format!("┌─ {title} ")))
        .unwrap_or_else(|| panic!("{name}: no modal {title}"));
    let left = expected[top].chars().position(|c| c == '┌').unwrap();
    let right = expected[top].chars().position(|c| c == '┐').unwrap() + 1;
    let bottom = (top..expected.len())
        .find(|row| chars(&expected[*row], left, left + 1) == "└")
        .unwrap();
    let mut wrong = Vec::new();
    for row in (top..=bottom).chain([expected.len() - 1]) {
        let (from, to) = if row == expected.len() - 1 {
            (0, expected[row].chars().count())
        } else {
            (left, right)
        };
        let want = chars(&expected[row], from, to);
        let got = chars(&actual[row], from, to);
        if want != got {
            wrong.push(format!("row {row}\n want |{want}|\n got  |{got}|"));
        }
    }
    assert!(wrong.is_empty(), "{name} differs:\n{}", wrong.join("\n"));
}

fn keys(shell: &mut Shell, text: &str) {
    for character in text.chars() {
        shell.key(Key::Char(character));
    }
}

fn member(index: usize, status: MemberStatus) -> PoolMember {
    PoolMember {
        id: CredentialRef::new(format!("member-{index}")).unwrap(),
        kind: MemberKind::Chatgpt,
        provider: "ChatGPT".into(),
        status,
        status_label: status.label().into(),
    }
}

fn pool(name: &str, ready: usize, waiting: usize, projects: usize) -> PoolSummary {
    PoolSummary {
        id: CredentialRef::new(format!("pool-{}", name.replace(' ', "-").to_lowercase())).unwrap(),
        name: name.into(),
        summary: String::new(),
        members: (0..ready)
            .map(|index| member(index, MemberStatus::SignedIn))
            .chain((0..waiting).map(|index| member(ready + index, MemberStatus::NotSignedIn)))
            .collect(),
        project_count: projects,
    }
}

fn pools() -> Vec<PoolSummary> {
    vec![
        pool("Main", 6, 1, 2),
        pool("Research", 3, 1, 0),
        pool("Local only", 1, 1, 0),
    ]
}

fn card(
    name: &str,
    about: &str,
    platforms: &[Platform],
    pool: Option<&str>,
    research: ResearchCounts,
    age_ms: i64,
) -> ProjectCard {
    ProjectCard {
        id: ProjectId::new(format!(
            "project-{}",
            name.split(' ').next().unwrap().to_lowercase()
        ))
        .unwrap(),
        name: name.into(),
        about: about.into(),
        platforms: platforms.to_vec(),
        pool: pool.map(Into::into),
        research,
        created_ms: Some(clock::now_ms() - age_ms),
    }
}

fn projects() -> Vec<ProjectCard> {
    vec![
        card(
            "Tidepool",
            "Live dashboard for marine sensor buoys.",
            &[Platform::Website],
            Some("Main"),
            ResearchCounts {
                done: 2,
                running: 2,
                ..Default::default()
            },
            4 * 60_000 + 5_000,
        ),
        card(
            "Ledgerline",
            "Invoicing for small design studios.",
            &[Platform::Website, Platform::DesktopApp],
            Some("Main"),
            ResearchCounts {
                done: 3,
                ..Default::default()
            },
            DAY_MS,
        ),
        card(
            "Fieldnotes Mobile",
            "Offline field notes for ecologists.",
            &[Platform::MobileApp],
            None,
            ResearchCounts {
                needs_pool: 1,
                ..Default::default()
            },
            4 * DAY_MS,
        ),
    ]
}

fn profile() -> OwnerProfile {
    OwnerProfile {
        name: "Mira Okafor".into(),
        role: "Founder and full-stack developer".into(),
        bio: "Building tools for marine researchers. Ten years of TypeScript, learning Rust."
            .into(),
        preferred_stack: ["TypeScript", "React", "Rust", "Postgres"]
            .map(Into::into)
            .to_vec(),
        working_style: WorkingStyle {
            detail: AnswerDetail::Terse,
            initiative: Initiative::AskFirst,
        },
    }
}

const BIO_80: [(&str, &str); 2] = [
    (
        "Building tools for marine researchers. Ten years of                      ",
        "Building tools for marine researchers. Ten years of TypeScript, learning ",
    ),
    ("TypeScript, learning Rust.", "Rust.                     "),
];

fn old_date() -> String {
    clock::ago(
        clock::now_ms() - 4 * DAY_MS,
        clock::now_ms(),
        clock::local_offset(),
    )
}

fn project_swaps(old: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "Research: 2 done, 1 running",
            "Research: 2 done, 2 running".into(),
        ),
        ("Updated 4 min ago", "Created 4 min ago".into()),
        ("Updated yesterday", "Created yesterday".into()),
        ("Updated Oct 19", format!("Created {old}")),
    ]
}

fn swapped<'a>(swaps: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    swaps
        .iter()
        .map(|(from, to)| (*from, to.as_str()))
        .collect()
}

fn shell_at(ctx: Ctx, place: Place) -> Shell {
    let mut ctx = ctx;
    let screen = open(&place, &mut ctx);
    Shell::with(ctx, screen)
}

#[test]
fn startup_frames_match_the_design() {
    for (width, height) in [(120, 36), (80, 24)] {
        let mut shell = Shell::new(ctx(width, height));
        check(
            &format!("startup-connecting-{width}"),
            &render(&mut shell, width, height),
            &[],
        );
        shell.ctx.link = Link::Down("engine_unavailable".into());
        check(
            &format!("startup-down-{width}"),
            &render(&mut shell, width, height),
            &[],
        );
    }
    let mut shell = Shell::new(ctx(50, 12));
    check("too-small", &render(&mut shell, 50, 12), &[]);
}

#[test]
fn welcome_frames_match_the_design() {
    for (width, height) in [(120, 36), (80, 24)] {
        let mut shell = shell_at(ready(width, height), Place::Welcome);
        keys(&mut shell, "Mira Ok");
        check(
            &format!("welcome-{width}"),
            &render(&mut shell, width, height),
            &[],
        );
    }
    let mut shell = shell_at(ready(80, 24), Place::Welcome);
    for _ in 0..6 {
        shell.key(Key::Tab);
    }
    shell.key(Key::Enter);
    check("welcome-error-80", &render(&mut shell, 80, 24), &[]);

    let fill = |shell: &mut Shell| {
        keys(shell, "Mira Okafor");
        shell.key(Key::Tab);
        keys(shell, "Founder and full-stack developer");
        shell.key(Key::Tab);
        keys(
            shell,
            "Building tools for marine researchers. Ten years of TypeScript, learning Rust.",
        );
        shell.key(Key::Tab);
        keys(shell, "TypeScript, React, Rust, Postgres");
        for _ in 0..3 {
            shell.key(Key::Tab);
        }
    };
    let mut shell = shell_at(ready(120, 36), Place::Welcome);
    fill(&mut shell);
    check("welcome-filled-120", &render(&mut shell, 120, 36), &[]);

    let mut shell = shell_at(ready(80, 24), Place::Welcome);
    fill(&mut shell);
    shell.key(Key::Enter);
    assert!(shell.busy());
    check("welcome-saving-80", &render(&mut shell, 80, 24), &BIO_80);
}

#[test]
fn home_frames_match_the_design() {
    for (width, height) in [(120, 36), (80, 24)] {
        let mut ctx = ready(width, height);
        ctx.store.projects = Load::Ready(Vec::new());
        ctx.store.pools = Load::Ready(Vec::new());
        let mut shell = shell_at(ctx, Place::Home);
        shell.arrive(super::seam::Arrival::Pools(Ok(Vec::new())));
        check(
            &format!("home-first-{width}"),
            &render(&mut shell, width, height),
            &[
                ("[x] Check your harnesses", "[ ] Check your harnesses"),
                ("2 of 9 harnesses ready", "Harness checks are not"),
                ("on this Mac.", "ready yet."),
            ],
        );
    }

    let old = old_date();
    let swaps = project_swaps(&old);
    for (width, height) in [(120, 36), (80, 24)] {
        let mut ctx = ready(width, height);
        ctx.store.projects = Load::Ready(projects());
        ctx.store.pools = Load::Ready(pools());
        let mut shell = shell_at(ctx, Place::Home);
        shell.arrive(super::seam::Arrival::Pools(Ok(pools())));
        check(
            &format!("home-projects-{width}"),
            &render(&mut shell, width, height),
            &swapped(&swaps),
        );
    }

    let mut shell = shell_at(ready(80, 24), Place::Home);
    check("home-loading-80", &render(&mut shell, 80, 24), &[]);

    let mut ctx = ready(80, 24);
    ctx.store.projects = Load::Ready(projects());
    ctx.store.pools = Load::Ready(pools());
    ctx.store.projects_seen = Some("10:42".into());
    let mut shell = shell_at(ctx, Place::Home);
    shell.arrive(super::seam::Arrival::Disconnected(
        "trusted_connection_lost".into(),
    ));
    check(
        "home-disconnected-80",
        &render(&mut shell, 80, 24),
        &swapped(&swaps),
    );

    let mut ctx = ready(80, 24);
    ctx.store.projects = Load::Ready(Vec::new());
    let mut shell = shell_at(ctx, Place::Home);
    shell.arrive(super::seam::Arrival::Pools(Ok(vec![pool("Main", 0, 0, 0)])));
    shell.arrive(super::seam::Arrival::Pools(Ok(vec![pool("Main", 2, 0, 0)])));
    check("home-ready-80", &render(&mut shell, 80, 24), &[]);
}

#[test]
fn home_lists_a_kept_new_project_draft_first_and_enter_continues_it() {
    let mut ctx = ready(80, 24);
    ctx.store.projects = Load::Ready(projects());
    ctx.store.pools = Load::Ready(pools());
    let mut shell = shell_at(ctx, Place::Home);
    shell.arrive(super::seam::Arrival::Pools(Ok(pools())));
    shell.key(Key::Char('n'));
    keys(&mut shell, "Saltmarsh");
    shell.key(Key::Esc);
    shell.key(Key::Esc);
    assert_eq!(shell.place(), Some(Place::Home));
    assert_eq!(kept_draft().as_deref(), Some("Saltmarsh"));

    let rows = render(&mut shell, 80, 24);
    assert!(rows[2].starts_with("  Projects  3 "), "{}", rows[2]);
    assert!(rows[4].starts_with("  ▌ Saltmarsh "), "{}", rows[4]);
    assert!(rows[4].trim_end().ends_with(" Draft"), "{}", rows[4]);
    assert_eq!(rows[5].trim_end(), "  ▌ Not created yet.");
    assert_eq!(rows[6].trim_end(), "  ▌ Enter continues the draft.");
    assert!(rows[8].starts_with("    Tidepool "), "{}", rows[8]);

    shell.key(Key::Char('/'));
    keys(&mut shell, "ledger");
    let rows = render(&mut shell, 80, 24);
    assert!(
        !rows.iter().any(|row| row.contains("Saltmarsh")),
        "the filter hides a draft it does not match"
    );
    assert!(rows.iter().any(|row| row.contains("Ledgerline")));
    shell.key(Key::Esc);

    shell.key(Key::Enter);
    assert_eq!(shell.place(), Some(Place::NewProject));
    assert_eq!(kept_draft(), None, "the wizard took the draft back");
    let rows = render(&mut shell, 80, 24);
    assert!(rows.iter().any(|row| row.contains("Saltmarsh")));
}

#[test]
fn home_shows_a_kept_draft_instead_of_the_empty_state() {
    let mut ctx = ready(80, 24);
    ctx.store.projects = Load::Ready(Vec::new());
    ctx.store.pools = Load::Ready(pools());
    let mut shell = shell_at(ctx, Place::Home);
    shell.arrive(super::seam::Arrival::Pools(Ok(pools())));
    shell.key(Key::Char('n'));
    keys(&mut shell, "Saltmarsh");
    shell.key(Key::Esc);
    shell.key(Key::Esc);

    let rows = render(&mut shell, 80, 24);
    assert_eq!(
        rows[2].trim_end(),
        format!("  Projects{}Most recent first", " ".repeat(51))
    );
    assert!(rows[4].starts_with("  ▌ Saltmarsh "), "{}", rows[4]);
    assert!(!rows.iter().any(|row| row.contains("No projects yet.")));
}

const UNCOUNTED: [(&str, &str); 2] = [
    (
        "@ Accounts 240   ⊞ Pools 3   ❯ Harnesses 2 of 9   ✦ Plugins and skills   ⁂ Memory 41   ◉ Profile",
        "@ Accounts   ⊞ Pools 3   ❯ Harnesses   ✦ Plugins and skills   ⁂ Memory   ◉ Profile",
    ),
    (
        "───────────────────━━━━━━━━━",
        "───────────────━━━━━━━━━────",
    ),
];

#[test]
fn settings_frames_match_the_design() {
    let reach = [
        UNCOUNTED[0],
        UNCOUNTED[1],
        ("1 can't be reached", "1 not signed in"),
    ];
    for (width, height) in [(120, 36), (80, 24)] {
        let mut ctx = ready(width, height);
        ctx.store.pools = Load::Ready(pools());
        let mut projects = projects();
        projects[2].pool = None;
        ctx.store.projects = Load::Ready(projects);
        let mut shell = shell_at(ctx, Place::Settings(SettingsSection::Pools));
        check(
            &format!("pools-{width}"),
            &render(&mut shell, width, height),
            &reach,
        );
    }
    let mut ctx = ready(80, 24);
    ctx.store.pools = Load::Ready(Vec::new());
    let mut shell = shell_at(ctx, Place::Settings(SettingsSection::Pools));
    check("pools-empty-80", &render(&mut shell, 80, 24), &[]);

    for (width, height) in [(120, 36), (80, 24)] {
        let mut ctx = ready(width, height);
        ctx.colors = Colors::FollowMacos;
        ctx.store.profile = Load::Ready(Some(profile()));
        ctx.store.pools = Load::Ready(pools());
        if width == 80 {
            ctx.say(Message::success("Saved.", ""));
        }
        let mut shell = shell_at(ctx, Place::Settings(SettingsSection::Profile));
        let from = format!("{}{}", "─".repeat(89), "━".repeat(9));
        let to = format!("{}{}{}", "─".repeat(75), "━".repeat(9), "─".repeat(14));
        let swaps = [
            UNCOUNTED[0],
            (from.as_str(), to.as_str()),
            BIO_80[0],
            BIO_80[1],
        ];
        check(
            &format!("profile-{width}"),
            &render(&mut shell, width, height),
            &swaps,
        );
    }
}

struct Draft;

impl Screen for Draft {
    fn crumbs(&self) -> Vec<String> {
        vec!["Home".into(), "New project".into(), "Review".into()]
    }

    fn hints(&self, _ctx: &Ctx) -> Vec<super::frame::Hint> {
        Vec::new()
    }

    fn key(&mut self, _key: &Key, _ctx: &mut Ctx) -> Flow {
        Flow::Ignored
    }

    fn draw(&mut self, _frame: &mut ratatui::Frame, _area: Rect, _ctx: &Ctx) -> Option<Position> {
        None
    }

    fn draft(&self) -> bool {
        true
    }
}

#[test]
fn overlay_frames_match_the_design() {
    for (width, height) in [(120, 36), (80, 24)] {
        let mut ctx = ready(width, height);
        ctx.store.projects = Load::Ready(projects());
        ctx.store.pools = Load::Ready(pools());
        let mut shell = shell_at(ctx, Place::Home);
        shell.key(Key::Char('?'));
        check_modal(
            &format!("help-{width}"),
            &render(&mut shell, width, height),
            "Keys",
        );
        shell.key(Key::Esc);
        shell.key(Key::Ctrl('k'));
        keys(&mut shell, "tide");
        check_modal(
            &format!("goto-{width}"),
            &render(&mut shell, width, height),
            "Go to",
        );
        shell.key(Key::Down);
        shell.key(Key::Down);
        shell.key(Key::Enter);
        assert_eq!(shell.depth(), 2);
        assert_eq!(
            shell.place(),
            Some(Place::Project {
                id: ProjectId::new("project-tidepool").unwrap(),
                name: "Tidepool".into(),
                tab: ProjectTab::Agents,
            })
        );
    }

    let mut ctx = ready(80, 24);
    ctx.store.projects = Load::Ready(
        projects()
            .into_iter()
            .map(|mut card| {
                card.research.running = if card.name == "Tidepool" { 3 } else { 0 };
                card
            })
            .collect(),
    );
    let mut shell = Shell::with(ctx, Box::new(Draft));
    shell.key(Key::Ctrl('q'));
    assert!(!shell.quitting());
    check_modal("quit-80", &render(&mut shell, 80, 24), "Quit CrazyAgent?");
    shell.key(Key::Left);
    shell.key(Key::Enter);
    assert!(shell.quitting());
}

const POOL_ROWS: [(&str, &str, &str); 23] = [
    ("Anthropic API - team", "Anthropic API", "Key saved"),
    ("ChatGPT Plus - spare 1", "ChatGPT / Codex", "Signed in"),
    ("ChatGPT Plus - spare 2", "ChatGPT / Codex", "Signed in"),
    ("ChatGPT Plus - spare 3", "ChatGPT / Codex", "Signed in"),
    ("ChatGPT Plus - spare 4", "ChatGPT / Codex", "Not signed in"),
    ("ChatGPT Pro - main", "ChatGPT / Codex", "Signed in"),
    ("ChatGPT Team - seat 01", "ChatGPT / Codex", "Signed in"),
    ("ChatGPT Team - seat 02", "ChatGPT / Codex", "Signed in"),
    ("ChatGPT Team - seat 03", "ChatGPT / Codex", "Signed in"),
    ("Claude Max - work", "Claude", "Signed in"),
    ("Claude Max - work 2", "Claude", "Signed in"),
    ("Claude Pro - personal", "Claude", "Not signed in"),
    ("Claude Team - seat 01", "Claude", "Signed in"),
    ("Claude Team - seat 02", "Claude", "Signed in"),
    ("Claude Team - seat 03", "Claude", "Signed in"),
    ("Claude Team - seat 04", "Claude", "Signed in"),
    ("Claude Team - seat 05", "Claude", "Signed in"),
    ("Claude Team - seat 06", "Claude", "Signed in"),
    ("Claude Team - seat 07", "Claude", "Signed in"),
    ("LM Studio on mini", "Local model", "Can't reach this URL"),
    ("Ollama on this Mac", "Local model", "Connected"),
    ("OpenAI API - billing", "OpenAI API", "Key saved"),
    ("OpenRouter - shared", "OpenRouter", "Key saved"),
];

fn accounts() -> Vec<super::seam::AccountRow> {
    (0..240)
        .map(|index| {
            let (name, provider, status) = POOL_ROWS
                .get(index)
                .map(|(name, provider, status)| (name.to_string(), *provider, *status))
                .unwrap_or_else(|| (format!("Zeta seat {index:03}"), "Claude", "Signed in"));
            super::seam::AccountRow {
                id: CredentialRef::new(format!("account-{index:03}")).unwrap(),
                name,
                provider: provider.into(),
                status: status.into(),
            }
        })
        .collect()
}

fn same_bar(rows: &[String], column: usize) -> Vec<String> {
    rows.iter()
        .map(|row| {
            row.chars()
                .enumerate()
                .map(|(index, character)| {
                    if index == column && character == '█' {
                        '│'
                    } else {
                        character
                    }
                })
                .collect()
        })
        .collect()
}

fn check_without_thumb(name: &str, actual: &[String], width: u16) {
    save(name, actual);
    let column = width as usize - 3;
    let expected = same_bar(&fixture(name, &[]), column);
    let actual = same_bar(actual, column);
    let wrong: Vec<String> = expected
        .iter()
        .zip(&actual)
        .enumerate()
        .filter(|(_, (want, got))| want != got)
        .map(|(row, (want, got))| format!("row {row}\n want |{want}|\n got  |{got}|"))
        .collect();
    assert_eq!(expected.len(), actual.len(), "{name}: row count");
    assert!(wrong.is_empty(), "{name} differs:\n{}", wrong.join("\n"));
}

fn pick(shell: &mut Shell, indexes: &[usize]) {
    shell.key(Key::Home);
    let mut at = 0;
    for index in indexes {
        while at < *index {
            shell.key(Key::Down);
            at += 1;
        }
        shell.key(Key::Char(' '));
    }
}

#[test]
fn pool_editor_frames_match_the_design() {
    for (width, height) in [(120, 36), (80, 24)] {
        let mut ctx = ready(width, height);
        ctx.store.accounts = Load::Ready(accounts());
        ctx.store.pools = Load::Ready(pools());
        let mut shell = shell_at(ctx, Place::NewPool);
        keys(&mut shell, "Build team");
        shell.key(Key::Tab);
        pick(&mut shell, &[0, 1, 5, 6, 9, 12, 13]);
        shell.key(Key::Home);
        shell.key(Key::Down);
        shell.key(Key::Down);
        check_without_thumb(
            &format!("pool-editor-{width}"),
            &render(&mut shell, width, height),
            width,
        );
    }

    let mut ctx = ready(80, 24);
    ctx.store.accounts = Load::Ready(accounts());
    ctx.store.pools = Load::Ready(pools());
    let mut shell = shell_at(ctx, Place::NewPool);
    keys(&mut shell, "Build team");
    shell.key(Key::Tab);
    pick(&mut shell, &(0..16).collect::<Vec<_>>());
    check_without_thumb("pool-editor-full-80", &render(&mut shell, 80, 24), 80);
}

fn styled(
    depth: Depth,
    colors: Colors,
    appearance: Appearance,
    glyphs: &'static Glyphs,
    width: u16,
    height: u16,
) -> Ctx {
    let mut ctx = Ctx::new(
        depth,
        colors,
        appearance,
        glyphs,
        "/Users/mira/.crazyagent/engine.sock".into(),
    );
    ctx.home = Some("/Users/mira".into());
    ctx.width = width;
    ctx.height = height;
    ctx.tick = 4;
    ctx.link = Link::Ready;
    ctx.store.projects = Load::Ready(projects());
    ctx.store.pools = Load::Ready(pools());
    ctx.store.profile = Load::Ready(Some(profile()));
    ctx
}

fn buffer(shell: &mut Shell, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| shell.draw(frame)).unwrap();
    terminal.backend().buffer().clone()
}

fn scenes(make: impl Fn() -> Ctx) -> Vec<(&'static str, Shell)> {
    let mut connecting = make();
    connecting.link = Link::Connecting;
    let mut down = make();
    down.link = Link::Down("engine_unavailable".into());
    let mut welcome = shell_at(make(), Place::Welcome);
    keys(&mut welcome, "Mira Ok");
    let mut home = make();
    home.say(Message::error(
        "Lost the connection to the engine.",
        "Retrying in 3 s.",
    ));
    let mut help = shell_at(make(), Place::Home);
    help.key(Key::Char('?'));
    let mut go = shell_at(make(), Place::Home);
    go.key(Key::Ctrl('k'));
    keys(&mut go, "po");
    let mut delete = shell_at(make(), Place::Settings(SettingsSection::Pools));
    delete.key(Key::Down);
    delete.key(Key::Char('d'));
    let mut saved = make();
    saved.say(Message::success("Saved.", ""));
    vec![
        ("startup-connecting", Shell::new(connecting)),
        ("startup-down", Shell::new(down)),
        ("welcome", welcome),
        ("home", shell_at(home, Place::Home)),
        ("help", help),
        ("goto", go),
        (
            "pools",
            shell_at(make(), Place::Settings(SettingsSection::Pools)),
        ),
        ("pool-delete", delete),
        (
            "profile",
            shell_at(saved, Place::Settings(SettingsSection::Profile)),
        ),
        ("pool-editor", shell_at(make(), Place::NewPool)),
    ]
}

fn non_ascii(cells: &Buffer, width: u16) -> Vec<String> {
    cells
        .content()
        .iter()
        .enumerate()
        .filter(|(_, cell)| !cell.symbol().is_ascii())
        .map(|(index, cell)| {
            let width = usize::from(width);
            format!("({}, {}) {:?}", index % width, index / width, cell.symbol())
        })
        .collect()
}

#[test]
fn ascii_terminals_get_only_ascii_on_every_screen() {
    for (width, height) in [(120, 36), (80, 24)] {
        let scenes = scenes(|| {
            styled(
                Depth::NoColor,
                Colors::Dark,
                Appearance::Dark,
                &ASCII,
                width,
                height,
            )
        });
        assert_eq!(scenes.len(), 10);
        for (name, mut shell) in scenes {
            let cells = buffer(&mut shell, width, height);
            let wrong = non_ascii(&cells, width);
            assert!(
                wrong.is_empty(),
                "{name} at {width}: non-ASCII cells {wrong:?}"
            );
            let drawn = cells
                .content()
                .iter()
                .filter(|cell| cell.symbol() != " ")
                .count();
            assert!(drawn > 40, "{name} at {width} drew almost nothing");
        }
    }
    let mut unicode = shell_at(
        styled(
            Depth::NoColor,
            Colors::Dark,
            Appearance::Dark,
            &UNICODE,
            80,
            24,
        ),
        Place::Home,
    );
    assert!(
        !non_ascii(&buffer(&mut unicode, 80, 24), 80).is_empty(),
        "the check must see Unicode glyphs when they are drawn"
    );
}

fn theme_colors(ctx: &Ctx) -> Vec<Color> {
    let theme = &ctx.theme;
    let mut styles = vec![
        theme.text,
        theme.heading,
        theme.muted,
        theme.accent,
        theme.mark,
        theme.success,
        theme.warning,
        theme.error,
        theme.running,
        theme.selection,
        theme.selection_dim,
        theme.backdrop,
    ];
    styles.extend(theme.paint);
    let mut colors = vec![Color::Reset];
    for style in styles {
        colors.extend(style.fg);
        colors.extend(style.bg);
    }
    colors
}

#[test]
fn every_cell_uses_only_theme_colors_at_every_depth() {
    let cases = [
        (Depth::TrueColor, Colors::Dark, Appearance::Dark),
        (Depth::TrueColor, Colors::Light, Appearance::Dark),
        (Depth::TrueColor, Colors::Terminal, Appearance::Dark),
        (Depth::Ansi256, Colors::Dark, Appearance::Dark),
        (Depth::Ansi256, Colors::FollowMacos, Appearance::Light),
        (Depth::Ansi16, Colors::Dark, Appearance::Dark),
        (Depth::NoColor, Colors::Light, Appearance::Light),
    ];
    for (depth, colors, appearance) in cases {
        for (name, mut shell) in scenes(|| styled(depth, colors, appearance, &UNICODE, 80, 24)) {
            let allowed = theme_colors(&shell.ctx);
            let painted = shell.ctx.theme.paint.is_some();
            assert_eq!(
                painted,
                matches!(depth, Depth::TrueColor | Depth::Ansi256) && colors != Colors::Terminal
            );
            let cells = buffer(&mut shell, 80, 24);
            for (index, cell) in cells.content().iter().enumerate() {
                let label = format!(
                    "{name} {depth:?} {colors:?} {appearance:?} at ({}, {})",
                    index % 80,
                    index / 80
                );
                for color in [cell.fg, cell.bg] {
                    assert!(
                        allowed.contains(&color),
                        "{label}: {color:?} is not a theme color"
                    );
                    let fits = match depth {
                        Depth::NoColor => color == Color::Reset,
                        Depth::Ansi16 => !matches!(color, Color::Rgb(..) | Color::Indexed(_)),
                        Depth::Ansi256 => !matches!(color, Color::Rgb(..)),
                        Depth::TrueColor => !matches!(color, Color::Indexed(_)),
                    };
                    assert!(fits, "{label}: {color:?} does not fit the depth");
                }
                if painted {
                    assert_ne!(cell.bg, Color::Reset, "{label}: cell left unpainted");
                }
            }
        }
    }
}

fn pool_rows(rows: &[String]) -> Vec<String> {
    rows.iter()
        .filter(|row| row.contains("Pool "))
        .cloned()
        .collect()
}

fn bar(rows: &[String]) -> String {
    pool_rows(rows)
        .iter()
        .map(|row| row.chars().nth(77).unwrap())
        .collect()
}

#[test]
fn a_long_pool_list_scrolls_with_the_selection_and_shows_a_bar() {
    let many: Vec<PoolSummary> = (0..30)
        .map(|index| pool(&format!("Pool {index:02}"), 1, 0, 0))
        .collect();
    let mut ctx = ready(80, 24);
    ctx.store.pools = Load::Ready(many);
    ctx.store.projects = Load::Ready(Vec::new());
    let mut shell = shell_at(ctx, Place::Settings(SettingsSection::Pools));

    let rows = render(&mut shell, 80, 24);
    let shown = pool_rows(&rows);
    assert_eq!(shown.len(), 14, "the body holds 14 rows under the heading");
    assert!(shown[0].contains("> Pool 00"));
    assert!(shown[13].contains("Pool 13"));
    assert_eq!(bar(&rows), format!("{}{}", "█".repeat(7), "│".repeat(7)));

    shell.key(Key::PageDown);
    let rows = render(&mut shell, 80, 24);
    let shown = pool_rows(&rows);
    assert!(shown[13].contains("> Pool 13"), "a page is 13 rows");
    assert!(shown[0].contains("Pool 00"));

    shell.key(Key::Down);
    let rows = render(&mut shell, 80, 24);
    let shown = pool_rows(&rows);
    assert!(shown[0].contains("Pool 01"), "the window follows down");
    assert!(shown[13].contains("> Pool 14"));

    shell.key(Key::End);
    let rows = render(&mut shell, 80, 24);
    let shown = pool_rows(&rows);
    assert_eq!(shown.len(), 14);
    assert!(shown[0].contains("Pool 16"));
    assert!(shown[13].contains("> Pool 29"));
    assert_eq!(bar(&rows), format!("{}{}", "│".repeat(7), "█".repeat(7)));

    shell.key(Key::PageUp);
    let rows = render(&mut shell, 80, 24);
    let shown = pool_rows(&rows);
    assert!(shown[0].contains("> Pool 16"), "PgUp moves 13 rows up");

    shell.key(Key::Home);
    let rows = render(&mut shell, 80, 24);
    assert!(pool_rows(&rows)[0].contains("> Pool 00"));
}

#[test]
fn a_profile_read_failure_is_not_shown_as_a_stopped_engine() {
    let mut shell = Shell::new(ready(80, 24));
    shell.arrive(super::seam::Arrival::Connected(None));
    shell.arrive(super::seam::Arrival::Profile(Err(
        "owner_profile_read".into()
    )));
    let text = render(&mut shell, 80, 24).join("\n");
    assert!(text.contains("Could not read your profile"));
    assert!(text.contains("Press r to try again."));
    assert!(text.contains("r Try again"));
    assert!(!text.contains("engine isn't running"));
    assert!(!text.contains("Engine not running"));
}
