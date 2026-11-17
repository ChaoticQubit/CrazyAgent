use super::intake::{ask, copy, event, not_ready};
use super::intake_job::{self, JobAction};
use super::new_project::display_path;
use super::new_project_page::kept_draft;
use super::settings_accounts::{load_accounts, plain, send};
use super::{Flow, Place, ProjectTab, Screen, SettingsSection};
use crate::accounts::AccountStatus;
use crate::learning::{LearningProgress, MarkStep, StepId, Topic};
use crate::pools::PoolSummary;
use crate::projects_home::job::{JobKind, JobView};
use crate::projects_home::{FoundProject, FoundState, ProjectsHome};
use crate::setup::{Dependency, DependencyState, SetupStatus};
use crate::tui::client::OwnerEvent;
use crate::tui::client::app_setup::AppSetupJob;
use crate::tui::client::intake::{IntakeQuery, IntakeView, start_id};
use crate::tui::client::learning::LearningQuery;
use crate::tui::client::settings::SettingsJob;
use crate::tui::clock;
use crate::tui::ctx::{Ctx, Link, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::{AccountRow, Arrival, ProjectCard, Request};
use crate::tui::text::{cut, join, pad, restyle, row, status, styled, width, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::TextField;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};

mod screen;

const CARD_ROWS: usize = 4;
const PANEL_WIDE: usize = 38;
const PANEL_NARROW: usize = 30;
const FILTER_CHARS_MAX: usize = 100;
const TOOLS_TEXT_CHARS: usize = 24;
const SETUP_ITEMS: usize = 4;
const TOPICS: [(Topic, &str); 4] = [
    (Topic::Basics, "Basics"),
    (Topic::CoreLoop, "Core loop"),
    (Topic::IdeAndTerminals, "IDE and terminals"),
    (Topic::ProcessMetricsUsage, "Process, Metrics and Usage"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Panel {
    Projects,
    Setup,
}

/// Where Enter on a row of Home's side panel goes: a place, or a step only the desktop app
/// has, which Enter explains instead of opening.
#[derive(Clone, Debug, PartialEq)]
enum Target {
    Place(Place),
    Desktop,
}

/// One row of Home's side panel: a setup item or a learning step, whether it is done, its
/// title and wrapped words, what Enter says and does, and whether the progress line counts it.
#[derive(Clone, Debug)]
struct GuideItem {
    done: bool,
    title: String,
    body: Vec<String>,
    action: String,
    target: Target,
    counted: bool,
}

/// Home's side panel: "Get set up" with the setup items, or "Learn CrazyAgent" with the
/// setup items under Basics and the engine's learning steps under their topics.
#[derive(Clone, Debug)]
struct Guide {
    learning: bool,
    groups: Vec<(Option<&'static str>, Vec<GuideItem>)>,
}

impl Guide {
    /// Every row in panel order. Input: none. Output: the rows.
    fn items(&self) -> impl Iterator<Item = &GuideItem> {
        self.groups.iter().flat_map(|(_, items)| items)
    }

    /// The counted rows done and in all; the harness check is not counted, as on the
    /// desktop. Input: none. Output: done and total.
    fn progress(&self) -> (usize, usize) {
        self.items()
            .filter(|item| item.counted)
            .fold((0, 0), |(done, total), item| {
                (done + usize::from(item.done), total + 1)
            })
    }
}

/// True while the learning guide has something to show: the owner has not hidden it and a
/// step is not done. Input: the learning read. Output: whether the guide shows.
fn learning_open(progress: &LearningProgress) -> bool {
    !progress.skipped && progress.steps.iter().any(|step| !step.done())
}

/// Says where Enter on one learning step goes in the TUI: the first project's page for the
/// project steps, New project while there is none, Settings or Usage for the rest. The IDE
/// is in the desktop app only. Inputs: the step and the first project. Output: the words
/// Enter shows and its target.
fn step_target(id: StepId, project: Option<&ProjectCard>) -> (&'static str, Target) {
    let new_project = ("New project", Target::Place(Place::NewProject));
    let tab = |action: &'static str, page: ProjectTab| match project {
        Some(card) => (
            action,
            Target::Place(Place::Project {
                id: card.id.clone(),
                name: card.name.clone(),
                tab: page,
            }),
        ),
        None => new_project.clone(),
    };
    match id {
        StepId::Project => new_project.clone(),
        StepId::Master => tab("Open conversation", ProjectTab::Conversation),
        StepId::Agents => tab("Open agents", ProjectTab::Agents),
        StepId::Question => tab("Open questions", ProjectTab::Questions),
        StepId::ChangeSet => tab("Open changes", ProjectTab::Changes),
        StepId::Ide => ("Open IDE", Target::Desktop),
        StepId::Terminal => match project {
            Some(card) => (
                "Open team",
                Target::Place(Place::Team {
                    id: card.id.clone(),
                    name: card.name.clone(),
                }),
            ),
            None => new_project.clone(),
        },
        StepId::Process => (
            "Open process",
            Target::Place(Place::Settings(SettingsSection::Process)),
        ),
        StepId::Metrics | StepId::Usage => ("Open usage", Target::Place(Place::Usage)),
    }
}

/// Hides the learning guide until the owner asks to see it again: sets the engine's skipped
/// mark. Input: the context. Output: the generation the mark went with; the panel goes once
/// the next learning read, which a set mark asks for, shows the mark.
fn hide(ctx: &mut Ctx) -> u64 {
    let generation = ctx.next_generation();
    ctx.request(Request::Learning(Box::new(LearningQuery::Mark {
        step: MarkStep::Skipped,
        on: true,
        generation,
    })));
    generation
}

/// One row of the project list: the New project draft kept after leaving the
/// wizard, a projects folder entry that is not a ready project, or a project.
/// Input: the engine's intake job, the kept draft, the found folders and the store. Output:
/// drawn as a four-row card; Enter does the job's first action, continues the draft, sets up
/// the folder or opens the project.
enum Entry {
    Job(JobView),
    Draft(String),
    Folder(FoundProject),
    Project(ProjectCard),
}

/// Setup progress shown in the "Get set up" panel.
/// Input: engine pools, accounts and whether the tools are found. Output: which
/// items are done and what Add an account says; the panel hides when an account
/// is ready for agents, a pool exists and the tools are found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Setup {
    pub ready_accounts: Vec<String>,
    pub accounts: usize,
    pub pool_exists: bool,
    pub tools_ready: bool,
}

impl Setup {
    /// Reads setup progress from the pool list, the account list and the tools
    /// check. An account is ready for agents when it is signed in (a reachable
    /// URL or local model counts) or has a saved key.
    /// Input: the pools, the accounts and whether every tool is found. Output:
    /// whether a ready account, a pool and the tools exist.
    pub fn of(pools: &[PoolSummary], accounts: &[AccountRow], tools_ready: bool) -> Self {
        Self {
            ready_accounts: accounts
                .iter()
                .filter(|account| ready_for_agents(account))
                .map(|account| account.name.clone())
                .collect(),
            accounts: accounts.len(),
            pool_exists: !pools.is_empty(),
            tools_ready,
        }
    }

    /// True when at least one account is ready for agents.
    /// Input: none. Output: whether Add an account is done.
    pub fn account_ready(&self) -> bool {
        !self.ready_accounts.is_empty()
    }

    /// What Add an account says: the account ready for agents, how many are,
    /// that none is yet, or how to add the first one. A name is cut to one
    /// panel line so the text stays within three lines.
    /// Input: the ellipsis glyph. Output: the sentence.
    pub fn account_line(&self, ellipsis: &str) -> String {
        match self.ready_accounts.as_slice() {
            [one] => format!(
                "{} is ready for agents.",
                cut(one, TOOLS_TEXT_CHARS, ellipsis)
            ),
            [] if self.accounts == 0 => {
                "Sign in with ChatGPT or Claude, or add an API key or local model.".into()
            }
            [] => "No account is ready for agents yet. Sign one in or check its key or URL.".into(),
            many => format!("{} accounts are ready for agents.", many.len()),
        }
    }

    /// True when nothing is left to set up.
    /// Input: none. Output: whether the panel hides.
    pub fn done(&self) -> bool {
        self.account_ready() && self.pool_exists && self.tools_ready
    }
}

/// True when agents can use the account: signed in (a reachable URL or local
/// model counts) or with a saved key.
/// Input: the account row. Output: whether it is ready for agents.
fn ready_for_agents(account: &AccountRow) -> bool {
    account.status == AccountStatus::SignedIn.label()
        || account.status == AccountStatus::KeySaved.label()
}

/// True while the engine still checks a tool's version in the background.
/// Input: the setup status. Output: whether any tool reads Checking.
fn checking(status: &SetupStatus) -> bool {
    status
        .dependencies
        .iter()
        .any(|tool| tool.state == DependencyState::Checking)
}

/// Names the tools the setup check did not find, in its order, leaving out Node, which
/// only npm harnesses need. Home's short line calls the Graphify command just Graphify, as
/// there is no pack row beside it. Input: the setup status. Output: the names of tools not
/// found, bundled or installed.
fn missing_tools(status: &SetupStatus) -> Vec<String> {
    unready(status).into_iter().map(home_name).collect()
}

/// The tools Get set up still waits for, in the setup check's order, leaving out Node.
/// Input: the setup status. Output: the rows not found, bundled or installed.
fn unready(status: &SetupStatus) -> Vec<&crate::setup::DependencyStatus> {
    status
        .dependencies
        .iter()
        .filter(|tool| tool.name != Dependency::Node)
        .filter(|tool| {
            !matches!(
                tool.state,
                DependencyState::Found { .. }
                    | DependencyState::Bundled { .. }
                    | DependencyState::System { .. }
                    | DependencyState::Installed { .. }
            )
        })
        .collect()
}

fn home_name(tool: &crate::setup::DependencyStatus) -> String {
    match tool.name {
        Dependency::Graphify => "Graphify".into(),
        _ => tool.label.clone(),
    }
}

/// Home's line for the tools Get set up waits for: the missing ones, then the ones found and
/// not used (DEC-162), which only setup can fix. Input: the setup status. Output: the line,
/// empty when every tool is ready.
fn tools_line(status: &SetupStatus) -> String {
    let (unused, missing): (Vec<_>, Vec<_>) = unready(status)
        .into_iter()
        .partition(|tool| matches!(tool.state, DependencyState::Refused { .. }));
    let names = |tools: Vec<&crate::setup::DependencyStatus>| {
        tools
            .into_iter()
            .map(home_name)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut parts = Vec::new();
    if !missing.is_empty() {
        parts.push(format!("Missing: {}.", names(missing)));
    }
    if !unused.is_empty() {
        parts.push(format!("Found, not used: {}.", names(unused)));
    }
    parts.join(" ")
}

/// Home: the project list and, until setup is done, the "Get set up" panel.
/// Input: keys, project and pool arrivals. Output: project cards with status,
/// research progress and pool; Enter opens a project or a Settings section.
pub struct Home {
    panel: Panel,
    selected: usize,
    offset: usize,
    setup_selected: usize,
    filter: Option<TextField>,
    filtering: bool,
    was_done: Option<bool>,
    owner_moved: bool,
    hiding: Option<u64>,
    folder: Load<ProjectsHome>,
    found: Load<Vec<FoundProject>>,
    sent: Option<IntakeQuery>,
    job_shown: bool,
}

impl Default for Home {
    fn default() -> Self {
        Self::new()
    }
}

/// The lead said when Home could not start a job of `kind` again. Input: the kind. Output:
/// the lead.
fn retry_failed(kind: JobKind) -> &'static str {
    match kind {
        JobKind::Add => "Adding the folder did not start again.",
        JobKind::MoveProject => "Moving the project did not start again.",
        JobKind::Delete => "Deleting the project did not start again.",
        JobKind::Setup => "Setting up the project did not start again.",
    }
}

/// The lead said when a request Home sent for the engine's job failed. Input: the request, a
/// stop, a start again or a dismiss. Output: the lead.
fn job_failed(query: &IntakeQuery) -> &'static str {
    match query {
        IntakeQuery::JobStop { .. } => "The folder did not stop.",
        IntakeQuery::JobStart { subject, .. } => retry_failed(subject.kind()),
        IntakeQuery::JobDismiss { .. } => "The ended add was not dismissed.",
        other => panic!("Home sends no {other:?} for the intake job"),
    }
}

/// The Home card's corner for unseen team notifications and suggested memories, both
/// shown with notifications first; a card too narrow for "new team" drops those words so
/// both counts still fit. Inputs: the two counts, not both 0, and the card width. Output:
/// the text.
fn activity_corner(notifications: usize, suggestions: usize, width: usize) -> String {
    assert!(notifications > 0 || suggestions > 0);
    let memories = (suggestions > 0).then(|| {
        format!(
            "{suggestions} suggested memor{}",
            if suggestions == 1 { "y" } else { "ies" }
        )
    });
    let corner = |team: &str| {
        let notices = (notifications > 0).then(|| {
            format!(
                "{notifications} {team}notification{}",
                if notifications == 1 { "" } else { "s" }
            )
        });
        [notices, memories.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ")
    };
    let long = corner("new team ");
    if long.chars().count() <= width {
        return long;
    }
    corner("")
}

#[cfg(test)]
pub(in crate::tui) mod tests {
    use super::*;
    use crate::learning::LearningProgress;
    use crate::pools::{MemberKind, MemberStatus, PoolMember};
    use crate::protocol::CredentialRef;
    use crate::protocol::ProjectId;
    use crate::setup::{Dependency, DependencyStatus, Version};
    use crate::terminal::{AgentId, TerminalFrame};
    use crate::tui::client::OwnerEvent;
    use crate::tui::client::app_setup::AppSetupEvent;
    use crate::tui::client::learning::{LearningEvent, LearningReply};
    use crate::tui::client::settings::SettingsEvent;
    use crate::tui::screens::settings_accounts::tests::{ctx, render};
    use crate::tui::shell::Shell;

    fn pool(name: &str, statuses: &[MemberStatus]) -> PoolSummary {
        PoolSummary {
            id: CredentialRef::new(format!("pool-{name}")).unwrap(),
            name: name.into(),
            summary: String::new(),
            members: statuses
                .iter()
                .enumerate()
                .map(|(index, status)| PoolMember {
                    id: CredentialRef::new(format!("member-{index}")).unwrap(),
                    kind: MemberKind::Chatgpt,
                    provider: "ChatGPT".into(),
                    status: *status,
                    status_label: String::new(),
                })
                .collect(),
            project_count: 0,
            provider_quota_policy: crate::config::ProviderQuotaPolicy::default(),
        }
    }

    /// A setup check where every tool is found except the `missing` ones.
    /// Input: the tools to report missing. Output: the status, with no pack entries.
    pub(in crate::tui) fn tools(missing: &[Dependency]) -> SetupStatus {
        SetupStatus {
            dependencies: Dependency::ALL
                .iter()
                .map(|name| DependencyStatus {
                    name: *name,
                    label: name.label().into(),
                    state: if missing.contains(name) {
                        DependencyState::Missing {
                            line: name.not_found().line(),
                        }
                    } else {
                        DependencyState::Found {
                            version: Version::parse("1.0.0").unwrap(),
                        }
                    },
                })
                .collect(),
            pack: Vec::new(),
            running: false,
            notes: Vec::new(),
            version: 0,
            refused_tool: Default::default(),
        }
    }

    fn status(status: SetupStatus) -> Arrival {
        Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
            SettingsEvent::AppSetup(AppSetupEvent::Status(Ok(status))),
        ))))
    }

    fn status_reads(shell: &mut Shell) -> usize {
        std::iter::from_fn(|| shell.ctx.next_request())
            .filter(|request| {
                matches!(
                    request,
                    Request::Settings(job)
                        if matches!(job.as_ref(), SettingsJob::AppSetup(AppSetupJob::Status))
                )
            })
            .count()
    }

    fn account(index: usize, name: &str, status: AccountStatus) -> AccountRow {
        AccountRow {
            id: CredentialRef::new(format!("account-{index}")).unwrap(),
            name: name.into(),
            provider: "Claude".into(),
            status: status.label().into(),
            no_window: None,
        }
    }

    /// A Home on an engine with no account and no pool whose setup check has not landed.
    /// Input: none. Output: the shell on Home with its project list focused.
    fn unchecked_home() -> Shell {
        let mut ctx = ctx(80, 24);
        ctx.store.projects = Load::Ready(Vec::new());
        ctx.store.pools = Load::Ready(Vec::new());
        ctx.store.accounts = Load::Ready(Vec::new());
        let mut shell = Shell::with(ctx, Box::new(Home::new()));
        let footer = render(&mut shell, 80, 24).pop().unwrap();
        assert!(footer.contains("Enter Open"), "{footer}");
        shell
    }

    #[test]
    fn with_no_key_pressed_home_moves_the_focus_to_setup_once_the_check_lands() {
        let mut shell = unchecked_home();
        shell.arrive(status(tools(&[])));
        let footer = render(&mut shell, 80, 24).pop().unwrap();
        assert!(footer.contains("Enter Add an account"), "{footer}");
    }

    #[test]
    fn a_key_that_moves_home_before_the_setup_check_lands_keeps_the_owner_s_focus() {
        let mut shell = unchecked_home();
        shell.key(Key::Tab);
        shell.key(Key::Tab);
        shell.arrive(status(tools(&[])));
        let footer = render(&mut shell, 80, 24).pop().unwrap();
        assert!(
            footer.contains("Enter Open"),
            "the owner's Tab back to the projects holds: {footer}"
        );

        let mut shell = unchecked_home();
        shell.key(Key::Char('/'));
        shell.key(Key::Char('x'));
        shell.arrive(status(tools(&[])));
        shell.key(Key::Enter);
        let footer = render(&mut shell, 80, 24).pop().unwrap();
        assert!(
            footer.contains("Enter Open"),
            "the filter the owner typed keeps the project list: {footer}"
        );
    }

    #[test]
    fn setup_is_done_with_an_account_ready_for_agents_a_pool_and_tools_found() {
        assert!(!Setup::of(&[], &[], true).done());
        let empty = Setup::of(&[pool("Main", &[])], &[], true);
        assert!(empty.pool_exists && !empty.account_ready() && !empty.done());
        assert_eq!(
            empty.account_line("…"),
            "Sign in with ChatGPT or Claude, or add an API key or local model."
        );

        let pools = [pool("Main", &[MemberStatus::SetUp, MemberStatus::SignedIn])];
        let waiting = [
            account(1, "ChaoticQubit", AccountStatus::NotSignedIn),
            account(2, "ChaoticQubit", AccountStatus::SigningIn),
            account(3, "Qwen 2.5 - 7B", AccountStatus::Unreachable),
        ];
        let none = Setup::of(&pools, &waiting, true);
        assert!(
            !none.account_ready() && !none.done(),
            "pool members count for nothing while no account is ready"
        );
        assert_eq!(
            none.account_line("…"),
            "No account is ready for agents yet. Sign one in or check its key or URL."
        );
        assert_eq!(wrap(&none.account_line("…"), TOOLS_TEXT_CHARS).len(), 3);

        let one = [
            waiting[0].clone(),
            account(4, "Qwen 2.5 - 7B", AccountStatus::SignedIn),
        ];
        let ready = Setup::of(&[pool("Empty", &[])], &one, true);
        assert!(
            ready.done(),
            "an account outside every pool still counts as ready for agents"
        );
        assert_eq!(
            ready.account_line("…"),
            "Qwen 2.5 - 7B is ready for agents."
        );
        let long = Setup::of(
            &[],
            &[account(6, &"Seat ".repeat(19), AccountStatus::SignedIn)],
            true,
        );
        assert_eq!(
            long.account_line("…"),
            "Seat Seat Seat Seat Sea… is ready for agents."
        );
        assert_eq!(wrap(&long.account_line("…"), TOOLS_TEXT_CHARS).len(), 2);
        assert!(!Setup::of(&[pool("Empty", &[])], &one, false).done());
        let two = [
            one[1].clone(),
            account(5, "Team key", AccountStatus::KeySaved),
        ];
        assert_eq!(
            Setup::of(&pools, &two, true).account_line("…"),
            "2 accounts are ready for agents."
        );
    }

    #[test]
    fn home_reads_accounts_on_each_refresh_only_until_one_is_ready_for_agents() {
        let account_reads = |shell: &mut Shell| {
            std::iter::from_fn(|| shell.ctx.next_request())
                .filter(|request| {
                    matches!(
                        request,
                        Request::Settings(job) if matches!(job.as_ref(), SettingsJob::AllAccounts)
                    )
                })
                .count()
        };
        let mut ctx = ctx(80, 24);
        ctx.store.accounts =
            Load::Ready(vec![account(1, "ChaoticQubit", AccountStatus::NotSignedIn)]);
        let mut shell = Shell::with(ctx, Box::new(Home::new()));
        assert_eq!(account_reads(&mut shell), 1);
        shell.refresh();
        assert_eq!(
            account_reads(&mut shell),
            1,
            "while no account is ready, each refresh reads them again"
        );
        shell.ctx.store.accounts =
            Load::Ready(vec![account(2, "Team key", AccountStatus::KeySaved)]);
        shell.refresh();
        assert_eq!(
            account_reads(&mut shell),
            0,
            "a ready account is not read again on every refresh"
        );
    }

    #[test]
    fn found_and_installed_tools_count_and_every_other_state_is_missing() {
        let mut status = tools(&[]);
        assert_eq!(missing_tools(&status), Vec::<String>::new());
        status.dependencies[0].state = DependencyState::Installed {
            version: Version::parse("3.5.0").unwrap(),
        };
        status.dependencies[1].state = DependencyState::Installing;
        status.dependencies[2].state = DependencyState::Failed {
            reason: "setup_uv_missing".into(),
        };
        assert_eq!(missing_tools(&status), ["uv", "Graphify"]);
        status.dependencies[1].state = DependencyState::Checking;
        assert_eq!(missing_tools(&status), ["uv", "Graphify"]);
    }

    /// DEC-157, desktop parity with toolsStep: a tmux the app ships counts as installed, so
    /// Get set up never asks to install tmux on a Mac that has it bundled.
    #[test]
    fn a_bundled_tmux_counts_as_installed() {
        let mut status = tools(&[]);
        status.dependencies[0].state = DependencyState::Bundled {
            version: Version::parse("3.8").unwrap(),
        };
        assert_eq!(missing_tools(&status), Vec::<String>::new());
    }

    /// Leader 20:08: a system tmux used in place of the bundled one is a working tmux, so it
    /// counts as installed like the desktop's Install tools step.
    #[test]
    fn a_system_tmux_in_place_of_the_bundled_one_counts_as_installed() {
        let mut status = tools(&[]);
        status.dependencies[0].state = DependencyState::System {
            version: Version::parse("3.5a").unwrap(),
            note: crate::tools::BundleNote::Missing.line().to_owned(),
        };
        assert_eq!(missing_tools(&status), Vec::<String>::new());
    }

    /// DEC-162: a tool found and not used keeps Get set up open, since agents cannot use it,
    /// and Home names it as not used, never as missing.
    #[test]
    fn a_tool_found_and_not_used_keeps_get_set_up_and_reads_not_used_not_missing() {
        let mut status = tools(&[Dependency::Uv]);
        status.dependencies[2].state = DependencyState::Refused {
            line: "graphify can't start".into(),
            file: "/h/g/bin/python3".into(),
            part: None,
            fix: None,
            fix_failed: None,
        };
        assert_eq!(missing_tools(&status), ["uv", "Graphify"]);
        assert_eq!(
            tools_line(&status),
            "Missing: uv. Found, not used: Graphify."
        );
        status.dependencies[1].state = DependencyState::Found {
            version: Version::parse("0.13.5").unwrap(),
        };
        assert_eq!(tools_line(&status), "Found, not used: Graphify.");
        assert_eq!(tools_line(&tools(&[])), "");
    }

    #[test]
    fn node_is_not_a_tool_home_waits_for_as_only_npm_harnesses_need_it() {
        assert_eq!(
            missing_tools(&tools(&[Dependency::Node])),
            Vec::<String>::new()
        );
        assert_eq!(
            missing_tools(&tools(&[Dependency::Uv, Dependency::Node])),
            ["uv"]
        );
    }

    #[test]
    fn get_set_up_stays_until_tools_are_found_and_opens_setup() {
        let mut ctx = ctx(80, 24);
        ctx.store.projects = Load::Ready(Vec::new());
        ctx.store.pools = Load::Ready(vec![pool("Main", &[MemberStatus::SetUp])]);
        ctx.store.accounts = Load::Ready(vec![account(1, "Team key", AccountStatus::KeySaved)]);
        let mut shell = Shell::with(ctx, Box::new(Home::new()));
        assert_eq!(status_reads(&mut shell), 1, "Home asks for the setup check");
        let rows = render(&mut shell, 80, 24);
        assert!(
            rows.iter().any(|row| row.contains("Get set up")),
            "{rows:#?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("[ ] Install tools")),
            "checking counts as not done: {rows:#?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("Agents need tmux, uv and")),
            "{rows:#?}"
        );
        assert!(
            rows.iter()
                .all(|row| !row.contains("Failed") && !row.contains("setup_")),
            "{rows:#?}"
        );

        let mut still = tools(&[]);
        still.dependencies[0].state = DependencyState::Checking;
        shell.arrive(status(still));
        let rows = render(&mut shell, 80, 24);
        assert!(
            rows.iter().any(|row| row.contains("[ ] Install tools")),
            "{rows:#?}"
        );
        assert!(
            rows.iter().all(|row| !row.contains("Missing")),
            "a tool still being checked is not called missing: {rows:#?}"
        );
        shell.refresh();
        assert_eq!(
            status_reads(&mut shell),
            1,
            "Home reads the check again until it finishes"
        );

        shell.arrive(status(tools(&[Dependency::Uv, Dependency::Graphify])));
        assert!(matches!(shell.ctx.store.setup, Load::Ready(_)));
        let rows = render(&mut shell, 80, 24);
        assert!(
            rows.iter().any(|row| row.contains("[ ] Install tools")),
            "{rows:#?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("Missing: uv, Graphify.")),
            "{rows:#?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("[x] Add an account")),
            "{rows:#?}"
        );
        assert!(
            rows.iter().all(|row| row.chars().count() <= 80),
            "{rows:#?}"
        );
        shell.refresh();
        assert_eq!(
            status_reads(&mut shell),
            0,
            "a known check is not read again"
        );

        for _ in 0..3 {
            shell.key(Key::Down);
        }
        let rows = render(&mut shell, 80, 24);
        assert!(
            rows.last().unwrap().contains("Enter Install tools"),
            "{rows:#?}"
        );
        shell.key(Key::Enter);
        assert_eq!(shell.place(), Some(Place::Setup { first_run: false }));
        shell.key(Key::Esc);
        assert_eq!(shell.place(), Some(Place::Home));

        shell.arrive(status(tools(&[])));
        assert_eq!(
            shell
                .ctx
                .message()
                .map(|message| (message.lead.clone(), message.rest.clone())),
            Some(("Setup done.".into(), "Team key is ready for agents.".into()))
        );
        let rows = render(&mut shell, 80, 24);
        assert!(
            rows.iter().all(|row| !row.contains("Get set up")),
            "the panel hides once accounts, a pool and tools are done: {rows:#?}"
        );
    }

    /// A learning read from a fresh engine with the given steps done and the guide hidden
    /// or not. Inputs: the done steps and the skip. Output: the read.
    fn learning(done: &[StepId], skipped: bool) -> LearningProgress {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        let tx = db.transaction().unwrap();
        let mut progress = crate::learning::progress(&tx).unwrap();
        assert_eq!(progress.steps.len(), 10);
        for step in &mut progress.steps {
            if done.contains(&step.id) {
                step.count = 1;
            }
        }
        progress.skipped = skipped;
        progress
    }

    fn reply(query: LearningQuery, result: Result<LearningReply, String>) -> Arrival {
        Arrival::Owner(Box::new(OwnerEvent::Learning(Box::new(LearningEvent {
            query,
            result,
        }))))
    }

    fn read(progress: LearningProgress) -> Arrival {
        reply(
            LearningQuery::Progress,
            Ok(LearningReply::Progress(progress)),
        )
    }

    fn mark(step: MarkStep, result: Result<LearningReply, String>) -> Arrival {
        reply(
            LearningQuery::Mark {
                step,
                on: true,
                generation: 1,
            },
            result,
        )
    }

    /// The learning requests waiting in the queue, the rest dropped.
    /// Input: the shell. Output: the queries in order.
    fn learning_requests(shell: &mut Shell) -> Vec<LearningQuery> {
        std::iter::from_fn(|| shell.ctx.next_request())
            .filter_map(|request| match request {
                Request::Learning(query) => Some(*query),
                _ => None,
            })
            .collect()
    }

    fn marks(shell: &mut Shell) -> Vec<MarkStep> {
        learning_requests(shell)
            .into_iter()
            .filter_map(|query| match query {
                LearningQuery::Mark { step, on: true, .. } => Some(step),
                _ => None,
            })
            .collect()
    }

    fn card() -> ProjectCard {
        ProjectCard {
            id: ProjectId::new("notes-cli").unwrap(),
            name: "Notes CLI".into(),
            about: String::new(),
            platforms: Vec::new(),
            pool: None,
            pool_id: None,
            research: Default::default(),
            created_ms: None,
            activity: Default::default(),
        }
    }

    /// A Home whose setup is done or not, with the projects and learning read given.
    /// Inputs: whether an account, a pool and the tools are ready, the projects and the
    /// learning state. Output: the shell on Home, its first requests taken.
    fn guide_home(
        setup_done: bool,
        projects: Vec<ProjectCard>,
        learning: Load<LearningProgress>,
    ) -> Shell {
        let mut ctx = ctx(80, 24);
        ctx.store.projects = Load::Ready(projects);
        ctx.store.setup = Load::Ready(tools(&[]));
        if setup_done {
            ctx.store.pools = Load::Ready(vec![pool("Main", &[MemberStatus::SetUp])]);
            ctx.store.accounts = Load::Ready(vec![account(1, "Team key", AccountStatus::KeySaved)]);
        } else {
            ctx.store.pools = Load::Ready(Vec::new());
            ctx.store.accounts = Load::Ready(Vec::new());
        }
        ctx.store.learning = learning;
        let mut shell = Shell::with(ctx, Box::new(Home::new()));
        while shell.ctx.next_request().is_some() {}
        shell
    }

    fn shows(rows: &[String], text: &str) -> bool {
        rows.iter().any(|row| row.contains(text))
    }

    #[test]
    fn learn_crazyagent_lists_setup_under_basics_and_every_topic_with_its_progress_at_80x24() {
        let mut shell = guide_home(
            true,
            Vec::new(),
            Load::Ready(learning(&[StepId::Project], false)),
        );
        let rows = render(&mut shell, 80, 24);
        for wanted in [
            "Learn CrazyAgent",
            "4 of 13 done",
            "Basics",
            "[x] Add an account",
            "[x] Create a pool",
            "[ ] Check your harnesses",
            "[x] Install tools",
            "[x] Create a project",
            "Core loop",
            "[ ] Talk to the master",
            "IDE and terminals",
            "[ ] Open the IDE",
            "Process, Metrics and Usage",
            "[ ] Check your usage",
        ] {
            assert!(shows(&rows, wanted), "{wanted:?} in {rows:#?}");
        }
        assert!(!shows(&rows, "Get set up"), "{rows:#?}");
        assert!(!shows(&rows, "Enter opens Settings"), "{rows:#?}");
        assert!(
            !shows(&rows, "Each project has a master"),
            "a row's words show only while it is picked: {rows:#?}"
        );
        assert!(
            rows.iter().all(|row| row.chars().count() <= 80),
            "{rows:#?}"
        );

        shell.key(Key::Tab);
        for _ in 0..20 {
            shell.key(Key::Down);
        }
        let rows = render(&mut shell, 80, 24);
        assert!(
            shows(&rows, "Learn CrazyAgent"),
            "the title stays on top: {rows:#?}"
        );
        assert!(shows(&rows, "4 of 13 done"), "{rows:#?}");
        assert!(shows(&rows, "Process, Metrics and Usage"), "{rows:#?}");
        assert!(shows(&rows, "[ ] Check your usage"), "{rows:#?}");
        assert!(shows(&rows, "Usage shows how much of"), "{rows:#?}");
        assert!(
            rows.iter().all(|row| row.chars().count() <= 80),
            "{rows:#?}"
        );
        let footer = rows.last().unwrap();
        assert!(footer.contains("Enter Open usage"), "{footer}");
        assert!(footer.contains("h Hide guide"), "{footer}");
        assert_eq!(
            TOPICS.map(|(_, heading)| heading),
            [
                "Basics",
                "Core loop",
                "IDE and terminals",
                "Process, Metrics and Usage"
            ],
            "the same words as the desktop's TOPIC_HEADINGS"
        );
    }

    #[test]
    fn the_guide_waits_for_the_learning_read_and_a_hidden_guide_or_an_older_engine_keeps_get_set_up()
     {
        let mut ctx = ctx(80, 24);
        ctx.store.projects = Load::Ready(Vec::new());
        ctx.store.pools = Load::Ready(Vec::new());
        ctx.store.accounts = Load::Ready(Vec::new());
        ctx.store.setup = Load::Ready(tools(&[]));
        ctx.store.learning = Load::Idle;
        let mut shell = Shell::with(ctx, Box::new(Home::new()));
        assert_eq!(learning_requests(&mut shell), [LearningQuery::Progress]);
        assert_eq!(shell.ctx.store.learning, Load::Loading);
        let rows = render(&mut shell, 80, 24);
        assert!(
            !shows(&rows, "Get set up") && !shows(&rows, "Learn CrazyAgent"),
            "no panel until the learning read settles: {rows:#?}"
        );

        shell.arrive(read(learning(&[], true)));
        let rows = render(&mut shell, 80, 24);
        assert!(shows(&rows, "Get set up"), "{rows:#?}");
        assert!(!shows(&rows, "Learn CrazyAgent"), "{rows:#?}");
        assert!(shows(&rows, "Enter opens Settings"), "{rows:#?}");
        shell.refresh();
        assert!(
            learning_requests(&mut shell).is_empty(),
            "a hidden guide is not read again on every refresh"
        );

        for learning in [Load::NotReady, Load::Failed("owner_busy".into())] {
            let mut shell = guide_home(false, Vec::new(), learning.clone());
            let rows = render(&mut shell, 80, 24);
            assert!(shows(&rows, "Get set up"), "{learning:?}: {rows:#?}");
            assert!(!shows(&rows, "Learn CrazyAgent"), "{learning:?}: {rows:#?}");
        }

        let mut shell = guide_home(false, Vec::new(), Load::Ready(learning(&[], false)));
        shell.arrive(reply(LearningQuery::Progress, Err("owner_busy".into())));
        let rows = render(&mut shell, 80, 24);
        assert!(
            shows(&rows, "Learn CrazyAgent"),
            "a failed read keeps the last good one: {rows:#?}"
        );

        let all = [
            StepId::Project,
            StepId::Master,
            StepId::Agents,
            StepId::Question,
            StepId::ChangeSet,
            StepId::Ide,
            StepId::Terminal,
            StepId::Process,
            StepId::Metrics,
            StepId::Usage,
        ];
        for (learning, why) in [
            (learning(&[], true), "a hidden guide"),
            (learning(&all, false), "every step done"),
        ] {
            let mut shell = guide_home(true, Vec::new(), Load::Ready(learning));
            let rows = render(&mut shell, 80, 24);
            assert!(
                !shows(&rows, "Get set up") && !shows(&rows, "Learn CrazyAgent"),
                "{why} with setup done shows no panel: {rows:#?}"
            );
        }
    }

    #[test]
    fn enter_on_a_step_opens_its_place_and_h_hides_the_guide_with_the_last_row_picked() {
        let mut shell = guide_home(true, vec![card()], Load::Ready(learning(&[], false)));
        shell.key(Key::Tab);
        for _ in 0..5 {
            shell.key(Key::Down);
        }
        let footer = render(&mut shell, 80, 24).pop().unwrap();
        assert!(footer.contains("Enter Open conversation"), "{footer}");
        shell.key(Key::Enter);
        assert_eq!(
            shell.place(),
            Some(Place::Project {
                id: ProjectId::new("notes-cli").unwrap(),
                name: "Notes CLI".into(),
                tab: ProjectTab::Conversation,
            })
        );

        let mut shell = guide_home(true, Vec::new(), Load::Ready(learning(&[], false)));
        shell.key(Key::Tab);
        for _ in 0..9 {
            shell.key(Key::Down);
        }
        let footer = render(&mut shell, 80, 24).pop().unwrap();
        assert!(
            !footer.contains("Enter"),
            "the IDE has no TUI page: {footer}"
        );
        shell.key(Key::Enter);
        assert_eq!(shell.place(), Some(Place::Home));
        assert_eq!(
            shell.ctx.message().map(|message| message.lead.clone()),
            Some("The IDE opens in the desktop app.".into())
        );
        shell.key(Key::Down);
        shell.key(Key::Enter);
        assert_eq!(
            shell.place(),
            Some(Place::NewProject),
            "with no project yet, a project step starts one"
        );

        let mut shell = guide_home(false, Vec::new(), Load::Ready(learning(&[], false)));
        shell.key(Key::Tab);
        for _ in 0..20 {
            shell.key(Key::Down);
        }
        let footer = render(&mut shell, 80, 24).pop().unwrap();
        assert!(footer.contains("Enter Open usage"), "{footer}");
        shell.key(Key::Char('h'));
        let sent = learning_requests(&mut shell);
        let [
            LearningQuery::Mark {
                step: MarkStep::Skipped,
                on: true,
                generation,
            },
        ] = sent.as_slice()
        else {
            panic!("h sets the skipped mark: {sent:?}");
        };
        let hide = |generation: u64| LearningQuery::Mark {
            step: MarkStep::Skipped,
            on: true,
            generation,
        };
        shell.arrive(reply(hide(generation + 1), Err("owner_busy".into())));
        assert!(
            shell.ctx.message().is_none(),
            "a skip another page sent is that page's to report"
        );
        shell.arrive(reply(hide(*generation), Err("owner_busy".into())));
        assert_eq!(
            shell.ctx.message().map(|message| message.lead.clone()),
            Some("Your learning progress was not saved.".into())
        );
        shell.arrive(mark(MarkStep::Skipped, Ok(LearningReply::Marked)));
        assert_eq!(
            learning_requests(&mut shell),
            [LearningQuery::Progress],
            "a set mark reads the steps again"
        );
        shell.arrive(read(learning(&[], true)));
        let rows = render(&mut shell, 80, 24);
        assert!(shows(&rows, "Get set up"), "{rows:#?}");
        assert!(
            rows.last().unwrap().contains("Enter Install tools"),
            "the picked row moves back inside the setup items: {rows:#?}"
        );
        shell.key(Key::Up);
        shell.key(Key::Char('h'));
        assert!(
            learning_requests(&mut shell).is_empty(),
            "Get set up has no guide to hide"
        );
    }

    #[test]
    fn usage_process_and_an_attached_terminal_each_set_their_mark_once_per_session() {
        let mut shell = guide_home(true, Vec::new(), Load::Idle);
        shell.key(Key::Char('u'));
        assert_eq!(shell.place(), Some(Place::Usage));
        assert_eq!(marks(&mut shell), [MarkStep::Metrics, MarkStep::Usage]);
        shell.key(Key::Esc);
        shell.key(Key::Char('u'));
        assert!(marks(&mut shell).is_empty(), "once per session");
        shell.arrive(mark(MarkStep::Usage, Err("owner_busy".into())));
        shell.key(Key::Esc);
        shell.key(Key::Char('u'));
        assert_eq!(
            marks(&mut shell),
            [MarkStep::Usage],
            "a refused mark goes again"
        );
        shell.arrive(mark(MarkStep::Usage, Ok(LearningReply::Marked)));
        assert_eq!(learning_requests(&mut shell), [LearningQuery::Progress]);

        let opened = |place: &Place, learning: Load<LearningProgress>| {
            let mut context = ctx(80, 24);
            context.store.learning = learning;
            let screen = crate::tui::screens::open(place, &mut context);
            let mut shell = Shell::with(context, screen);
            marks(&mut shell)
        };
        for place in [
            Place::Settings(SettingsSection::Process),
            Place::ProjectProcess {
                id: ProjectId::new("notes-cli").unwrap(),
                name: "Notes CLI".into(),
            },
        ] {
            assert_eq!(opened(&place, Load::Idle), [MarkStep::Process], "{place:?}");
        }
        assert!(
            opened(
                &Place::Settings(SettingsSection::Process),
                Load::Ready(learning(&[StepId::Process], false)),
            )
            .is_empty(),
            "a step the read shows done is not sent"
        );
        assert!(
            opened(&Place::Usage, Load::NotReady).is_empty(),
            "an engine without the learning read gets none"
        );

        let mut shell = guide_home(true, Vec::new(), Load::Idle);
        let agent = AgentId::team(1).unwrap();
        shell.arrive(Arrival::Terminal {
            agent: agent.clone(),
            frame: TerminalFrame::Rejected {
                code: "owner_busy".into(),
            },
        });
        assert!(
            marks(&mut shell).is_empty(),
            "a refused terminal opened nothing"
        );
        for _ in 0..2 {
            shell.arrive(Arrival::Terminal {
                agent: agent.clone(),
                frame: TerminalFrame::Attached {
                    agent_id: agent.clone(),
                    cols: 74,
                    rows: 17,
                },
            });
        }
        assert_eq!(marks(&mut shell), [MarkStep::Terminal]);
    }
}
