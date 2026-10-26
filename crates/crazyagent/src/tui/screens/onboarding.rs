use super::intake::{NOT_READY, ask, copy, event, not_ready};
use super::new_project::{NAME_BYTES_MAX, PLATFORM_ORDER};
use super::project_time::{clock, local_offset};
use super::{Flow, Place, ProjectTab, Screen, SettingsSection, open};
use crate::engine::owner::OwnerResult;
use crate::onboarding::{OnboardingView, Step};
use crate::pools::PoolSummary;
use crate::project_setup::{Platform, SetupAction, SetupQuery, SetupView};
use crate::protocol::{CredentialRef, ProjectId};
use crate::tui::client::OwnerEvent;
use crate::tui::client::intake::{IntakeQuery, IntakeView};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::{Arrival, PoolChange, Request};
use crate::tui::text::{join, list_row, row, status, styled, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::{TextField, buttons, checkbox};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};

const PAGE_WIDTH: usize = 76;
const ABOUT_CHARS_MAX: usize = 2_000;
const STACK_CHARS_MAX: usize = 1_000;
const STACK_ITEMS_MAX: usize = 32;
const PLATFORMS_PER_ROW: usize = 3;
const GRAPH_OFF: &str = "Graphify is off in Setup, so no code graph is built.";
const GRAPH_MISSING: &str =
    "Graphify isn't installed, so no code graph is built. Install it in Setup.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Name,
    About,
    Stack,
    Platforms,
    Confirm,
}

impl Focus {
    const ORDER: [Focus; 5] = [
        Self::Name,
        Self::About,
        Self::Stack,
        Self::Platforms,
        Self::Confirm,
    ];

    fn step(self, forward: bool) -> Self {
        let index = Self::ORDER
            .iter()
            .position(|focus| *focus == self)
            .expect("every focus is listed");
        let count = Self::ORDER.len();
        Self::ORDER[if forward {
            (index + 1) % count
        } else {
            (index + count - 1) % count
        }]
    }
}

/// The review form filled from the onboarding draft. The owner fixes the
/// fields the engine marked missing and confirms it as the project's setup.
struct Review {
    onboarding: Box<OnboardingView>,
    name: TextField,
    about: TextField,
    stack: TextField,
    platforms: Vec<Platform>,
    platform: usize,
    focus: Focus,
    revision: Load<u64>,
}

impl Review {
    fn new(onboarding: Box<OnboardingView>) -> Self {
        let mut name = TextField::new("Name", NAME_BYTES_MAX);
        name.set(&onboarding.name);
        let mut about = TextField::new("About", ABOUT_CHARS_MAX);
        about.set(&onboarding.draft.about);
        let mut stack = TextField::new("Tech stack", STACK_CHARS_MAX).note("Separate with commas.");
        stack.set(&onboarding.draft.tech_stack.join(", "));
        let platforms = onboarding.draft.platforms.clone();
        Self {
            onboarding,
            name,
            about,
            stack,
            platforms,
            platform: 0,
            focus: Focus::Name,
            revision: Load::Idle,
        }
    }

    fn stack(&self) -> Vec<String> {
        self.stack
            .value()
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .take(STACK_ITEMS_MAX)
            .map(str::to_owned)
            .collect()
    }

    fn still_missing(&self, field: &str) -> bool {
        let empty = match field {
            "name" => self.name.value().trim().is_empty(),
            "about" => self.about.value().trim().is_empty(),
            "tech_stack" => self.stack().is_empty(),
            "platforms" => self.platforms.is_empty(),
            _ => true,
        };
        empty
            && self
                .onboarding
                .missing
                .iter()
                .any(|missing| missing == field)
    }

    fn missing_labels(&self) -> Vec<String> {
        self.onboarding
            .missing
            .iter()
            .filter(|field| self.still_missing(field))
            .map(|field| {
                let mut label = field.replace('_', " ");
                if let Some(first) = label.get_mut(0..1) {
                    first.make_ascii_uppercase();
                }
                label
            })
            .collect()
    }

    fn action(&self, revision: u64) -> SetupAction {
        let name = self.name.value().trim().to_owned();
        let mut setup = self.onboarding.draft.clone();
        setup.about = self.about.value().trim().to_owned();
        setup.tech_stack = self.stack();
        setup.platforms = self.platforms.clone();
        setup.documents = Vec::new();
        SetupAction::Update {
            project_id: self.onboarding.project_id.clone(),
            expected_revision: revision,
            name: (name != self.onboarding.name).then_some(name),
            setup,
        }
    }
}

enum Phase {
    Pool { selected: usize },
    Starting,
    Running(Load<Box<OnboardingView>>),
    Review(Box<Review>),
    Confirming(Box<Review>),
    Assigning(String),
}

/// The onboarding page for a found, cloned or added folder: picks the pool,
/// starts onboarding, shows the graph and mapping steps while they run, then
/// shows the drafted setup with the missing items marked for the owner to fix
/// and confirm as the project's setup.
pub struct OnboardingPage {
    path: String,
    name: String,
    project: Option<ProjectId>,
    phase: Phase,
    problem: Option<String>,
}

impl OnboardingPage {
    /// An onboarding page for one folder.
    /// Takes the folder path in the projects folder, its name, and the project when onboarding already started.
    /// Returns the screen at the pool step, or at the running step for a started onboarding.
    pub fn new(path: String, name: String, project: Option<ProjectId>) -> Self {
        let phase = if project.is_some() {
            Phase::Running(Load::Idle)
        } else {
            Phase::Pool { selected: 0 }
        };
        Self {
            path,
            name,
            project,
            phase,
            problem: None,
        }
    }

    fn pools(ctx: &Ctx) -> &[PoolSummary] {
        ctx.store.pools.ready().map_or(&[], Vec::as_slice)
    }

    fn start(&mut self, pool: CredentialRef, ctx: &mut Ctx) {
        self.problem = None;
        self.phase = Phase::Starting;
        let generation = ctx.next_generation();
        ask(
            ctx,
            IntakeQuery::StartOnboarding {
                path: self.path.clone(),
                pool,
                generation,
            },
        );
    }

    fn poll(&mut self, ctx: &mut Ctx) {
        if let (Some(project), Phase::Running(load)) = (&self.project, &mut self.phase)
            && !matches!(load, Load::Loading)
        {
            if matches!(load, Load::Idle) {
                *load = Load::Loading;
            }
            ask(
                ctx,
                IntakeQuery::Onboarding {
                    project: project.clone(),
                },
            );
        }
    }

    fn settled(onboarding: &OnboardingView) -> bool {
        let done = |step: &Step| {
            matches!(
                step,
                Step::Done | Step::Off | Step::NotInstalled | Step::Failed { .. }
            )
        };
        onboarding.finished || (done(&onboarding.graph) && done(&onboarding.mapping))
    }

    /// Whether the review offers to map again: the mapping agent failed and
    /// the owner has not saved the setup yet.
    fn retryable(onboarding: &OnboardingView) -> bool {
        matches!(onboarding.mapping, Step::Failed { .. }) && !onboarding.finished
    }

    /// Starts the onboarding again on the draft's pool so the failed mapping
    /// runs once more; the review's edits are dropped for the new draft.
    fn retry(&mut self, ctx: &mut Ctx) {
        let Phase::Review(review) = &self.phase else {
            return;
        };
        match review.onboarding.draft.pool.clone() {
            Some(pool) => self.start(pool, ctx),
            None => self.problem = Some("Pick a pool for this project first.".into()),
        }
    }

    fn read_revision(project: &ProjectId, review: &mut Review, ctx: &mut Ctx) {
        review.revision = Load::Loading;
        ctx.request(Request::Setup(SetupQuery::Setup {
            project: project.clone(),
        }));
    }

    fn confirm(&mut self, ctx: &mut Ctx) {
        let Phase::Review(review) = &mut self.phase else {
            return;
        };
        if review.name.value().trim().is_empty() {
            self.problem = Some("Give the project a name.".into());
            review.focus = Focus::Name;
            return;
        }
        let revision = match review.revision {
            Load::Ready(revision) => revision,
            Load::Loading => return,
            _ => {
                let project = review.onboarding.project_id.clone();
                Self::read_revision(&project, review, ctx);
                return;
            }
        };
        self.problem = None;
        let action = review.action(revision);
        let generation = ctx.next_generation();
        ctx.request(Request::Setup(SetupQuery::Change { action, generation }));
        let Phase::Review(review) = std::mem::replace(&mut self.phase, Phase::Starting) else {
            unreachable!("the phase was review above");
        };
        self.phase = Phase::Confirming(review);
    }

    fn intake(&mut self, query: &IntakeQuery, result: &Result<IntakeView, String>, ctx: &mut Ctx) {
        match (query, result, &mut self.phase) {
            (IntakeQuery::StartOnboarding { path, .. }, result, Phase::Starting)
                if *path == self.path =>
            {
                match result {
                    Ok(IntakeView::OnboardingStarted(project)) => {
                        self.project = Some(project.clone());
                        self.phase = Phase::Running(Load::Idle);
                        self.poll(ctx);
                    }
                    Ok(_) => {
                        self.problem = Some(copy("intake_change_outcome_unknown"));
                        self.phase = Phase::Pool { selected: 0 };
                    }
                    Err(code) => {
                        self.problem = Some(copy(code));
                        self.phase = Phase::Pool { selected: 0 };
                    }
                }
            }
            (IntakeQuery::Onboarding { project }, result, Phase::Running(load))
                if self.project.as_ref() == Some(project) =>
            {
                match result {
                    Ok(IntakeView::Onboarding(onboarding)) if Self::settled(onboarding) => {
                        let mut review = Box::new(Review::new(onboarding.clone()));
                        Self::read_revision(project, &mut review, ctx);
                        self.phase = Phase::Review(review);
                    }
                    Ok(IntakeView::Onboarding(onboarding)) => {
                        *load = Load::Ready(onboarding.clone());
                    }
                    Ok(_) => *load = Load::Failed("intake_change_outcome_unknown".into()),
                    Err(code) if not_ready(code) => *load = Load::NotReady,
                    Err(code) => *load = Load::Failed(code.clone()),
                }
            }
            _ => {}
        }
    }

    fn setup(
        &mut self,
        query: &SetupQuery,
        result: &Result<SetupView, String>,
        ctx: &mut Ctx,
    ) -> Flow {
        let Some(project) = self.project.clone() else {
            return Flow::Handled;
        };
        match (query, result, &mut self.phase) {
            (SetupQuery::Setup { project: read }, result, Phase::Review(review))
                if *read == project =>
            {
                review.revision = match result {
                    Ok(SetupView::Setup(Some(stored))) => Load::Ready(stored.revision),
                    Ok(_) => Load::Failed("project_missing".into()),
                    Err(code) => Load::Failed(code.clone()),
                };
                Flow::Handled
            }
            (
                SetupQuery::Change {
                    action: SetupAction::Update { project_id, .. },
                    ..
                },
                result,
                Phase::Confirming(_),
            ) if *project_id == project => {
                let Phase::Confirming(review) = std::mem::replace(&mut self.phase, Phase::Starting)
                else {
                    unreachable!("the phase was confirming above");
                };
                match result {
                    Ok(SetupView::Changed(OwnerResult::Applied)) => {
                        let name = review.name.value().trim().to_owned();
                        match review.onboarding.draft.pool.clone() {
                            Some(pool) => {
                                ctx.request(Request::EditPool {
                                    change: PoolChange::Assign {
                                        pool: Some(pool),
                                        project,
                                    },
                                });
                                self.phase = Phase::Assigning(name);
                                Flow::Handled
                            }
                            None => {
                                ctx.say(Message::success("Set up.", &format!("{name} is ready.")));
                                Self::opened(project, name, ctx)
                            }
                        }
                    }
                    other => {
                        let code = match other {
                            Err(code) => code.as_str(),
                            Ok(SetupView::Changed(OwnerResult::Rejected { code })) => code.as_str(),
                            Ok(_) => "intake_change_outcome_unknown",
                        };
                        self.problem = Some(copy(code));
                        let mut review = review;
                        if code == "stale_revision" {
                            Self::read_revision(&project, &mut review, ctx);
                        }
                        self.phase = Phase::Review(review);
                        Flow::Handled
                    }
                }
            }
            _ => Flow::Handled,
        }
    }

    /// Finishes an assignment of the setup's pool to the project's master the way
    /// New project does: a refused assignment is said plainly and the project opens
    /// either way. Takes the pool edit reply and the context. Returns the project page.
    fn assigned(&mut self, result: &Result<(), String>, ctx: &mut Ctx) -> Flow {
        let Phase::Assigning(name) = std::mem::replace(&mut self.phase, Phase::Starting) else {
            unreachable!("only an assigning onboarding takes a pool edit reply");
        };
        let project = self
            .project
            .clone()
            .expect("an assigning onboarding names its project");
        match result {
            Ok(()) => {
                ctx.request(Request::PoolProjects);
                ctx.say(Message::success("Set up.", &format!("{name} is ready.")));
            }
            Err(_) => ctx.say(Message::error(
                &format!("{name} is set up, but its pool is not assigned."),
                "Open the project's Settings tab to assign it.",
            )),
        }
        Self::opened(project, name, ctx)
    }

    fn opened(project: ProjectId, name: String, ctx: &mut Ctx) -> Flow {
        Flow::Replace(open(
            &Place::Project {
                id: project,
                name,
                tab: ProjectTab::Overview,
            },
            ctx,
        ))
    }

    fn step_line(label: &str, step: &Step, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let state = match step {
            Step::Waiting { until_ms: None } => status("Waiting", theme, ctx.glyphs),
            Step::Waiting {
                until_ms: Some(until),
            } => status(
                &format!("Rate limited until {}", clock(*until, local_offset(*until))),
                theme,
                ctx.glyphs,
            ),
            Step::Running => Line::from(vec![
                Span::styled(ctx.spinner(), theme.running),
                Span::styled(" Running", theme.running),
            ]),
            Step::Done => status("Done", theme, ctx.glyphs),
            Step::Off => status("Turned off", theme, ctx.glyphs),
            Step::NotInstalled => status("Not installed", theme, ctx.glyphs),
            Step::Failed { .. } => status("Failed", theme, ctx.glyphs),
        };
        let mut lines = vec![row(limit, Line::raw(label.to_owned()), state, ctx.glyphs)];
        let why = match step {
            Step::Off => Some(GRAPH_OFF),
            Step::NotInstalled => Some(GRAPH_MISSING),
            _ => None,
        };
        if let Some(why) = why {
            lines.extend(
                wrap(why, limit.saturating_sub(2))
                    .into_iter()
                    .map(|text| styled(format!("  {text}"), Role::Muted, theme)),
            );
        }
        if let Step::Failed { reason } = step {
            lines.extend(
                wrap(reason, limit.saturating_sub(2))
                    .into_iter()
                    .map(|text| styled(format!("  {text}"), Role::Muted, theme)),
            );
        }
        lines
    }

    fn pool_lines(&self, selected: usize, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let mut lines: Vec<Line<'static>> = wrap(
            "Pick the pool whose accounts map this project. CrazyAgent builds a code graph, then an agent reads the project and drafts its setup for you to check.",
            limit,
        )
        .into_iter()
        .map(Line::raw)
        .collect();
        lines.push(Line::default());
        match &ctx.store.pools {
            Load::Ready(pools) if pools.is_empty() => {
                lines.push(Line::raw("No pools yet."));
                lines.push(styled(
                    "Press p to create one in Settings, Pools.",
                    Role::Muted,
                    theme,
                ));
            }
            Load::Ready(pools) => {
                for (index, pool) in pools.iter().enumerate() {
                    lines.push(list_row(
                        row(
                            limit.saturating_sub(2),
                            Line::raw(pool.name.clone()),
                            styled(pool.summary.clone(), Role::Muted, theme),
                            ctx.glyphs,
                        ),
                        limit,
                        index == selected,
                        true,
                        theme,
                        ctx.glyphs,
                    ));
                }
            }
            Load::Failed(_) => lines.push(status("Failed to load pools.", theme, ctx.glyphs)),
            Load::NotReady => lines.push(styled(NOT_READY, Role::Muted, theme)),
            Load::Idle | Load::Loading => lines.push(ctx.loading("Loading pools")),
        }
        lines
    }

    fn review_lines(
        review: &Review,
        limit: usize,
        confirming: bool,
        ctx: &Ctx,
    ) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        let mut lines =
            Self::step_line("Build the code graph", &review.onboarding.graph, limit, ctx);
        lines.push(Line::default());
        if let Step::Failed { reason } = &review.onboarding.mapping
            && !review.onboarding.finished
        {
            lines.extend(
                wrap(&format!("Mapping failed: {reason}"), limit)
                    .into_iter()
                    .map(|text| styled(text, Role::Error, theme)),
            );
            lines.extend(
                wrap(
                    "Fill in the setup yourself to skip it, or press r on Confirm to map again.",
                    limit,
                )
                .into_iter()
                .map(|text| styled(text, Role::Muted, theme)),
            );
            lines.push(Line::default());
        }
        let missing = review.missing_labels();
        if review.onboarding.finished {
            lines.push(status("Saved", theme, ctx.glyphs));
            lines.push(Line::raw(
                "This project is set up. Change anything, then save.",
            ));
        } else if missing.is_empty() {
            lines.push(status("Ready", theme, ctx.glyphs));
            lines.push(Line::raw("Check the setup and confirm it."));
        } else {
            lines.push(status("Needs you", theme, ctx.glyphs));
            lines.extend(
                wrap(&format!("Missing: {}.", missing.join(", ")), limit)
                    .into_iter()
                    .map(Line::raw),
            );
        }
        lines.push(Line::default());
        let mut cursor = None;
        let editing = !confirming;
        for (focus, field, key, label) in [
            (Focus::Name, &review.name, "name", "Name"),
            (Focus::About, &review.about, "about", "About"),
            (Focus::Stack, &review.stack, "tech_stack", "Tech stack"),
        ] {
            let top = lines.len() as u16;
            let focused = editing && review.focus == focus;
            let (mut field_lines, at) = field.lines(limit, focused, theme, ctx.glyphs);
            if review.still_missing(key)
                && let Some(first) = field_lines.first_mut()
            {
                let role = if focused { Role::Accent } else { Role::Heading };
                *first = row(
                    limit,
                    styled(label, role, theme),
                    status("Missing", theme, ctx.glyphs),
                    ctx.glyphs,
                );
            }
            if focused {
                cursor = at.map(|at| Position::new(at.x, at.y + top));
            }
            lines.extend(field_lines);
            lines.push(Line::default());
        }
        let platforms_focused = editing && review.focus == Focus::Platforms;
        let platforms_label = styled(
            "Platforms",
            if platforms_focused {
                Role::Accent
            } else {
                Role::Heading
            },
            theme,
        );
        lines.push(if review.still_missing("platforms") {
            row(
                limit,
                platforms_label,
                status("Missing", theme, ctx.glyphs),
                ctx.glyphs,
            )
        } else {
            platforms_label
        });
        for (chunk, platforms) in PLATFORM_ORDER.chunks(PLATFORMS_PER_ROW).enumerate() {
            let mut spans = Vec::new();
            for (offset, platform) in platforms.iter().enumerate() {
                let index = chunk * PLATFORMS_PER_ROW + offset;
                if offset > 0 {
                    spans.push(Span::raw("  "));
                }
                let text = checkbox(review.platforms.contains(platform), platform.label());
                spans.push(if platforms_focused && index == review.platform {
                    Span::styled(text, theme.selection)
                } else {
                    Span::raw(text)
                });
            }
            lines.push(Line::from(spans));
        }
        lines.push(Line::default());
        let facts = &review.onboarding.facts;
        for (label, items) in [
            ("Languages", &facts.languages),
            ("Build", &facts.build_commands),
            ("Tests", &facts.test_commands),
            ("Entry points", &facts.entry_points),
        ] {
            let text = if items.is_empty() {
                "None found".to_owned()
            } else {
                items.join(", ")
            };
            lines.extend(
                wrap(&format!("{label}: {text}"), limit)
                    .into_iter()
                    .map(|line| styled(line, Role::Muted, theme)),
            );
        }
        lines.push(Line::default());
        if confirming {
            lines.push(ctx.loading("Saving the setup"));
        } else {
            let label = match review.revision {
                Load::Loading => "Loading",
                _ if review.onboarding.finished => "Save changes",
                _ => "Confirm setup",
            };
            lines.push(buttons(
                &[label],
                (review.focus == Focus::Confirm).then_some(0),
                theme,
            ));
        }
        (lines, cursor)
    }
}

impl Screen for OnboardingPage {
    fn crumbs(&self) -> Vec<String> {
        vec!["Set up".into(), self.name.clone()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let arrows = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        match &self.phase {
            Phase::Pool { .. } if Self::pools(ctx).is_empty() => {
                vec![hint("p", "Create a pool"), hint("Esc", "Back")]
            }
            Phase::Pool { .. } => vec![
                hint("Enter", "Start setup"),
                hint(&arrows, "Move"),
                hint("Esc", "Back"),
            ],
            Phase::Review(review) => match review.focus {
                Focus::Platforms => vec![
                    hint("Space", "Turn on or off"),
                    hint(&format!("{}{}", ctx.glyphs.left, ctx.glyphs.right), "Move"),
                    hint("Tab", "Next"),
                    hint("Esc", "Back"),
                ],
                Focus::Confirm => {
                    let mut hints = vec![hint(
                        "Enter",
                        if review.onboarding.finished {
                            "Save changes"
                        } else {
                            "Confirm setup"
                        },
                    )];
                    if Self::retryable(&review.onboarding) {
                        hints.push(hint("r", "Retry mapping"));
                    }
                    hints.extend([hint("Shift-Tab", "Previous"), hint("Esc", "Back")]);
                    hints
                }
                _ => vec![
                    hint("Tab", "Next"),
                    hint("Shift-Tab", "Previous"),
                    hint("Esc", "Back"),
                ],
            },
            _ => vec![hint("Esc", "Back")],
        }
    }

    fn typing(&self) -> bool {
        matches!(&self.phase, Phase::Review(review)
            if matches!(review.focus, Focus::Name | Focus::About | Focus::Stack))
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if matches!(key, Key::Esc) {
            return Flow::Ignored;
        }
        let pools = Self::pools(ctx).len();
        match &mut self.phase {
            Phase::Pool { .. } if pools == 0 => match key {
                Key::Char('p') => Flow::Open(Place::Settings(SettingsSection::Pools)),
                _ => Flow::Ignored,
            },
            Phase::Pool { selected } => {
                match key {
                    Key::Up => *selected = selected.saturating_sub(1),
                    Key::Down => *selected = (*selected + 1).min(pools - 1),
                    Key::Enter => {
                        let index = (*selected).min(pools - 1);
                        let pool = Self::pools(ctx)[index].id.clone();
                        self.start(pool, ctx);
                    }
                    _ => return Flow::Ignored,
                }
                Flow::Handled
            }
            Phase::Review(review) => {
                match (review.focus, key) {
                    (Focus::Confirm, Key::Enter) => {
                        self.confirm(ctx);
                        return Flow::Handled;
                    }
                    (Focus::Confirm, Key::Char('r')) if Self::retryable(&review.onboarding) => {
                        self.retry(ctx);
                        return Flow::Handled;
                    }
                    (_, Key::Tab | Key::Enter) => review.focus = review.focus.step(true),
                    (_, Key::BackTab) => review.focus = review.focus.step(false),
                    (Focus::Platforms, Key::Left) => {
                        review.platform = review.platform.saturating_sub(1);
                    }
                    (Focus::Platforms, Key::Right) => {
                        review.platform = (review.platform + 1).min(PLATFORM_ORDER.len() - 1);
                    }
                    (Focus::Platforms, Key::Char(' ')) => {
                        let platform = PLATFORM_ORDER[review.platform];
                        match review.platforms.iter().position(|on| *on == platform) {
                            Some(at) => {
                                review.platforms.remove(at);
                            }
                            None => review.platforms.push(platform),
                        }
                    }
                    (Focus::Name, other) => {
                        review.name.key(other);
                    }
                    (Focus::About, other) => {
                        review.about.key(other);
                    }
                    (Focus::Stack, other) => {
                        review.stack.key(other);
                    }
                    _ => return Flow::Ignored,
                }
                self.problem = None;
                Flow::Handled
            }
            _ => Flow::Handled,
        }
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        if let Some(reply) = event(arrival) {
            let (query, result) = (reply.query.clone(), reply.result.clone());
            self.intake(&query, &result, ctx);
            return Flow::Handled;
        }
        if let Arrival::Owner(owner) = arrival
            && let OwnerEvent::Setup(setup) = owner.as_ref()
        {
            let (query, result) = (setup.query.clone(), setup.result.clone());
            return self.setup(&query, &result, ctx);
        }
        if let Arrival::PoolEdited(result) = arrival
            && matches!(self.phase, Phase::Assigning(_))
        {
            return self.assigned(result, ctx);
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        match &self.phase {
            Phase::Pool { .. } => ctx.request(Request::Pools),
            Phase::Running(_) => self.poll(ctx),
            _ => {}
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let theme = &ctx.theme;
        let limit = (area.width as usize).saturating_sub(4).clamp(6, PAGE_WIDTH);
        let mut lines = vec![
            styled(format!("Set up {}", self.name), Role::Heading, theme),
            Line::default(),
        ];
        let mut cursor = None;
        match &self.phase {
            Phase::Pool { selected } => lines.extend(self.pool_lines(*selected, limit, ctx)),
            Phase::Starting => lines.push(ctx.loading("Starting the setup")),
            Phase::Assigning(_) => {
                lines.push(ctx.loading("Giving the pool to the project's master"))
            }
            Phase::Running(load) => match load {
                Load::Ready(onboarding) => {
                    lines.extend(Self::step_line(
                        "Build the code graph",
                        &onboarding.graph,
                        limit,
                        ctx,
                    ));
                    lines.extend(Self::step_line(
                        "Map the project",
                        &onboarding.mapping,
                        limit,
                        ctx,
                    ));
                    lines.push(Line::default());
                    lines.push(styled(
                        "The setup draft shows here when both steps finish.",
                        Role::Muted,
                        theme,
                    ));
                }
                Load::NotReady => lines.push(styled(NOT_READY, Role::Muted, theme)),
                Load::Failed(code) => lines.push(styled(copy(code), Role::Error, theme)),
                Load::Idle | Load::Loading => lines.push(ctx.loading("Reading the setup")),
            },
            Phase::Review(review) | Phase::Confirming(review) => {
                let top = lines.len() as u16;
                let confirming = matches!(self.phase, Phase::Confirming(_));
                let (body, at) = Self::review_lines(review, limit, confirming, ctx);
                cursor = at.map(|at| Position::new(at.x, at.y + top));
                lines.extend(body);
            }
        }
        if let Some(problem) = &self.problem {
            lines.push(Line::default());
            lines.extend(
                wrap(problem, limit)
                    .into_iter()
                    .map(|text| styled(text, Role::Error, theme)),
            );
        }
        let lines: Vec<Line<'static>> = lines
            .into_iter()
            .map(|line| join([Line::raw("  "), line]))
            .collect();
        draw_lines(frame.buffer_mut(), area, &lines);
        cursor.map(|at| Position::new(area.x + 2 + at.x, area.y + at.y))
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        match &self.phase {
            Phase::Starting | Phase::Confirming(_) | Phase::Assigning(_) => true,
            Phase::Running(load) => !matches!(load, Load::Failed(_) | Load::NotReady),
            _ => false,
        }
    }

    fn draft(&self) -> bool {
        matches!(self.phase, Phase::Review(_) | Phase::Confirming(_))
    }

    fn loss(&self) -> &'static str {
        "Your changes to this setup will not be saved."
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Onboarding {
            path: self.path.clone(),
            name: self.name.clone(),
            project: self.project.clone(),
        })
    }
}
