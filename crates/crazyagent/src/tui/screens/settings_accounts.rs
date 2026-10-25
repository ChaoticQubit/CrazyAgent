use super::settings::{draw_page, step};
use super::settings_add_account::{KEY_CHARS_MAX, key_problem};
use super::{Flow, Place, Screen, SettingsSection};
use crate::accounts::{AccountStatus, AccountView, Provider, SignInMethod};
use crate::config::POOL_MEMBERS_MAX;
use crate::engine::owner::AccountSecret;
use crate::protocol::CredentialRef;
use crate::tui::client::OwnerEvent;
use crate::tui::client::accounts::{AccountsReply, AccountsRequest};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::{Ctx, Link, Load};
use crate::tui::frame::{Hint, Message, Modal, hint, sections_hint};
use crate::tui::glyphs::Glyphs;
use crate::tui::keys::Key;
use crate::tui::seam::{AccountRow, Arrival, Request};
use crate::tui::text::{
    cells, cut, join, list_row, pad, restyle, row, status, styled, thumb, with_bar, wrap,
};
use crate::tui::theme::Role;
use crate::tui::widgets::{TextField, buttons};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use std::collections::BTreeSet;

const FILTER_CHARS_MAX: usize = 96;
const NAME_CHARS_MAX: usize = crate::accounts::ACCOUNT_NAME_MAX;
const CHROME_ROWS: usize = 6;
const LABEL_WIDTH: usize = 15;

/// Queues one Settings job on the shell.
/// Inputs: the context and the job. Output: none.
pub fn send(ctx: &mut Ctx, job: SettingsJob) {
    ctx.request(Request::Settings(Box::new(job)));
}

/// The Settings event inside an arrival, if it is one.
/// Input: the arrival. Output: the event.
pub fn settings_event(arrival: &Arrival) -> Option<&SettingsEvent> {
    match arrival {
        Arrival::Owner(event) => match &**event {
            OwnerEvent::Settings(event) => Some(event),
            _ => None,
        },
        _ => None,
    }
}

/// The design's word for a provider.
/// Input: the provider. Output: "ChatGPT / Codex", "Local model" and so on.
pub fn provider_word(provider: Provider) -> &'static str {
    match provider {
        Provider::Chatgpt => "ChatGPT / Codex",
        Provider::Claude => "Claude",
        Provider::Openrouter => "OpenRouter",
        Provider::OpenaiApi => "OpenAI API",
        Provider::AnthropicApi => "Anthropic API",
        Provider::OpenaiCompatible => "OpenAI-compatible URL",
        Provider::Local => "Local model",
    }
}

/// A provider with its glyph, as tables show it.
/// Inputs: provider and glyphs. Output: "✻ Claude".
pub fn provider_cell(provider: Provider, glyphs: &Glyphs) -> String {
    let word = provider_word(provider);
    format!("{} {word}", glyphs.provider(word))
}

/// The short sign-in word of an account.
/// Input: the account. Output: "ChatGPT sign-in", "Claude sign-in", "API key" or "URL".
pub fn sign_in_word(account: &AccountView) -> &'static str {
    match (account.method, account.provider) {
        (SignInMethod::Subscription, Provider::Chatgpt) => "ChatGPT sign-in",
        (SignInMethod::Subscription, _) => "Claude sign-in",
        (SignInMethod::ApiKey, _) => "API key",
        (SignInMethod::Url, _) => "URL",
    }
}

/// Says whether an account needs the owner.
/// Input: the status. Output: true for Not signed in and Can't reach this URL.
pub fn needs_attention(status: AccountStatus) -> bool {
    matches!(
        status,
        AccountStatus::NotSignedIn | AccountStatus::Unreachable
    )
}

fn attention_rank(status: AccountStatus) -> u8 {
    match status {
        AccountStatus::NotSignedIn => 0,
        AccountStatus::Unreachable => 1,
        _ => 2,
    }
}

/// Names of the pools an account is in, in pool order.
/// Inputs: the account id and the context. Output: the names.
pub fn pool_names(id: &CredentialRef, ctx: &Ctx) -> Vec<String> {
    ctx.store
        .pools
        .ready()
        .map(|pools| {
            pools
                .iter()
                .filter(|pool| pool.members.iter().any(|member| &member.id == id))
                .map(|pool| pool.name.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Sorts accounts the way the list shows them: needs attention first, members
/// of a pool before the rest, then by name.
/// Inputs: the accounts and context. Output: the accounts in list order.
pub fn sorted<'a>(accounts: &'a [AccountView], ctx: &Ctx) -> Vec<&'a AccountView> {
    let mut rows: Vec<&AccountView> = accounts.iter().collect();
    rows.sort_by_cached_key(|account| {
        let attention = attention_rank(account.status);
        let pooled = attention < 2 && pool_names(&account.id, ctx).is_empty();
        (
            attention,
            pooled,
            account.name.to_lowercase(),
            account.id.as_str().to_owned(),
        )
    });
    rows
}

/// One account as the shared lists show it.
/// Input: the account. Output: the row with the plain provider and status words.
pub fn account_row(account: &AccountView) -> AccountRow {
    AccountRow {
        id: account.id.clone(),
        name: account.name.clone(),
        provider: provider_word(account.provider).into(),
        status: account.status_label.clone(),
    }
}

/// Asks the engine for every account; the store shows Loading until the
/// first reply and keeps old rows during a refresh.
/// Input: the context. Output: none.
pub fn load_accounts(ctx: &mut Ctx) {
    if ctx.store.accounts.ready().is_none() {
        ctx.store.accounts = Load::Loading;
    }
    send(ctx, SettingsJob::AllAccounts);
}

/// Keeps the shared account rows in step with a full account load.
/// Inputs: the arrival and the context. Output: none.
pub fn keep_rows(arrival: &Arrival, ctx: &mut Ctx) {
    match settings_event(arrival) {
        Some(SettingsEvent::AllAccounts(Ok(accounts))) => {
            ctx.store.accounts = Load::Ready(accounts.iter().map(account_row).collect());
        }
        Some(SettingsEvent::AllAccounts(Err(code))) if ctx.store.accounts.ready().is_none() => {
            ctx.store.accounts = Load::Failed(code.clone());
        }
        _ => {}
    }
}

fn matches_filter(account: &AccountView, words: &[String], ctx: &Ctx) -> bool {
    let haystack = format!(
        "{} {} {} {}",
        account.name,
        provider_word(account.provider),
        account.status_label,
        pool_names(&account.id, ctx).join(" ")
    )
    .to_lowercase();
    words.iter().all(|word| haystack.contains(word.as_str()))
}

/// Settings, Accounts: every account with provider, sign-in, status and pools,
/// built for hundreds of rows.
/// Input: keys and account pages from the engine. Output: the account table,
/// the empty state, or the error with the key to try again.
pub struct Accounts {
    accounts: Vec<AccountView>,
    load: Load<()>,
    waiting: bool,
    selected: usize,
    offset: usize,
    filter: TextField,
    filtering: bool,
    attention_only: bool,
    marked: BTreeSet<CredentialRef>,
}

impl Default for Accounts {
    fn default() -> Self {
        Self::new()
    }
}

impl Accounts {
    /// The account list with nothing loaded yet.
    /// Input: none. Output: the screen; `shown` asks the engine for accounts.
    pub fn new() -> Self {
        Self {
            accounts: Vec::new(),
            load: Load::Idle,
            waiting: false,
            selected: 0,
            offset: 0,
            filter: TextField::new("Filter", FILTER_CHARS_MAX),
            filtering: false,
            attention_only: false,
            marked: BTreeSet::new(),
        }
    }

    /// The list with accounts already known, as when coming back from details.
    /// Inputs: the accounts. Output: the screen.
    pub fn with(accounts: Vec<AccountView>) -> Self {
        let mut screen = Self::new();
        screen.accounts = accounts;
        screen.load = Load::Ready(());
        screen
    }

    fn reload(&mut self, ctx: &mut Ctx) {
        if self.waiting {
            return;
        }
        if matches!(self.load, Load::Idle | Load::Failed(_)) {
            self.load = Load::Loading;
        }
        self.waiting = true;
        load_accounts(ctx);
    }

    fn loaded(&mut self, accounts: &[AccountView], ctx: &Ctx) {
        self.accounts = accounts.to_vec();
        self.load = Load::Ready(());
        let shown = self.visible(ctx).len();
        self.selected = self.selected.min(shown.saturating_sub(1));
        self.marked
            .retain(|id| accounts.iter().any(|account| &account.id == id));
    }

    fn words(&self) -> Vec<String> {
        self.filter
            .value()
            .split_whitespace()
            .map(str::to_lowercase)
            .collect()
    }

    fn visible<'a>(&'a self, ctx: &Ctx) -> Vec<&'a AccountView> {
        let words = self.words();
        sorted(&self.accounts, ctx)
            .into_iter()
            .filter(|account| !self.attention_only || needs_attention(account.status))
            .filter(|account| words.is_empty() || matches_filter(account, &words, ctx))
            .collect()
    }

    fn rows_visible(ctx: &Ctx) -> usize {
        (ctx.height as usize)
            .saturating_sub(4 + 2 + CHROME_ROWS)
            .max(1)
    }

    fn widths(wide: bool) -> Vec<usize> {
        if wide {
            vec![24, 17, 16, 22, 26]
        } else {
            vec![23, 17, 22, 5]
        }
    }

    fn cells_of(&self, account: &AccountView, ctx: &Ctx) -> Vec<Line<'static>> {
        let pools = pool_names(&account.id, ctx);
        let mut values = vec![
            Line::raw(account.name.clone()),
            Line::raw(provider_cell(account.provider, ctx.glyphs)),
        ];
        if ctx.wide() {
            values.push(Line::raw(sign_in_word(account)));
        }
        values.push(status(&account.status_label, &ctx.theme, ctx.glyphs));
        values.push(Line::raw(match (ctx.wide(), pools.len()) {
            (_, 0) => "-".to_owned(),
            (true, _) => pools.join(", "),
            (false, count) => count.to_string(),
        }));
        values
    }

    fn move_by(&mut self, delta: isize, ctx: &Ctx) {
        let count = self.visible(ctx).len();
        if count == 0 {
            self.selected = 0;
            return;
        }
        let target = self.selected as isize + delta;
        self.selected = target.clamp(0, count as isize - 1) as usize;
    }

    fn open_selected(&self, ctx: &Ctx) -> Flow {
        let visible = self.visible(ctx);
        match visible.get(self.selected) {
            Some(account) => {
                let account = (*account).clone();
                Flow::Replace(Box::new(AccountDetails::new(account, self.snapshot())))
            }
            None => Flow::Handled,
        }
    }

    fn snapshot(&self) -> Accounts {
        let mut list = Accounts::with(self.accounts.clone());
        list.selected = self.selected;
        list.offset = self.offset;
        list.attention_only = self.attention_only;
        list.filter.set(self.filter.value());
        list.marked = self.marked.clone();
        list
    }

    fn filter_key(&mut self, key: &Key, ctx: &Ctx) -> Flow {
        match key {
            Key::Esc => {
                self.filter.clear();
                self.filtering = false;
            }
            Key::Enter | Key::Down | Key::Tab => self.filtering = false,
            other => {
                self.filter.key(other);
            }
        }
        let count = self.visible(ctx).len();
        self.selected = self.selected.min(count.saturating_sub(1));
        Flow::Handled
    }

    /// The body lines of the list and the filter cursor.
    /// Inputs: the content width and context. Output: lines below the tab strip
    /// and the cursor relative to them while the filter has focus.
    pub fn body(&mut self, limit: usize, ctx: &Ctx) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        match &self.load {
            Load::Failed(code) => {
                return (
                    vec![
                        styled("Accounts", Role::Accent, theme),
                        Line::default(),
                        status(
                            &format!("Failed to load accounts. {}", plain(code)),
                            theme,
                            ctx.glyphs,
                        ),
                        Line::raw("Press r to try again."),
                    ],
                    None,
                );
            }
            Load::Ready(()) if self.accounts.is_empty() => {
                let mut lines = vec![
                    styled("Accounts", Role::Accent, theme),
                    Line::default(),
                    Line::raw("No accounts yet."),
                ];
                lines.extend(
                    wrap(
                        "An account lets agents use a model: a ChatGPT or Claude subscription, an API key, or a model at a URL. Add as many as you like.",
                        limit,
                    )
                    .into_iter()
                    .map(Line::raw),
                );
                lines.push(Line::default());
                lines.push(buttons(&["Add account"], Some(0), theme));
                return (lines, None);
            }
            Load::Ready(()) => {}
            _ => {
                return (
                    vec![
                        styled("Accounts", Role::Accent, theme),
                        Line::default(),
                        Line::from(vec![
                            Span::styled(ctx.spinner(), theme.running),
                            Span::raw(" Loading accounts"),
                        ]),
                    ],
                    None,
                );
            }
        }
        let total = self.accounts.len();
        let attention = self
            .accounts
            .iter()
            .filter(|account| needs_attention(account.status))
            .count();
        let mut right = vec![Span::styled(
            format!("{total} account{}", if total == 1 { "" } else { "s" }),
            theme.muted,
        )];
        if attention > 0 {
            right.push(Span::raw("   "));
            right.push(Span::styled(
                format!("{attention} need attention"),
                theme.warning,
            ));
        }
        let mut lines = vec![row(
            limit,
            styled("Accounts", Role::Accent, theme),
            Line::from(right),
            ctx.glyphs,
        )];
        let mut cursor = None;
        if self.filtering {
            let shown = cut(
                self.filter.value(),
                limit.saturating_sub(3),
                ctx.glyphs.ellipsis,
            );
            cursor = Some(Position::new(2 + crate::tui::text::width(&shown) as u16, 1));
            lines.push(join([styled("/ ", Role::Accent, theme), Line::raw(shown)]));
        } else if self.filter.is_empty() {
            lines.push(join([
                Line::raw("/  "),
                styled(
                    "Filter by name, provider, status or pool",
                    Role::Muted,
                    theme,
                ),
            ]));
        } else {
            lines.push(Line::raw(format!("/ {}", self.filter.value())));
        }
        lines.push(Line::default());
        let wide = ctx.wide();
        let widths = Self::widths(wide);
        let heads: Vec<&str> = if wide {
            vec!["Name", "Provider", "Sign-in", "Status", "Pools"]
        } else {
            vec!["Name", "Provider", "Status", "Pools"]
        };
        lines.push(join([
            Line::raw("  "),
            restyle(
                cells(
                    &widths,
                    heads.into_iter().map(Line::raw).collect(),
                    2,
                    ctx.glyphs,
                ),
                theme.muted,
            ),
        ]));
        let visible = self.visible(ctx);
        let window = Self::rows_visible(ctx);
        let count = visible.len();
        let selected = self.selected.min(count.saturating_sub(1));
        let mut offset = self.offset.min(count.saturating_sub(window));
        if selected < offset {
            offset = selected;
        } else if selected >= offset + window {
            offset = selected + 1 - window;
        }
        let row_limit = if count > window { limit - 1 } else { limit };
        let rows: Vec<Line<'static>> = visible
            .iter()
            .enumerate()
            .skip(offset)
            .take(window)
            .map(|(index, account)| {
                let marked = self.marked.contains(&account.id);
                let mut content = cells(&widths, self.cells_of(account, ctx), 2, ctx.glyphs);
                if marked {
                    content = restyle(content, theme.selection_dim);
                }
                list_row(
                    content,
                    row_limit,
                    index == selected,
                    !self.filtering,
                    theme,
                    ctx.glyphs,
                )
            })
            .collect();
        let shown_rows = rows.len();
        if count > window {
            let (top, size) = thumb(window, offset, count);
            lines.extend(with_bar(rows, limit, top, size, theme, ctx.glyphs));
        } else {
            lines.extend(rows);
        }
        for _ in shown_rows..window {
            lines.push(Line::default());
        }
        lines.push(Line::default());
        let filtered = !self.filter.is_empty() || self.attention_only;
        lines.push(Line::raw(if filtered {
            format!("Showing {count} of {total}")
        } else if count == 0 {
            "Showing 0".to_owned()
        } else {
            format!(
                "Showing {}-{} of {count}. Needs attention first, then by name.",
                offset + 1,
                offset + shown_rows
            )
        }));
        self.offset = offset;
        self.selected = selected;
        (lines, cursor)
    }
}

/// A plain sentence for an engine problem code about accounts.
/// Input: the code. Output: the sentence, or a general one.
pub fn plain(code: &str) -> &'static str {
    match code {
        "account_not_found" => "This account no longer exists.",
        "account_in_pool" | "account_in_use" => {
            "This account is in a pool. Remove it from the pool first."
        }
        "account_name_bounds" | "account_name_invalid" => {
            "Use a name of 1 to 96 characters, without control characters."
        }
        "account_secret_invalid" => "This key cannot be saved. Paste the whole key on one line.",
        "account_url_invalid" | "account_url_bounds" => {
            "This address is not a valid http or https URL."
        }
        "account_unreachable" | "account_url_unreachable" => {
            "Could not reach this address. Is the server running?"
        }
        "model_access_read_only" => {
            "Settings are read-only in this engine. Start it with writable settings."
        }
        "trusted_connection_unavailable" | "controller_disconnected" => {
            "The engine connection is not ready yet."
        }
        "controller_pending_intent" => "Another change is still finishing. Try again in a moment.",
        other => crate::config::plain_problem(other).unwrap_or("The engine could not do this."),
    }
}

impl Screen for Accounts {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), "Accounts".into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        if self.filtering {
            return vec![
                hint("Enter", "Keep filter"),
                hint(ctx.glyphs.down, "To the list"),
                hint("Esc", "Clear filter"),
            ];
        }
        if matches!(self.load, Load::Ready(())) && self.accounts.is_empty() {
            return vec![
                hint("Enter", "Add account"),
                sections_hint(ctx.glyphs),
                hint("Esc", "Home"),
            ];
        }
        let mut hints = vec![hint("a", "Add account")];
        if !self.marked.is_empty() {
            hints.push(hint("p", "New pool from selected"));
        }
        hints.extend([
            hint("Enter", "Open"),
            hint("/", "Filter"),
            hint(
                "f",
                if self.attention_only {
                    "All accounts"
                } else {
                    "Needs attention"
                },
            ),
            hint("Space", "Select"),
            sections_hint(ctx.glyphs),
            hint("Esc", "Home"),
        ]);
        hints
    }

    fn typing(&self) -> bool {
        self.filtering
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.filtering {
            return self.filter_key(key, ctx);
        }
        let empty = matches!(self.load, Load::Ready(())) && self.accounts.is_empty();
        let page = Self::rows_visible(ctx).saturating_sub(1).max(1) as isize;
        match key {
            Key::Char('a') => Flow::Open(Place::AddAccount),
            Key::Enter if empty => Flow::Open(Place::AddAccount),
            Key::Enter => self.open_selected(ctx),
            Key::Char('r') if matches!(self.load, Load::Failed(_)) => {
                self.reload(ctx);
                Flow::Handled
            }
            Key::Char('/') if !empty => {
                self.filtering = true;
                Flow::Handled
            }
            Key::Char('f') if !empty => {
                self.attention_only = !self.attention_only;
                self.selected = 0;
                Flow::Handled
            }
            Key::Char('p') => {
                if self.marked.is_empty() {
                    ctx.say(Message::warning("Select accounts with Space first.", ""));
                    return Flow::Handled;
                }
                if self.marked.len() > POOL_MEMBERS_MAX {
                    ctx.say(Message::warning(
                        "A pool holds at most 15 accounts.",
                        "Unselect some, then press p again.",
                    ));
                    return Flow::Handled;
                }
                Flow::Open(Place::NewPoolFrom(self.marked.iter().cloned().collect()))
            }
            Key::Char(' ') => {
                let id = self
                    .visible(ctx)
                    .get(self.selected)
                    .map(|account| account.id.clone());
                if let Some(id) = id {
                    if !self.marked.remove(&id) {
                        self.marked.insert(id);
                    }
                    ctx.say(Message::success(
                        &format!("{} selected.", self.marked.len()),
                        "",
                    ));
                }
                Flow::Handled
            }
            Key::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                Flow::Handled
            }
            Key::Up => {
                self.move_by(-1, ctx);
                Flow::Handled
            }
            Key::Down => {
                self.move_by(1, ctx);
                Flow::Handled
            }
            Key::PageUp => {
                self.move_by(-page, ctx);
                Flow::Handled
            }
            Key::PageDown => {
                self.move_by(page, ctx);
                Flow::Handled
            }
            Key::Home => {
                self.selected = 0;
                Flow::Handled
            }
            Key::End => {
                self.selected = self.visible(ctx).len().saturating_sub(1);
                Flow::Handled
            }
            _ => Flow::Ignored,
        }
    }

    fn step(&mut self, forward: bool, ctx: &mut Ctx) -> Flow {
        step(SettingsSection::Accounts, forward, ctx)
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        let Some(SettingsEvent::AllAccounts(result)) = settings_event(arrival) else {
            return Flow::Handled;
        };
        let asked = std::mem::take(&mut self.waiting);
        match result {
            Ok(accounts) => self.loaded(accounts, ctx),
            Err(code) if self.accounts.is_empty() => self.load = Load::Failed(code.clone()),
            Err(_) if ctx.link != Link::Ready => {}
            Err(code) if asked => {
                ctx.say(Message::error("Could not refresh accounts.", plain(code)))
            }
            Err(_) => {}
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        self.reload(ctx);
        ctx.request(Request::Pools);
        ctx.request(Request::Projects);
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = (area.width as usize).saturating_sub(4).max(8);
        let (content, cursor) = self.body(limit, ctx);
        draw_page(frame, area, SettingsSection::Accounts, content, cursor, ctx)
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        matches!(self.load, Load::Loading | Load::Idle)
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Settings(SettingsSection::Accounts))
    }
}

enum DetailMode {
    View,
    Rename(TextField),
    ReplaceKey(TextField),
    Remove { remove_focused: bool },
}

/// One account: provider, sign-in, model, pools and projects using it, with
/// Sign in again, Replace key, Rename and Remove.
/// Input: the account and the list to return to. Output: the details page.
pub struct AccountDetails {
    account: AccountView,
    list: Option<Box<Accounts>>,
    focus: usize,
    mode: DetailMode,
    waiting: bool,
    replacing: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    SignIn,
    ReplaceKey,
    CheckAgain,
    Rename,
    Remove,
}

impl Action {
    fn label(self) -> &'static str {
        match self {
            Self::SignIn => "Sign in again",
            Self::ReplaceKey => "Replace key",
            Self::CheckAgain => "Check again",
            Self::Rename => "Rename",
            Self::Remove => "Remove",
        }
    }
}

impl AccountDetails {
    /// Details for one account.
    /// Inputs: the account and the list state to restore on Esc. Output: the
    /// screen with the first action focused.
    pub fn new(account: AccountView, list: Accounts) -> Self {
        Self {
            account,
            list: Some(Box::new(list)),
            focus: 0,
            mode: DetailMode::View,
            waiting: false,
            replacing: false,
        }
    }

    fn actions(&self) -> Vec<Action> {
        let first = match self.account.method {
            SignInMethod::Subscription => Some(Action::SignIn),
            SignInMethod::ApiKey => Some(Action::ReplaceKey),
            SignInMethod::Url => Some(Action::CheckAgain),
        };
        first
            .into_iter()
            .chain([Action::Rename, Action::Remove])
            .collect()
    }

    fn back(&mut self) -> Flow {
        let list = self
            .list
            .take()
            .unwrap_or_else(|| Box::new(Accounts::new()));
        Flow::Replace(list)
    }

    fn run(&mut self, action: Action, ctx: &mut Ctx) -> Flow {
        match action {
            Action::SignIn => Flow::Replace(Box::new(
                super::settings_add_account::AddAccount::again(self.account.clone()),
            )),
            Action::ReplaceKey => {
                let field = TextField::secret("New API key", KEY_CHARS_MAX)
                    .counted()
                    .note("Replaces the saved key. The old key is deleted.");
                self.mode = DetailMode::ReplaceKey(field);
                Flow::Handled
            }
            Action::CheckAgain => {
                self.waiting = true;
                send(
                    ctx,
                    SettingsJob::Accounts(AccountsRequest::Check {
                        id: self.account.id.clone(),
                    }),
                );
                Flow::Handled
            }
            Action::Rename => {
                let mut field = TextField::new("Account name", NAME_CHARS_MAX);
                field.set(&self.account.name);
                self.mode = DetailMode::Rename(field);
                Flow::Handled
            }
            Action::Remove => {
                let pools = pool_names(&self.account.id, ctx);
                if pools.is_empty() {
                    self.mode = DetailMode::Remove {
                        remove_focused: false,
                    };
                } else {
                    ctx.say(Message::error(
                        &format!(
                            "{} is in pool {}. Remove it from the pool first.",
                            self.account.name,
                            pools.join(", ")
                        ),
                        "",
                    ));
                }
                Flow::Handled
            }
        }
    }

    fn used_by(&self, ctx: &Ctx) -> String {
        let pools = pool_names(&self.account.id, ctx);
        let projects: Vec<String> = ctx
            .store
            .projects
            .ready()
            .map(|cards| {
                cards
                    .iter()
                    .filter(|card| card.pool.as_ref().is_some_and(|pool| pools.contains(pool)))
                    .map(|card| card.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        if projects.is_empty() {
            "No projects".into()
        } else {
            projects.join(", ")
        }
    }

    fn model(&self) -> String {
        match (&self.account.model, self.account.provider) {
            (Some(model), _) => model.clone(),
            (None, Provider::Chatgpt) => "Default for Codex".into(),
            (None, Provider::Claude) => "Default for Claude Code".into(),
            (None, _) => "Not set".into(),
        }
    }

    /// The body lines of the details page.
    /// Inputs: content width and context. Output: lines and the rename cursor.
    pub fn body(&self, limit: usize, ctx: &Ctx) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        let label = |name: &str, value: Line<'static>| {
            join([
                pad(styled(name, Role::Heading, theme), LABEL_WIDTH, ctx.glyphs),
                value,
            ])
        };
        let mut lines = vec![
            row(
                limit,
                styled(self.account.name.clone(), Role::Accent, theme),
                status(&self.account.status_label, theme, ctx.glyphs),
                ctx.glyphs,
            ),
            Line::default(),
            label(
                "Provider",
                Line::raw(provider_cell(self.account.provider, ctx.glyphs)),
            ),
            label(
                "Sign-in",
                Line::raw(match (self.account.method, self.account.provider) {
                    (SignInMethod::Subscription, Provider::Chatgpt) => "ChatGPT subscription",
                    (SignInMethod::Subscription, _) => "Claude subscription",
                    (SignInMethod::ApiKey, _) => "API key",
                    (SignInMethod::Url, _) => "URL",
                }),
            ),
        ];
        if self.account.method == SignInMethod::ApiKey {
            let saved = self.account.status == AccountStatus::KeySaved;
            lines.push(label(
                "Key",
                if saved {
                    join([
                        status("Key saved", theme, ctx.glyphs),
                        styled("  kept by the engine", Role::Muted, theme),
                    ])
                } else {
                    status("Not signed in", theme, ctx.glyphs)
                },
            ));
        }
        if let Some(url) = &self.account.url {
            lines.push(label("URL", Line::raw(url.clone())));
        }
        lines.push(label("Model", Line::raw(self.model())));
        let pools = pool_names(&self.account.id, ctx);
        lines.push(label(
            "Pools",
            Line::raw(if pools.is_empty() {
                "None".to_owned()
            } else {
                pools.join(", ")
            }),
        ));
        lines.push(label("Used by", Line::raw(self.used_by(ctx))));
        lines.push(Line::default());
        if self.account.status == AccountStatus::NotSignedIn
            && self.account.method == SignInMethod::Subscription
        {
            lines.push(Line::raw(
                "The sign-in expired or never finished. Sign in again to keep using this account.",
            ));
            lines.push(Line::default());
        }
        if self.account.status == AccountStatus::Unreachable {
            lines.push(Line::raw(
                "Could not reach this address. Is the server running?",
            ));
            lines.push(Line::default());
        }
        let mut cursor = None;
        match &self.mode {
            DetailMode::Rename(field) => {
                let top = lines.len() as u16;
                let (field_lines, at) = field.lines(limit.min(64), true, theme, ctx.glyphs);
                cursor = at.map(|at| Position::new(at.x, top + at.y));
                lines.extend(field_lines);
                lines.push(Line::default());
                lines.push(buttons(&["Save name", "Cancel"], None, theme));
            }
            DetailMode::ReplaceKey(field) => {
                let top = lines.len() as u16;
                let (field_lines, at) = field.lines(limit.min(64), true, theme, ctx.glyphs);
                cursor = at.map(|at| Position::new(at.x, top + at.y));
                lines.extend(field_lines);
                lines.push(Line::default());
                lines.push(buttons(&["Save key", "Cancel"], None, theme));
            }
            _ => {
                let labels: Vec<&str> =
                    self.actions().iter().map(|action| action.label()).collect();
                lines.push(buttons(&labels, Some(self.focus), theme));
            }
        }
        (lines, cursor)
    }

    fn rename_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let DetailMode::Rename(field) = &mut self.mode else {
            return Flow::Ignored;
        };
        match key {
            Key::Esc => self.mode = DetailMode::View,
            Key::Enter => {
                let name = field.value().trim().to_owned();
                if name.is_empty() {
                    field.error = Some("Type a name for this account.".into());
                    return Flow::Handled;
                }
                self.waiting = true;
                send(
                    ctx,
                    SettingsJob::Accounts(AccountsRequest::Rename {
                        id: self.account.id.clone(),
                        name,
                    }),
                );
                self.mode = DetailMode::View;
            }
            other => {
                field.error = None;
                field.key(other);
            }
        }
        Flow::Handled
    }

    fn replace_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let DetailMode::ReplaceKey(field) = &mut self.mode else {
            return Flow::Ignored;
        };
        match key {
            Key::Esc => self.mode = DetailMode::View,
            Key::Enter => {
                field.error = key_problem(self.account.provider, field.len(), true);
                if field.error.is_some() {
                    return Flow::Handled;
                }
                let Ok(secret) = AccountSecret::new(field.take_secret()) else {
                    field.error =
                        Some("This key cannot be saved. Paste the whole key on one line.".into());
                    return Flow::Handled;
                };
                self.waiting = true;
                self.replacing = true;
                send(
                    ctx,
                    SettingsJob::Accounts(AccountsRequest::ReplaceKey {
                        id: self.account.id.clone(),
                        key: secret,
                    }),
                );
                self.mode = DetailMode::View;
            }
            other => {
                field.error = None;
                field.key(other);
            }
        }
        Flow::Handled
    }

    fn remove_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let DetailMode::Remove { remove_focused } = &mut self.mode else {
            return Flow::Ignored;
        };
        match key {
            Key::Esc => self.mode = DetailMode::View,
            Key::Left | Key::Right | Key::Tab | Key::BackTab => *remove_focused = !*remove_focused,
            Key::Enter if *remove_focused => {
                self.mode = DetailMode::View;
                self.waiting = true;
                send(
                    ctx,
                    SettingsJob::Accounts(AccountsRequest::Remove {
                        id: self.account.id.clone(),
                    }),
                );
            }
            Key::Enter => self.mode = DetailMode::View,
            _ => {}
        }
        Flow::Handled
    }
}

impl Screen for AccountDetails {
    fn crumbs(&self) -> Vec<String> {
        vec![
            "Settings".into(),
            "Accounts".into(),
            self.account.name.clone(),
        ]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let arrows = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
        match self.mode {
            DetailMode::Rename(_) => vec![
                hint("Enter", "Save name"),
                hint("Ctrl-U", "Clear"),
                hint("Esc", "Cancel"),
            ],
            DetailMode::ReplaceKey(_) => vec![
                hint("Enter", "Save key"),
                hint("Ctrl-U", "Clear"),
                hint("Esc", "Cancel"),
            ],
            _ => vec![
                hint(&arrows, "Choose"),
                hint("Enter", "Select"),
                hint("Esc", "Back"),
            ],
        }
    }

    fn typing(&self) -> bool {
        matches!(self.mode, DetailMode::Rename(_) | DetailMode::ReplaceKey(_))
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        match self.mode {
            DetailMode::Rename(_) => return self.rename_key(key, ctx),
            DetailMode::ReplaceKey(_) => return self.replace_key(key, ctx),
            DetailMode::Remove { .. } => return self.remove_key(key, ctx),
            DetailMode::View => {}
        }
        let count = self.actions().len();
        match key {
            Key::Esc => self.back(),
            Key::Left | Key::BackTab => {
                self.focus = (self.focus + count - 1) % count;
                Flow::Handled
            }
            Key::Right | Key::Tab => {
                self.focus = (self.focus + 1) % count;
                Flow::Handled
            }
            Key::Enter if !self.waiting => {
                let action = self.actions()[self.focus.min(count - 1)];
                self.run(action, ctx)
            }
            _ => Flow::Handled,
        }
    }

    fn step(&mut self, forward: bool, ctx: &mut Ctx) -> Flow {
        step(SettingsSection::Accounts, forward, ctx)
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        let Some(SettingsEvent::Accounts(result)) = settings_event(arrival) else {
            return Flow::Handled;
        };
        if !self.waiting {
            return Flow::Handled;
        }
        self.waiting = false;
        let replacing = std::mem::take(&mut self.replacing);
        match result {
            Ok(AccountsReply::Account(account)) if replacing => {
                self.account = account.clone();
                ctx.say(Message::success(
                    "Key replaced.",
                    "Agents use the new key from their next start.",
                ));
            }
            Ok(AccountsReply::Account(account)) => {
                self.account = account.clone();
                ctx.say(Message::success("Saved.", ""));
            }
            Ok(AccountsReply::Checked(check)) => {
                self.account = check.account.clone();
                if check.account.status == AccountStatus::Unreachable {
                    ctx.say(Message::error(
                        "Could not reach this address.",
                        "Is the server running?",
                    ));
                } else {
                    ctx.say(Message::success(
                        &format!("Connected. {} models found.", check.models.len()),
                        "",
                    ));
                }
            }
            Ok(AccountsReply::Removed(_)) => {
                ctx.say(Message::success("Account removed.", ""));
                ctx.request(Request::Pools);
                return Flow::Replace(Box::new(Accounts::new()));
            }
            Ok(_) => {}
            Err(_) if ctx.link != Link::Ready => {}
            Err(code) => ctx.say(Message::error(plain(code), "")),
        }
        Flow::Handled
    }

    fn modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        let DetailMode::Remove { remove_focused } = self.mode else {
            return None;
        };
        let arrows = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
        let mut body = vec![
            Line::raw(format!("Remove {}?", self.account.name)),
            Line::default(),
        ];
        let note = match self.account.method {
            SignInMethod::ApiKey => {
                "It is in no pool, so no agent uses it. Its saved key is deleted too."
            }
            _ => "It is in no pool, so no agent uses it.",
        };
        body.extend(wrap(note, 50).into_iter().map(Line::raw));
        body.push(Line::default());
        body.push(buttons(
            &["Cancel", "Remove"],
            Some(usize::from(remove_focused)),
            &ctx.theme,
        ));
        Some((
            Modal {
                width: 54,
                title: "Remove account".into(),
                body,
                top: None,
            },
            vec![
                hint(&arrows, "Choose"),
                hint("Enter", "Confirm"),
                hint("Esc", "Cancel"),
            ],
        ))
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        ctx.request(Request::Pools);
        ctx.request(Request::Projects);
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = (area.width as usize).saturating_sub(4).max(8);
        let (content, cursor) = self.body(limit, ctx);
        draw_page(frame, area, SettingsSection::Accounts, content, cursor, ctx)
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.waiting
    }

    fn draft(&self) -> bool {
        match &self.mode {
            DetailMode::Rename(_) => true,
            DetailMode::ReplaceKey(field) => !field.is_empty(),
            _ => false,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
