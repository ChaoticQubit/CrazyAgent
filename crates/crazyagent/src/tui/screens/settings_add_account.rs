use super::settings::draw_page;
use super::settings_accounts::{
    Accounts, plain, provider_cell, provider_word, send, settings_event,
};
use super::{Flow, Screen, SettingsSection};
use crate::accounts::{
    AccountDraft, AccountStatus, AccountView, Field, MethodEntry, Provider, ProviderEntry,
    SignInMethod,
};
use crate::config::POOL_MEMBERS_MAX;
use crate::engine::owner::{AccountSecret, NativeAuthMethod, NativeAuthPhase};
use crate::protocol::CredentialRef;
use crate::tui::client::accounts::{AccountsReply, AccountsRequest};
use crate::tui::client::settings::{
    SettingsEvent, SettingsJob, SignInJob, SignInTicket, SignInView,
};
use crate::tui::ctx::{Ctx, Link, Load};
use crate::tui::frame::{Hint, Message, hint};
use crate::tui::keys::Key;
use crate::tui::seam::{Arrival, Request};
use crate::tui::text::{cells, join, list_row, status, styled, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::{TextField, buttons, checkbox, menu_item};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use std::collections::BTreeSet;

const FIELD_WIDTH_MAX: usize = 64;
const KEY_CHARS_MAX: usize = 4096;
const KEY_CHARS_MIN: usize = 20;
const CODE_CHARS_MAX: usize = 2048;
const URL_CHARS_MAX: usize = crate::accounts::ACCOUNT_URL_MAX;
const MODEL_CHARS_MAX: usize = crate::accounts::ACCOUNT_MODEL_MAX;
const NAME_CHARS_MAX: usize = crate::accounts::ACCOUNT_NAME_MAX;
const FAST_READS_MAX: u32 = 30;
const MODELS_SHOWN_MAX: usize = 8;

/// One sign-in choice on the method menu.
/// Input: built from the engine's provider catalog. Output: the title,
/// description and the native method a subscription uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub provider: Provider,
    pub method: SignInMethod,
    pub native: Option<NativeAuthMethod>,
    pub title: &'static str,
    pub description: &'static str,
}

/// The one-line description of a provider on the first step.
/// Input: the provider. Output: the plain sentence.
pub fn provider_line(provider: Provider) -> &'static str {
    match provider {
        Provider::Chatgpt => "Sign in with ChatGPT, or use an OpenAI API key",
        Provider::Claude => "Sign in with your Claude plan, or use an API key",
        Provider::Openrouter => "One API key for models from many companies",
        Provider::OpenaiApi => "Pay-as-you-go OpenAI API key",
        Provider::AnthropicApi => "Pay-as-you-go Anthropic API key",
        Provider::OpenaiCompatible => "Any server that speaks the OpenAI API",
        Provider::Local => "Ollama, LM Studio or another server on this Mac",
    }
}

/// The sign-in choices of one provider, in menu order.
/// Input: the engine's catalog entry. Output: choices; a ChatGPT subscription
/// offers a device code and the Codex sign-in already on this Mac.
pub fn choices(entry: &ProviderEntry) -> Vec<Choice> {
    let mut out = Vec::new();
    for method in &entry.methods {
        let choice = |native, title, description| Choice {
            provider: entry.provider,
            method: method.method,
            native,
            title,
            description,
        };
        match (entry.provider, method.method) {
            (Provider::Chatgpt, SignInMethod::Subscription) => {
                out.push(choice(
                    Some(NativeAuthMethod::CodexDeviceCode),
                    "Sign in with ChatGPT",
                    "Recommended. Uses your ChatGPT plan. You get a code to enter on any device.",
                ));
            }
            (_, SignInMethod::Subscription) => out.push(choice(
                Some(NativeAuthMethod::ClaudeOfficial),
                "Sign in with your Claude subscription",
                "Uses your Claude Pro or Max plan. Claude opens in your browser.",
            )),
            (Provider::Chatgpt | Provider::OpenaiApi, SignInMethod::ApiKey) => out.push(choice(
                None,
                "Use an API key",
                "Pay per use with an OpenAI key.",
            )),
            (Provider::Claude | Provider::AnthropicApi, SignInMethod::ApiKey) => out.push(choice(
                None,
                "Use an API key",
                "Pay per use with an Anthropic key.",
            )),
            (_, SignInMethod::ApiKey) => {
                out.push(choice(None, "Use an API key", "Pay per use with a key."))
            }
            (_, SignInMethod::Url) => out.push(choice(
                None,
                "Connect a URL",
                "Any server that speaks the OpenAI API.",
            )),
        }
    }
    if entry.provider == Provider::Chatgpt
        && entry
            .methods
            .iter()
            .any(|method| method.method == SignInMethod::Subscription)
    {
        out.push(Choice {
            provider: Provider::Chatgpt,
            method: SignInMethod::Subscription,
            native: Some(NativeAuthMethod::CodexExisting),
            title: "Use the Codex sign-in on this Mac",
            description: "Reuses the account Codex is already signed in to.",
        });
    }
    out
}

fn key_owner(provider: Provider) -> &'static str {
    match provider {
        Provider::Chatgpt | Provider::OpenaiApi => "an OpenAI",
        Provider::Claude | Provider::AnthropicApi => "an Anthropic",
        Provider::Openrouter => "an OpenRouter",
        _ => "this",
    }
}

/// Checks a typed key length locally; the provider checks it on first use.
/// Inputs: provider and the number of characters. Output: the field error.
pub fn key_problem(provider: Provider, chars: usize, required: bool) -> Option<String> {
    if chars == 0 {
        return required.then(|| "Paste the API key.".to_owned());
    }
    (chars < KEY_CHARS_MIN).then(|| {
        format!(
            "This is too short for {} key. Paste the whole key.",
            key_owner(provider)
        )
    })
}

fn url_problem(provider: Provider, text: &str) -> Option<&'static str> {
    if text.trim().is_empty() {
        return Some("Type the server address.");
    }
    let checked = if provider == Provider::Local {
        crate::accounts::local_url(text.trim()).map(|_| ())
    } else {
        crate::accounts::account_url(text.trim()).map(|_| ())
    };
    match checked {
        Ok(()) => None,
        Err(error) if error.0 == "account_local_url_required" => {
            Some("Use an http address on this Mac, such as http://127.0.0.1:11434/v1.")
        }
        Err(_) => Some("Use an https address. Plain http works only on this Mac."),
    }
}

fn loopback(text: &str) -> bool {
    crate::accounts::local_url(text.trim()).is_ok()
}

fn copy(text: &str) -> Result<(), String> {
    crate::platform::copy_text(text).map_err(String::from)
}

/// Opens an https page in the default browser.
/// Input: the address from the engine. Output: Ok once handed to the system's
/// opener; other schemes are refused.
pub fn open_page(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("page_refused".into());
    }
    crate::platform::open(std::ffi::OsStr::new(url)).map_err(|code| match code {
        "open_failed" => "page_failed".to_owned(),
        other => other.to_owned(),
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Slot {
    Name,
    Url,
    Key,
    Model,
    Button,
}

struct Form {
    choice: Choice,
    entry: MethodEntry,
    name: TextField,
    url: Option<TextField>,
    key: Option<TextField>,
    model: Option<TextField>,
    models: Load<Vec<String>>,
    model_selected: usize,
    focus: usize,
}

impl Form {
    fn new(choice: Choice, entry: MethodEntry) -> Self {
        let mut name = TextField::new("Account name", NAME_CHARS_MAX);
        name.set(provider_word(choice.provider));
        let url = (entry.url != Field::Hidden).then(|| {
            let mut field = TextField::new("Server URL", URL_CHARS_MAX)
                .note("An OpenAI-compatible address. It usually ends in /v1.");
            if let Some(preset) = &entry.url_preset {
                field.set(preset);
            }
            field
        });
        let key = (entry.key != Field::Hidden).then(|| {
            let field = TextField::secret("API key", KEY_CHARS_MAX).counted();
            if entry.key == Field::Optional {
                field
                    .right("Optional")
                    .placeholder("Leave empty if the server has no key")
            } else {
                field.note("Kept by the CrazyAgent engine. Never shown again.")
            }
        });
        let model = (entry.model != Field::Hidden).then(|| {
            TextField::new("Model", MODEL_CHARS_MAX)
                .placeholder("The model name your provider lists")
        });
        Self {
            choice,
            entry,
            name,
            url,
            key,
            model,
            models: Load::Idle,
            model_selected: 0,
            focus: 0,
        }
    }

    fn slots(&self) -> Vec<Slot> {
        let mut slots = vec![Slot::Name];
        if self.url.is_some() {
            slots.push(Slot::Url);
        }
        if self.key.is_some() {
            slots.push(Slot::Key);
        }
        if self.model.is_some() {
            slots.push(Slot::Model);
        }
        slots.push(Slot::Button);
        slots
    }

    fn slot(&self) -> Slot {
        let slots = self.slots();
        slots[self.focus.min(slots.len() - 1)]
    }

    fn listing(&self) -> Option<&Vec<String>> {
        self.models.ready().filter(|models| !models.is_empty())
    }

    fn typing(&self) -> bool {
        match self.slot() {
            Slot::Model => self.listing().is_none(),
            Slot::Button => false,
            _ => true,
        }
    }

    fn unreachable(&self) -> bool {
        matches!(self.models, Load::Failed(_))
    }

    fn probe(&mut self, ctx: &mut Ctx) -> bool {
        let Some(url) = &self.url else {
            return false;
        };
        let text = url.value().trim().to_owned();
        if !loopback(&text) || self.model.is_none() {
            return false;
        }
        self.models = Load::Loading;
        send(
            ctx,
            SettingsJob::Accounts(AccountsRequest::LocalModels { url: text }),
        );
        true
    }

    fn check(&mut self) -> bool {
        let mut ok = true;
        if let Some(url) = &mut self.url {
            url.error = url_problem(self.choice.provider, url.value()).map(str::to_owned);
            ok &= url.error.is_none();
        }
        if let Some(key) = &mut self.key {
            key.error = key_problem(
                self.choice.provider,
                key.len(),
                self.entry.key == Field::Required,
            );
            ok &= key.error.is_none();
        }
        if self.listing().is_none()
            && let Some(model) = &mut self.model
        {
            model.error = (self.entry.model == Field::Required && model.value().trim().is_empty())
                .then(|| "Type the model name to use.".to_owned());
            ok &= model.error.is_none();
        }
        ok
    }

    fn draft(&self) -> AccountDraft {
        let model = match self.listing() {
            Some(models) => models.get(self.model_selected).cloned(),
            None => self
                .model
                .as_ref()
                .map(|field| field.value().trim().to_owned()),
        };
        AccountDraft {
            provider: self.choice.provider,
            method: self.choice.method,
            name: self.name.value().trim().to_owned(),
            url: self
                .url
                .as_ref()
                .map(|field| field.value().trim().to_owned()),
            model: model.filter(|model| !model.is_empty()),
        }
    }

    fn lines(&self, limit: usize, ctx: &Ctx) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        let width = limit.min(FIELD_WIDTH_MAX);
        let title = if self.choice.method == SignInMethod::Url {
            "Connect a URL"
        } else {
            "Use an API key"
        };
        let mut lines = vec![
            styled(title, Role::Accent, theme),
            Line::raw(provider_word(self.choice.provider)),
            Line::default(),
        ];
        let mut cursor = None;
        let focus = self.slot();
        let mut put = |field: &TextField, slot: Slot, lines: &mut Vec<Line<'static>>| {
            let top = lines.len() as u16;
            let (drawn, at) = field.lines(width, focus == slot, theme, ctx.glyphs);
            if let Some(at) = at {
                cursor = Some(Position::new(at.x, top + at.y));
            }
            lines.extend(drawn);
        };
        put(&self.name, Slot::Name, &mut lines);
        if let Some(url) = &self.url {
            put(url, Slot::Url, &mut lines);
        }
        if let Some(key) = &self.key {
            put(key, Slot::Key, &mut lines);
        }
        lines.push(Line::default());
        if let Some(model) = &self.model {
            match &self.models {
                Load::Loading => lines.push(Line::from(vec![
                    Span::styled(ctx.spinner(), theme.running),
                    Span::raw(" Checking the server..."),
                ])),
                Load::Ready(models) if !models.is_empty() => {
                    lines.push(status(
                        &format!(
                            "Connected. {} model{} found",
                            models.len(),
                            if models.len() == 1 { "" } else { "s" }
                        ),
                        theme,
                        ctx.glyphs,
                    ));
                    lines.push(styled(
                        "Default model",
                        if focus == Slot::Model {
                            Role::Accent
                        } else {
                            Role::Heading
                        },
                        theme,
                    ));
                    let first = self.model_selected.saturating_sub(MODELS_SHOWN_MAX - 1);
                    for (index, name) in
                        models.iter().enumerate().skip(first).take(MODELS_SHOWN_MAX)
                    {
                        lines.push(list_row(
                            Line::raw(name.clone()),
                            width,
                            index == self.model_selected,
                            focus == Slot::Model,
                            theme,
                            ctx.glyphs,
                        ));
                    }
                    lines.push(Line::default());
                }
                _ => {
                    put(model, Slot::Model, &mut lines);
                    lines.push(Line::default());
                }
            }
        }
        let label = if self.unreachable() {
            "Check again"
        } else if self.choice.method == SignInMethod::Url {
            "Save account"
        } else {
            "Save key"
        };
        lines.push(buttons(
            &[label],
            (focus == Slot::Button).then_some(0),
            theme,
        ));
        (lines, cursor)
    }
}

struct SignIn {
    choice: Choice,
    account: Option<AccountView>,
    created: bool,
    ticket: Option<SignInTicket>,
    view: Option<SignInView>,
    code: TextField,
    opened: bool,
    reads: u32,
    problem: Option<String>,
}

impl SignIn {
    fn new(choice: Choice, account: Option<AccountView>, created: bool) -> Self {
        Self {
            choice,
            account,
            created,
            ticket: None,
            view: None,
            code: TextField::secret("If Claude shows a code, paste it here", CODE_CHARS_MAX)
                .note("Paste with Cmd-V. The code stays hidden."),
            opened: false,
            reads: 0,
            problem: None,
        }
    }

    fn phase(&self) -> Option<NativeAuthPhase> {
        self.view.as_ref().map(|view| view.phase)
    }

    fn moving(&self) -> bool {
        self.problem.is_none()
            && self.phase().is_none_or(|phase| {
                matches!(
                    phase,
                    NativeAuthPhase::Starting
                        | NativeAuthPhase::AwaitingOwner
                        | NativeAuthPhase::CheckingAccount
                        | NativeAuthPhase::Cancelling
                        | NativeAuthPhase::Uncertain
                )
            })
    }

    fn page(&self) -> Option<&str> {
        self.view.as_ref().and_then(|view| view.page.as_deref())
    }

    fn lines(&self, limit: usize, ctx: &Ctx) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        let mut lines = vec![
            styled(self.choice.title, Role::Accent, theme),
            Line::default(),
        ];
        let mut cursor = None;
        let waiting = Line::from(vec![
            Span::styled(ctx.spinner(), theme.running),
            Span::raw(" Waiting for you to finish signing in..."),
        ]);
        match self.choice.native {
            Some(NativeAuthMethod::ClaudeOfficial) => {
                if self.page().is_some() {
                    lines.push(Line::raw("Your browser opened the Claude sign-in page."));
                    lines.push(Line::raw("Approve access there, then come back here."));
                    lines.push(Line::default());
                    lines.push(Line::raw("Browser didn't open? Press o to open it again."));
                    lines.push(Line::default());
                    let accepts = self
                        .view
                        .as_ref()
                        .is_some_and(|view| view.claude_prompt.is_some());
                    if accepts {
                        let top = lines.len() as u16;
                        let (field, at) =
                            self.code
                                .lines(limit.min(FIELD_WIDTH_MAX), true, theme, ctx.glyphs);
                        cursor = at.map(|at| Position::new(at.x, top + at.y));
                        lines.extend(field);
                        lines.push(Line::default());
                    }
                } else if self.problem.is_none() {
                    lines.push(Line::raw("Starting the Claude sign-in..."));
                    lines.push(Line::default());
                }
            }
            Some(NativeAuthMethod::CodexExisting) => {
                lines.push(Line::raw(
                    "Checking the account Codex is signed in to on this Mac.",
                ));
                lines.push(Line::default());
            }
            _ => {
                let view = self.view.as_ref();
                match (
                    view.and_then(|v| v.page.clone()),
                    view.and_then(|v| v.device_code.clone()),
                ) {
                    (Some(page), Some(code)) => {
                        lines.push(Line::raw("1. Open this page on any device"));
                        lines.push(Line::raw(format!("   {page}")));
                        lines.push(Line::raw("2. Enter this code"));
                        lines.push(join([Line::raw("   "), styled(code, Role::Heading, theme)]));
                        lines.push(Line::raw("3. Approve CrazyAgent in ChatGPT"));
                        lines.push(Line::default());
                    }
                    (Some(page), None) => {
                        lines.push(Line::raw("1. Open this page in your browser"));
                        lines.push(Line::raw(format!("   {page}")));
                        lines.push(Line::raw("2. Approve CrazyAgent in ChatGPT"));
                        lines.push(Line::default());
                    }
                    _ if self.problem.is_none() => {
                        lines.push(Line::raw("Asking ChatGPT for a sign-in code..."));
                        lines.push(Line::default());
                    }
                    _ => {}
                }
            }
        }
        match &self.problem {
            Some(problem) => {
                for part in wrap(problem, limit) {
                    lines.push(styled(part, Role::Error, theme));
                }
                lines.push(Line::raw(
                    if self.choice.native == Some(NativeAuthMethod::CodexDeviceCode) {
                        "Press n for a new code."
                    } else {
                        "Press n to try again."
                    },
                ));
            }
            None => lines.push(waiting),
        }
        (lines, cursor)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DoneFocus {
    Name,
    Pools,
    Button,
}

struct Done {
    account: AccountView,
    headline: &'static str,
    detail: Vec<String>,
    name: TextField,
    picked: BTreeSet<CredentialRef>,
    selected: usize,
    focus: DoneFocus,
    saving: usize,
}

impl Done {
    fn new(account: AccountView, headline: &'static str, detail: Vec<String>, name: &str) -> Self {
        let mut field = TextField::new("Account name", NAME_CHARS_MAX);
        field.set(name);
        Self {
            account,
            headline,
            detail,
            name: field,
            picked: BTreeSet::new(),
            selected: 0,
            focus: DoneFocus::Pools,
            saving: 0,
        }
    }

    fn lines(&self, limit: usize, ctx: &Ctx) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        let mut lines = vec![status(self.headline, theme, ctx.glyphs), Line::default()];
        for sentence in &self.detail {
            lines.extend(wrap(sentence, limit).into_iter().map(Line::raw));
        }
        lines.push(Line::default());
        let top = lines.len() as u16;
        let (field, at) = self.name.lines(
            limit.min(FIELD_WIDTH_MAX),
            self.focus == DoneFocus::Name,
            theme,
            ctx.glyphs,
        );
        let cursor = at.map(|at| Position::new(at.x, top + at.y));
        lines.extend(field);
        lines.push(Line::default());
        lines.push(styled(
            "Add to pools",
            if self.focus == DoneFocus::Pools {
                Role::Accent
            } else {
                Role::Heading
            },
            theme,
        ));
        match ctx.store.pools.ready() {
            Some(pools) if pools.is_empty() => {
                lines.push(styled(
                    "No pools yet. Create one in Settings, Pools.",
                    Role::Muted,
                    theme,
                ));
            }
            Some(pools) => {
                for (index, pool) in pools.iter().enumerate() {
                    let on = self.picked.contains(&pool.id);
                    lines.push(list_row(
                        cells(
                            &[20, 12],
                            vec![
                                Line::raw(checkbox(on, &pool.name)),
                                Line::raw(format!("{} of {POOL_MEMBERS_MAX}", pool.members.len())),
                            ],
                            2,
                            ctx.glyphs,
                        ),
                        limit.min(FIELD_WIDTH_MAX),
                        index == self.selected,
                        self.focus == DoneFocus::Pools,
                        theme,
                        ctx.glyphs,
                    ));
                }
            }
            None => lines.push(Line::from(vec![
                Span::styled(ctx.spinner(), theme.running),
                Span::raw(" Loading pools"),
            ])),
        }
        lines.push(Line::default());
        lines.push(join([
            buttons(
                &["Done"],
                (self.focus == DoneFocus::Button).then_some(0),
                theme,
            ),
            Line::raw("   a  Add another account"),
        ]));
        (lines, cursor)
    }
}

enum Stage {
    Provider {
        selected: usize,
    },
    Method {
        entry: ProviderEntry,
        selected: usize,
    },
    Form(Box<Form>),
    SignIn(Box<SignIn>),
    Done(Box<Done>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Waiting {
    Nothing,
    Catalog,
    Add,
    Models,
    SignIn,
    Finish,
}

/// Settings, Add account: provider, then that provider's sign-in methods,
/// then the method's own step, then the result with pools to join.
/// Input: keys and engine replies. Output: a new account, signed in or with
/// its key saved; secrets go to the engine only.
pub struct AddAccount {
    catalog: Load<Vec<ProviderEntry>>,
    stage: Stage,
    again: bool,
    waiting: Waiting,
}

impl Default for AddAccount {
    fn default() -> Self {
        Self::new()
    }
}

impl AddAccount {
    /// The flow at its first step.
    /// Input: none. Output: the screen; `shown` loads the provider catalog.
    pub fn new() -> Self {
        Self {
            catalog: Load::Idle,
            stage: Stage::Provider { selected: 0 },
            again: false,
            waiting: Waiting::Nothing,
        }
    }

    /// Signs an existing subscription account in again, from its details.
    /// Input: the account. Output: the screen at the sign-in step, or the
    /// method step for an account that is not a subscription.
    pub fn again(account: AccountView) -> Self {
        let mut screen = Self::new();
        screen.again = true;
        if account.method == SignInMethod::Subscription {
            let native = if account.provider == Provider::Chatgpt {
                NativeAuthMethod::CodexDeviceCode
            } else {
                NativeAuthMethod::ClaudeOfficial
            };
            let choice = Choice {
                provider: account.provider,
                method: SignInMethod::Subscription,
                native: Some(native),
                title: if native == NativeAuthMethod::CodexDeviceCode {
                    "Sign in with ChatGPT"
                } else {
                    "Sign in with your Claude subscription"
                },
                description: "",
            };
            screen.stage = Stage::SignIn(Box::new(SignIn::new(choice, Some(account), false)));
        }
        screen
    }

    fn entries(&self) -> &[ProviderEntry] {
        self.catalog.ready().map_or(&[], Vec::as_slice)
    }

    fn start_sign_in(&mut self, ctx: &mut Ctx) {
        let Stage::SignIn(sign_in) = &mut self.stage else {
            return;
        };
        let Some(method) = sign_in.choice.native else {
            return;
        };
        let Some(account) = sign_in.account.as_ref().map(|account| account.id.clone()) else {
            return;
        };
        sign_in.ticket = None;
        sign_in.view = None;
        sign_in.problem = None;
        sign_in.opened = false;
        sign_in.reads = 0;
        sign_in.code.clear();
        self.waiting = Waiting::SignIn;
        send(
            ctx,
            SettingsJob::SignIn(SignInJob::Start { account, method }),
        );
    }

    fn read_sign_in(&mut self, ctx: &mut Ctx) {
        if self.waiting != Waiting::Nothing {
            return;
        }
        let Stage::SignIn(sign_in) = &self.stage else {
            return;
        };
        let Some(ticket) = sign_in.ticket.clone() else {
            return;
        };
        if !sign_in.moving() {
            return;
        }
        self.waiting = Waiting::SignIn;
        send(ctx, SettingsJob::SignIn(SignInJob::Read { ticket }));
    }

    fn choose(&mut self, choice: Choice, ctx: &mut Ctx) {
        let entry = self
            .entries()
            .iter()
            .find(|entry| entry.provider == choice.provider)
            .and_then(|entry| {
                entry
                    .methods
                    .iter()
                    .find(|method| method.method == choice.method)
            })
            .cloned();
        let Some(entry) = entry else {
            ctx.say(Message::error("This sign-in method is not ready yet.", ""));
            return;
        };
        if choice.method == SignInMethod::Subscription {
            self.waiting = Waiting::Add;
            let draft = AccountDraft {
                provider: choice.provider,
                method: choice.method,
                name: provider_word(choice.provider).to_owned(),
                url: None,
                model: None,
            };
            self.stage = Stage::SignIn(Box::new(SignIn::new(choice, None, true)));
            send(
                ctx,
                SettingsJob::Accounts(AccountsRequest::Add { draft, key: None }),
            );
            return;
        }
        let mut form = Form::new(choice, entry);
        if form.probe(ctx) {
            self.waiting = Waiting::Models;
        }
        self.stage = Stage::Form(Box::new(form));
    }

    fn open_provider(&mut self, entry: ProviderEntry, ctx: &mut Ctx) {
        let options = choices(&entry);
        if options.len() == 1 {
            let choice = options.into_iter().next().expect("one choice");
            self.choose(choice, ctx);
        } else {
            self.stage = Stage::Method { entry, selected: 0 };
        }
    }

    fn back_to_choice(&mut self, provider: Provider) {
        let entry = self
            .entries()
            .iter()
            .find(|entry| entry.provider == provider)
            .cloned();
        let index = self
            .entries()
            .iter()
            .position(|entry| entry.provider == provider)
            .unwrap_or(0);
        self.stage = match entry {
            Some(entry) if choices(&entry).len() > 1 => Stage::Method { entry, selected: 0 },
            _ => Stage::Provider { selected: index },
        };
    }

    fn leave(&self) -> Flow {
        if self.again {
            Flow::Replace(Box::new(Accounts::new()))
        } else {
            Flow::Back
        }
    }

    fn submit_form(&mut self, ctx: &mut Ctx) {
        let Stage::Form(form) = &mut self.stage else {
            return;
        };
        let unprobed = matches!(form.models, Load::Idle | Load::Failed(_));
        if unprobed && form.probe(ctx) {
            self.waiting = Waiting::Models;
            return;
        }
        if !form.check() {
            ctx.say(Message::error(
                "Some fields need a fix.",
                "See the notes under them.",
            ));
            return;
        }
        let key = match &mut form.key {
            Some(field) if !field.is_empty() => match AccountSecret::new(field.take_secret()) {
                Ok(secret) => Some(secret),
                Err(_) => {
                    field.error =
                        Some("This key cannot be saved. Paste the whole key on one line.".into());
                    return;
                }
            },
            _ => None,
        };
        self.waiting = Waiting::Add;
        send(
            ctx,
            SettingsJob::Accounts(AccountsRequest::Add {
                draft: form.draft(),
                key,
            }),
        );
    }

    fn finish(&mut self, ctx: &mut Ctx) -> Flow {
        let Stage::Done(done) = &mut self.stage else {
            return Flow::Handled;
        };
        let name = done.name.value().trim().to_owned();
        if !name.is_empty() && name != done.account.name {
            done.saving += 1;
            send(
                ctx,
                SettingsJob::Accounts(AccountsRequest::Rename {
                    id: done.account.id.clone(),
                    name,
                }),
            );
        }
        if !done.picked.is_empty() {
            let pools: Vec<(CredentialRef, u64)> = done
                .picked
                .iter()
                .map(|pool| (pool.clone(), ctx.next_generation()))
                .collect();
            done.saving += 1;
            send(
                ctx,
                SettingsJob::JoinPools {
                    account: done.account.id.clone(),
                    pools,
                },
            );
        }
        if done.saving == 0 {
            return self.leave();
        }
        self.waiting = Waiting::Finish;
        Flow::Handled
    }

    fn done_from_sign_in(&mut self, ctx: &mut Ctx) {
        let Stage::SignIn(sign_in) = &self.stage else {
            return;
        };
        let view = sign_in.view.clone();
        let Some(account) = sign_in.account.clone() else {
            return;
        };
        let plan = view
            .as_ref()
            .and_then(|view| view.plan.clone())
            .map(|plan| {
                let mut letters = plan.chars();
                let first = letters
                    .next()
                    .map(|c| c.to_uppercase().collect::<String>())
                    .unwrap_or_default();
                let brand = if account.provider == Provider::Chatgpt {
                    "ChatGPT"
                } else {
                    "Claude"
                };
                format!("{brand} {first}{}", letters.as_str())
            });
        let email = view.as_ref().and_then(|view| view.email.clone());
        let detail = match (&email, &plan) {
            (Some(email), Some(plan)) => vec![format!("Signed in as {email} ({plan}).")],
            (Some(email), None) => vec![format!("Signed in as {email}.")],
            _ => Vec::new(),
        };
        let name = match (&plan, &email) {
            (Some(plan), Some(email)) if account.name == provider_word(account.provider) => {
                format!("{plan} - {}", email.split('@').next().unwrap_or(email))
            }
            _ => account.name.clone(),
        };
        self.stage = Stage::Done(Box::new(Done::new(account, "Signed in", detail, &name)));
        ctx.say(Message::success(
            if self.again {
                "Signed in."
            } else {
                "Account added."
            },
            "",
        ));
        ctx.request(Request::Pools);
    }

    fn sign_in_reply(
        &mut self,
        result: &Result<(SignInTicket, SignInView), String>,
        ctx: &mut Ctx,
    ) {
        let Stage::SignIn(sign_in) = &mut self.stage else {
            return;
        };
        match result {
            Ok((ticket, view)) => {
                sign_in.ticket = Some(ticket.clone());
                sign_in.view = Some(view.clone());
                if sign_in.choice.native == Some(NativeAuthMethod::ClaudeOfficial)
                    && !sign_in.opened
                    && let Some(page) = &view.page
                {
                    sign_in.opened = true;
                    if open_page(page).is_err() {
                        ctx.say(Message::warning(
                            "The browser did not open.",
                            "Press o to open the page.",
                        ));
                    }
                }
                match view.phase {
                    NativeAuthPhase::AuthenticatedObserved => self.done_from_sign_in(ctx),
                    NativeAuthPhase::UnauthenticatedObserved
                    | NativeAuthPhase::Failed
                    | NativeAuthPhase::Cancelled
                    | NativeAuthPhase::Unavailable => {
                        sign_in.problem = Some(sign_in_problem(sign_in.choice.native, view));
                    }
                    NativeAuthPhase::Starting
                    | NativeAuthPhase::CheckingAccount
                    | NativeAuthPhase::Uncertain
                        if sign_in.reads < FAST_READS_MAX =>
                    {
                        sign_in.reads += 1;
                        self.read_sign_in(ctx);
                    }
                    _ => {}
                }
            }
            Err(code) => sign_in.problem = Some(plain_sign_in(code).to_owned()),
        }
    }

    fn add_reply(&mut self, result: &Result<AccountsReply, String>, ctx: &mut Ctx) {
        match (&mut self.stage, result) {
            (Stage::SignIn(sign_in), Ok(AccountsReply::Account(account))) => {
                sign_in.account = Some(account.clone());
                self.start_sign_in(ctx);
            }
            (Stage::SignIn(sign_in), Err(code)) => sign_in.problem = Some(plain(code).to_owned()),
            (Stage::Form(form), Ok(AccountsReply::Account(account))) => {
                let (headline, detail) = if form.choice.method == SignInMethod::ApiKey {
                    (
                        "Key saved",
                        vec!["CrazyAgent will check it the first time an agent uses it. The engine keeps it and never shows it again.".to_owned()],
                    )
                } else if account.status == AccountStatus::Unreachable {
                    ("Can't reach this URL", vec!["The account is saved. Start the server, then check it again from Accounts.".to_owned()])
                } else {
                    ("Connected", Vec::new())
                };
                let name = account.name.clone();
                self.stage = Stage::Done(Box::new(Done::new(
                    account.clone(),
                    headline,
                    detail,
                    &name,
                )));
                ctx.say(Message::success("Account added.", ""));
                ctx.request(Request::Pools);
            }
            (Stage::Form(_), Err(_)) if ctx.link != Link::Ready => {}
            (Stage::Form(_), Err(code)) => ctx.say(Message::error(plain(code), "")),
            _ => {}
        }
    }

    fn reply(&mut self, event: &SettingsEvent, ctx: &mut Ctx) {
        let waiting = self.waiting;
        match (waiting, event) {
            (Waiting::Catalog, SettingsEvent::Accounts(result)) => {
                self.waiting = Waiting::Nothing;
                self.catalog = match result {
                    Ok(AccountsReply::Catalog(entries)) => Load::Ready(entries.clone()),
                    Ok(_) => Load::Failed("catalog_reply".into()),
                    Err(code) => Load::Failed(code.clone()),
                };
            }
            (Waiting::Models, SettingsEvent::Accounts(result)) => {
                self.waiting = Waiting::Nothing;
                if let Stage::Form(form) = &mut self.stage {
                    form.models = match result {
                        Ok(AccountsReply::Models(models)) => Load::Ready(models.clone()),
                        Ok(_) => Load::Failed("models_reply".into()),
                        Err(code) => Load::Failed(code.clone()),
                    };
                    form.model_selected = 0;
                    if let (Load::Failed(_), Some(url)) = (&form.models, form.url.as_mut()) {
                        url.error =
                            Some("Could not reach this address. Is the server running?".into());
                    }
                }
            }
            (Waiting::Add, SettingsEvent::Accounts(result)) => {
                self.waiting = Waiting::Nothing;
                self.add_reply(result, ctx);
            }
            (Waiting::SignIn, SettingsEvent::SignIn(result)) => {
                self.waiting = Waiting::Nothing;
                self.sign_in_reply(result, ctx);
            }
            (Waiting::Finish, SettingsEvent::Accounts(result)) => {
                self.finished(result.as_ref().err(), ctx)
            }
            (Waiting::Finish, SettingsEvent::PoolsJoined(result)) => {
                self.finished(result.as_ref().err(), ctx)
            }
            _ => {}
        }
    }

    fn finished(&mut self, error: Option<&String>, ctx: &mut Ctx) {
        let Stage::Done(done) = &mut self.stage else {
            return;
        };
        done.saving = done.saving.saturating_sub(1);
        if let Some(code) = error {
            ctx.say(Message::error(plain(code), ""));
        }
        if done.saving == 0 {
            self.waiting = Waiting::Nothing;
            if error.is_none() {
                ctx.say(Message::success("Saved.", ""));
            }
            ctx.request(Request::Pools);
        }
    }

    fn provider_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let count = self.entries().len();
        let Stage::Provider { selected } = &mut self.stage else {
            return Flow::Ignored;
        };
        match key {
            Key::Up => *selected = selected.saturating_sub(1),
            Key::Down if count > 0 => *selected = (*selected + 1).min(count - 1),
            Key::Enter if count > 0 => {
                let index = (*selected).min(count - 1);
                let entry = self.entries()[index].clone();
                self.open_provider(entry, ctx);
            }
            Key::Char('r') if matches!(self.catalog, Load::Failed(_)) => {
                self.catalog = Load::Loading;
                self.waiting = Waiting::Catalog;
                send(ctx, SettingsJob::Accounts(AccountsRequest::Catalog));
            }
            Key::Esc => return self.leave(),
            _ => {}
        }
        Flow::Handled
    }

    fn method_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Stage::Method { entry, selected } = &mut self.stage else {
            return Flow::Ignored;
        };
        let options = choices(entry);
        match key {
            Key::Up => *selected = selected.saturating_sub(1),
            Key::Down => *selected = (*selected + 1).min(options.len() - 1),
            Key::Enter => {
                let choice = options[(*selected).min(options.len() - 1)].clone();
                self.choose(choice, ctx);
            }
            Key::Esc => {
                let provider = entry.provider;
                let index = self
                    .entries()
                    .iter()
                    .position(|entry| entry.provider == provider)
                    .unwrap_or(0);
                self.stage = Stage::Provider { selected: index };
            }
            _ => {}
        }
        Flow::Handled
    }

    fn form_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Stage::Form(form) = &mut self.stage else {
            return Flow::Ignored;
        };
        let slots = form.slots();
        let slot = form.slot();
        match key {
            Key::Esc => {
                let provider = form.choice.provider;
                self.back_to_choice(provider);
                return Flow::Handled;
            }
            Key::Tab => {
                if slot == Slot::Url && form.probe(ctx) {
                    self.waiting = Waiting::Models;
                }
                form.focus = (form.focus + 1) % slots.len();
            }
            Key::BackTab => form.focus = (form.focus + slots.len() - 1) % slots.len(),
            Key::Up | Key::Down if slot == Slot::Model && form.listing().is_some() => {
                let count = form.listing().map_or(0, Vec::len);
                form.model_selected = if *key == Key::Up {
                    form.model_selected.saturating_sub(1)
                } else {
                    (form.model_selected + 1).min(count.saturating_sub(1))
                };
            }
            Key::Enter if slot == Slot::Button || form.focus + 2 == slots.len() => {
                self.submit_form(ctx)
            }
            Key::Enter => {
                if slot == Slot::Url && form.probe(ctx) {
                    self.waiting = Waiting::Models;
                }
                form.focus += 1;
            }
            other => {
                let field = match slot {
                    Slot::Name => Some(&mut form.name),
                    Slot::Url => form.url.as_mut(),
                    Slot::Key => form.key.as_mut(),
                    Slot::Model if form.models.ready().is_none_or(Vec::is_empty) => {
                        form.model.as_mut()
                    }
                    _ => None,
                };
                if let Some(field) = field
                    && field.key(other)
                {
                    field.error = None;
                    if slot == Slot::Url && !matches!(form.models, Load::Idle) {
                        form.models = Load::Idle;
                    }
                }
            }
        }
        Flow::Handled
    }

    fn sign_in_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Stage::SignIn(sign_in) = &mut self.stage else {
            return Flow::Ignored;
        };
        let claude = sign_in.choice.native == Some(NativeAuthMethod::ClaudeOfficial);
        let accepts = sign_in.view.as_ref().and_then(|view| view.claude_prompt);
        match key {
            Key::Esc => {
                if let Some(ticket) = sign_in.ticket.clone()
                    && sign_in.moving()
                {
                    send(ctx, SettingsJob::SignIn(SignInJob::Cancel { ticket }));
                }
                if sign_in.created
                    && let Some(account) = &sign_in.account
                {
                    send(
                        ctx,
                        SettingsJob::Accounts(AccountsRequest::Remove {
                            id: account.id.clone(),
                        }),
                    );
                }
                self.waiting = Waiting::Nothing;
                if self.again {
                    return self.leave();
                }
                let provider = sign_in.choice.provider;
                self.back_to_choice(provider);
                ctx.say(Message::warning("Sign-in cancelled.", ""));
            }
            Key::Char('n') if sign_in.problem.is_some() && sign_in.code.is_empty() => {
                if sign_in.account.is_none() {
                    let choice = sign_in.choice.clone();
                    self.choose(choice, ctx);
                } else {
                    self.start_sign_in(ctx);
                }
            }
            Key::Char('o') if sign_in.code.is_empty() => {
                if let Some(page) = sign_in.page()
                    && open_page(page).is_err()
                {
                    ctx.say(Message::error(
                        "Could not open the page.",
                        "Copy it from the screen.",
                    ));
                }
            }
            Key::Char('c') if !claude => {
                if let Some(code) = sign_in
                    .view
                    .as_ref()
                    .and_then(|view| view.device_code.clone())
                {
                    match copy(&code) {
                        Ok(()) => ctx.say(Message::success("Code copied.", "")),
                        Err(_) => ctx.say(Message::error(
                            "Could not copy the code.",
                            "Type it from the screen.",
                        )),
                    }
                }
            }
            Key::Enter
                if claude
                    && accepts.is_some()
                    && !sign_in.code.is_empty()
                    && self.waiting == Waiting::Nothing =>
            {
                let Some(ticket) = sign_in.ticket.clone() else {
                    return Flow::Handled;
                };
                let generation = sign_in.view.as_ref().map_or(0, |view| view.generation);
                match AccountSecret::new(sign_in.code.take_secret()) {
                    Ok(code) => {
                        self.waiting = Waiting::SignIn;
                        send(
                            ctx,
                            SettingsJob::SignIn(SignInJob::ClaudeCode {
                                ticket,
                                generation,
                                prompt: accepts.expect("checked above"),
                                code,
                            }),
                        );
                    }
                    Err(_) => {
                        sign_in.code.error =
                            Some("This code cannot be used. Paste the whole code.".into())
                    }
                }
            }
            other if claude && accepts.is_some() => {
                sign_in.code.error = None;
                sign_in.code.key(other);
            }
            _ => {}
        }
        Flow::Handled
    }

    fn done_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let pools = ctx.store.pools.ready().cloned().unwrap_or_default();
        let Stage::Done(done) = &mut self.stage else {
            return Flow::Ignored;
        };
        if done.saving > 0 {
            return Flow::Handled;
        }
        if done.focus == DoneFocus::Name {
            match key {
                Key::Tab | Key::Enter => done.focus = DoneFocus::Pools,
                Key::BackTab => done.focus = DoneFocus::Button,
                Key::Esc => done.focus = DoneFocus::Pools,
                other => {
                    done.name.key(other);
                }
            }
            return Flow::Handled;
        }
        match key {
            Key::Tab => {
                done.focus = match done.focus {
                    DoneFocus::Pools => DoneFocus::Button,
                    _ => DoneFocus::Name,
                }
            }
            Key::BackTab => {
                done.focus = match done.focus {
                    DoneFocus::Button => DoneFocus::Pools,
                    _ => DoneFocus::Name,
                }
            }
            Key::Up if done.focus == DoneFocus::Pools => {
                done.selected = done.selected.saturating_sub(1)
            }
            Key::Down if done.focus == DoneFocus::Pools => {
                done.selected = (done.selected + 1).min(pools.len().saturating_sub(1));
            }
            Key::Char(' ') if done.focus == DoneFocus::Pools => {
                if let Some(pool) = pools.get(done.selected)
                    && !done.picked.remove(&pool.id)
                {
                    if pool.members.len() >= POOL_MEMBERS_MAX {
                        ctx.say(Message::warning(
                            "A pool holds at most 15 accounts.",
                            "Unselect one in the pool editor to add another.",
                        ));
                    } else {
                        done.picked.insert(pool.id.clone());
                    }
                }
            }
            Key::Char('a') => {
                self.stage = Stage::Provider { selected: 0 };
                self.again = false;
            }
            Key::Enter => return self.finish(ctx),
            Key::Esc => return self.leave(),
            _ => {}
        }
        Flow::Handled
    }

    /// The body lines of the current step.
    /// Inputs: content width and context. Output: lines and the field cursor.
    pub fn body(&self, limit: usize, ctx: &Ctx) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        match &self.stage {
            Stage::Provider { selected } => {
                let mut lines = vec![
                    styled("Add account", Role::Accent, theme),
                    Line::raw("Where does the model come from?"),
                    Line::default(),
                ];
                match &self.catalog {
                    Load::Ready(entries) => {
                        for (index, entry) in entries.iter().enumerate() {
                            lines.push(list_row(
                                cells(
                                    &[24, limit.saturating_sub(28)],
                                    vec![
                                        Line::raw(provider_cell(entry.provider, ctx.glyphs)),
                                        styled(provider_line(entry.provider), Role::Muted, theme),
                                    ],
                                    2,
                                    ctx.glyphs,
                                ),
                                limit,
                                index == *selected,
                                true,
                                theme,
                                ctx.glyphs,
                            ));
                        }
                        lines.push(Line::default());
                        lines.push(Line::raw(
                            "Add as many accounts as you like. One pool can mix providers.",
                        ));
                    }
                    Load::Failed(code) => {
                        lines.push(status(
                            &format!("Failed to load providers. {}", plain(code)),
                            theme,
                            ctx.glyphs,
                        ));
                        lines.push(Line::raw("Press r to try again."));
                    }
                    _ => lines.push(Line::from(vec![
                        Span::styled(ctx.spinner(), theme.running),
                        Span::raw(" Loading providers"),
                    ])),
                }
                (lines, None)
            }
            Stage::Method { entry, selected } => {
                let mut lines = vec![
                    styled(provider_word(entry.provider), Role::Accent, theme),
                    Line::raw("How do you want to connect?"),
                    Line::default(),
                ];
                for (index, choice) in choices(entry).iter().enumerate() {
                    lines.extend(menu_item(
                        choice.title,
                        choice.description,
                        limit,
                        index == *selected,
                        theme,
                        ctx.glyphs,
                    ));
                }
                (lines, None)
            }
            Stage::Form(form) => form.lines(limit, ctx),
            Stage::SignIn(sign_in) => sign_in.lines(limit, ctx),
            Stage::Done(done) => done.lines(limit, ctx),
        }
    }
}

fn plain_sign_in(code: &str) -> &'static str {
    match code {
        "native_auth_claude_input_outcome_unknown" => {
            "The code may not have reached Claude. Wait a moment, then try again."
        }
        "native_auth_method_mismatch" => "This account cannot use that sign-in method.",
        "native_auth_connection_missing" | "account_not_found" => "This account no longer exists.",
        other => crate::tui::native_auth::public_problem(other),
    }
}

fn sign_in_problem(native: Option<NativeAuthMethod>, view: &SignInView) -> String {
    match (view.phase, native) {
        (NativeAuthPhase::UnauthenticatedObserved, Some(NativeAuthMethod::CodexExisting)) => {
            "Codex is not signed in on this Mac. Choose Sign in with ChatGPT instead.".into()
        }
        (NativeAuthPhase::Cancelled, _) => "The sign-in was cancelled.".into(),
        (NativeAuthPhase::Unavailable, _) => "This sign-in is not available on this Mac.".into(),
        (_, Some(NativeAuthMethod::CodexDeviceCode)) if view.device_code.is_some() => {
            "The code expired or was not approved.".into()
        }
        _ => match view.problem.as_deref() {
            Some(code) => plain_sign_in(code).to_owned(),
            None => "The sign-in did not finish.".into(),
        },
    }
}

impl Screen for AddAccount {
    fn crumbs(&self) -> Vec<String> {
        let mut crumbs = vec![
            "Settings".to_owned(),
            "Accounts".to_owned(),
            "Add account".to_owned(),
        ];
        match &self.stage {
            Stage::Provider { .. } => {}
            Stage::Method { entry, .. } => crumbs.push(provider_word(entry.provider).into()),
            Stage::Form(form) => crumbs.push(provider_word(form.choice.provider).into()),
            Stage::SignIn(sign_in) => {
                crumbs.push(provider_word(sign_in.choice.provider).into());
                crumbs.push(sign_in.choice.title.into());
            }
            Stage::Done(_) => crumbs.push("Done".into()),
        }
        crumbs
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let vertical = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        match &self.stage {
            Stage::Provider { .. } | Stage::Method { .. } => {
                vec![
                    hint(&vertical, "Move"),
                    hint("Enter", "Choose"),
                    hint("Esc", "Back"),
                ]
            }
            Stage::Form(form) if form.unreachable() => {
                vec![
                    hint("Enter", "Check again"),
                    hint("Tab", "Next field"),
                    hint("Esc", "Back"),
                ]
            }
            Stage::Form(form) if form.choice.method == SignInMethod::Url => {
                let mut hints = Vec::new();
                if form.listing().is_some() {
                    hints.push(hint(&vertical, "Choose model"));
                }
                hints.extend([
                    hint("Tab", "Next"),
                    hint("Enter", "Save account"),
                    hint("Esc", "Back"),
                ]);
                hints
            }
            Stage::Form(_) => vec![
                hint("Enter", "Save key"),
                hint("Ctrl-U", "Clear"),
                hint("Tab", "Next field"),
                hint("Esc", "Back"),
            ],
            Stage::SignIn(sign_in) if sign_in.problem.is_some() => {
                let label = if sign_in.choice.native == Some(NativeAuthMethod::CodexDeviceCode) {
                    "New code"
                } else {
                    "Try again"
                };
                vec![hint("n", label), hint("Esc", "Cancel sign-in")]
            }
            Stage::SignIn(sign_in)
                if sign_in.choice.native == Some(NativeAuthMethod::ClaudeOfficial) =>
            {
                let mut hints = Vec::new();
                if sign_in
                    .view
                    .as_ref()
                    .is_some_and(|view| view.claude_prompt.is_some())
                {
                    hints.push(hint("Enter", "Submit code"));
                }
                hints.push(hint("o", "Open page"));
                hints.push(hint("Ctrl-U", "Clear"));
                hints.push(hint("Esc", "Cancel sign-in"));
                hints
            }
            Stage::SignIn(sign_in)
                if sign_in.choice.native == Some(NativeAuthMethod::CodexExisting) =>
            {
                vec![hint("Esc", "Cancel sign-in")]
            }
            Stage::SignIn(_) => vec![
                hint("o", "Open page"),
                hint("c", "Copy code"),
                hint("Esc", "Cancel sign-in"),
            ],
            Stage::Done(_) => vec![
                hint("Space", "Select pool"),
                hint("Enter", "Done"),
                hint("a", "Add another"),
                hint("Esc", "Accounts"),
            ],
        }
    }

    fn typing(&self) -> bool {
        match &self.stage {
            Stage::Form(form) => form.typing(),
            Stage::SignIn(sign_in) => !sign_in.code.is_empty(),
            Stage::Done(done) => done.focus == DoneFocus::Name,
            _ => false,
        }
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        match self.stage {
            Stage::Provider { .. } => self.provider_key(key, ctx),
            Stage::Method { .. } => self.method_key(key, ctx),
            Stage::Form(_) => self.form_key(key, ctx),
            Stage::SignIn(_) => self.sign_in_key(key, ctx),
            Stage::Done(_) => self.done_key(key, ctx),
        }
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        if let Some(event) = settings_event(arrival) {
            self.reply(event, ctx);
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if matches!(self.catalog, Load::Idle) && self.waiting == Waiting::Nothing {
            self.catalog = Load::Loading;
            self.waiting = Waiting::Catalog;
            send(ctx, SettingsJob::Accounts(AccountsRequest::Catalog));
        }
        ctx.request(Request::Pools);
        if let Stage::SignIn(sign_in) = &self.stage {
            if sign_in.ticket.is_none()
                && !sign_in.created
                && sign_in.problem.is_none()
                && self.waiting == Waiting::Nothing
            {
                self.start_sign_in(ctx);
            } else {
                self.read_sign_in(ctx);
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = (area.width as usize).saturating_sub(4).max(8);
        let (content, cursor) = self.body(limit, ctx);
        draw_page(frame, area, SettingsSection::Accounts, content, cursor, ctx)
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        match &self.stage {
            Stage::SignIn(sign_in) => sign_in.moving(),
            _ => self.waiting != Waiting::Nothing,
        }
    }

    fn draft(&self) -> bool {
        match &self.stage {
            Stage::Form(form) => form.key.as_ref().is_some_and(|key| !key.is_empty()),
            Stage::SignIn(sign_in) => sign_in.moving(),
            _ => false,
        }
    }
}
