use crate::app_plugins::{Binding, Origin, PluginStatus, Preview, Problem, TrustView};
use crate::connectors::callback::CALLBACK_WAIT;
use crate::connectors::sign_in::SignInState;
use crate::connectors::{
    ConnectorSettings, ConnectorView, ConnectorsView, NativeConnectors, SignIn, SignInWith,
    SizeCap, Wait, WritePolicy,
};
use crate::engine::owner::{AccountSecret, ConnectorsSaved};
use crate::tui::client::app_plugins::MANIFEST_BYTES_MAX;
use crate::tui::client::connectors::{ConnectorsEvent, ConnectorsJob, SignInStarted};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, Modal, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::run::REFRESH;
use crate::tui::screens::settings::app_plugins::safe;
use crate::tui::screens::settings::connector_activity::turn;
use crate::tui::screens::settings_accounts::{send, settings_event};
use crate::tui::screens::{Flow, Place, Screen};
use crate::tui::seam::Arrival;
use crate::tui::widgets::TextField;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use std::path::PathBuf;
use words::{
    ADD_FIELD, BACK, BROWSER_NO_KEY, CHECK, CHECKING, KEY_EMPTY, KEY_FIELD, NOT_CONNECTOR,
    NOT_OPENED, NOT_READ_SIGN_IN, OAUTH_NOT_YET, SAVE, SAVED, SIGN_IN, SIGN_IN_CANCELLED, SIGN_OUT,
    STALE, browser_now, chosen_way, hosts, keys_shown, other_way, refusal, secrets_shown,
    sign_in_first, signed_in, signed_in_line, signed_out_line, signs_in_with, way_word,
};

mod draw;
#[cfg(all(test, unix))]
mod tests;
pub(in crate::tui) mod words;

/// The page's title, the word the desktop's Settings list and page use too.
pub(in crate::tui) const TITLE: &str = "Connectors";
const GUTTER: u16 = 2;
const KEY_CHARS_MAX: usize = 1024;
const PATH_CHARS_MAX: usize = 1024;
const SETTING_ROWS: usize = 4;
const SIGN_IN_READS_MAX: usize = 72;
const _: () = assert!(SIGN_IN_READS_MAX as u64 * REFRESH.as_secs() > CALLBACK_WAIT.as_secs());
const _: () = assert!(SIGN_IN_READS_MAX as u64 * REFRESH.as_secs() <= 2 * CALLBACK_WAIT.as_secs());
const NOT_READ: &str = "Couldn't load the connectors.";
const NOT_SAVED: &str = "The connector settings were not saved.";
const ABSOLUTE: &str = "Type the full path of a .yaml file, starting with /.";

/// Which catalog line the cursor rests on: a connector or one of the four settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Connector(usize),
    Write,
    Wait,
    Native,
    Size,
}

/// The one request in flight: a read, a manifest check or review, a connector change, a
/// settings save, or the start of a sign-in through the browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    Read,
    Check,
    Change,
    Settings,
    SignIn,
}

/// A sign-in through the browser this page started and still reads: the connector and its
/// title, the address shown for copying, whether a cancel was sent, how many state reads
/// or cancels are on their way, and how many reads were sent.
struct Signing {
    name: String,
    title: String,
    url: String,
    cancelled: bool,
    asked: usize,
    reads: usize,
}

/// The paste box and the typed path, with the one that has focus.
struct AddForm {
    yaml: TextField,
    path: TextField,
    on_path: bool,
}

/// The key dialog for one secret of one connector: who it is sent to and the masked field.
struct KeyForm {
    name: String,
    title: String,
    hosts: Vec<String>,
    secret: String,
    key: TextField,
}

/// Whether a listed connector is a refused file, which has no page, no hosts and nothing
/// to review, key or remove here (WORDS r7). Input: the connector. Output: the answer.
fn refused(connector: &ConnectorView) -> bool {
    matches!(connector.status, PluginStatus::Refused { .. })
}

enum Open {
    None,
    Add(Box<AddForm>),
    Trust {
        yaml: Option<String>,
        trust: Box<TrustView>,
    },
    Refused {
        yaml: Option<String>,
        problem: Problem,
    },
    Remove {
        name: String,
        title: String,
    },
    Key(Box<KeyForm>),
}

/// Settings, Connectors (DEC-168 addenda 18, 27 and 32): the catalog of every connector with
/// its state, the harnesses that reach connectors and the four connector settings beside the
/// locked line. Enter opens a connector's page with its address and secrets, where `w`
/// switches one that offers both between Key and Browser, `k` sets a key and `u` clears it,
/// `s` signs in through the browser or out and `c` cancels a waiting sign-in, which each
/// refresh reads; Enter reviews a changed file and `x` removes the owner's own; `a` adds a
/// manifest only after its trust screen, read to its end, is confirmed with `y`. A settings
/// pick waits for Enter and saves with the version read. One request at a time.
pub(in crate::tui) struct ConnectorsPage {
    stored: Load<ConnectorsView>,
    write: WritePolicy,
    wait: Wait,
    native: NativeConnectors,
    size_cap: SizeCap,
    selected: usize,
    secret: usize,
    top: usize,
    detail: Option<String>,
    open: Open,
    trust_top: usize,
    trust_end: bool,
    pending: Option<Pending>,
    failure: Option<String>,
    sent: Option<String>,
    done: Option<String>,
    signing: Option<Signing>,
}

impl ConnectorsPage {
    /// Opens the page with nothing read yet. Input: none. Output: the page; the read goes
    /// out when it is shown.
    pub fn new() -> Self {
        Self {
            stored: Load::Idle,
            write: ConnectorSettings::DEFAULT.write,
            wait: ConnectorSettings::DEFAULT.wait,
            native: ConnectorSettings::DEFAULT.native,
            size_cap: ConnectorSettings::DEFAULT.size_cap,
            selected: 0,
            secret: 0,
            top: 0,
            detail: None,
            open: Open::None,
            trust_top: 0,
            trust_end: false,
            pending: None,
            failure: None,
            sent: None,
            done: None,
            signing: None,
        }
    }

    fn ask(&mut self, job: ConnectorsJob, pending: Pending, ctx: &mut Ctx) {
        assert!(self.pending.is_none(), "one request at a time");
        assert_eq!(
            pending == Pending::Read,
            matches!(job, ConnectorsJob::View { project: None }),
            "only a read asks for the page"
        );
        self.pending = Some(pending);
        self.failure = None;
        send(ctx, SettingsJob::Connectors(Box::new(job)));
    }

    fn read(&mut self, ctx: &mut Ctx) {
        if self.pending.is_some() {
            return;
        }
        if self.stored.ready().is_none() {
            self.stored = Load::Loading;
        }
        self.ask(ConnectorsJob::View { project: None }, Pending::Read, ctx);
    }

    fn connectors(&self) -> &[ConnectorView] {
        self.stored
            .ready()
            .map_or(&[], |view| view.connectors.as_slice())
    }

    fn opened(&self) -> Option<&ConnectorView> {
        let name = self.detail.as_ref()?;
        self.connectors()
            .iter()
            .find(|connector| connector.name == *name)
    }

    fn secrets_listed(&self) -> usize {
        self.opened()
            .map_or(0, |connector| secrets_shown(connector).len())
    }

    fn edited(&self) -> bool {
        self.stored.ready().is_some_and(|view| {
            view.settings.write != self.write
                || view.settings.wait != self.wait
                || view.settings.native != self.native
                || view.settings.size_cap != self.size_cap
        })
    }

    fn rows(&self) -> Vec<Row> {
        let count = self.connectors().len();
        let mut rows: Vec<Row> = (0..count).map(Row::Connector).collect();
        rows.extend([Row::Write, Row::Wait, Row::Native, Row::Size]);
        assert_eq!(rows.len(), count + SETTING_ROWS);
        rows
    }

    fn moved(&mut self, to: usize) {
        let last = self.rows().len() - 1;
        self.selected = to.min(last);
        assert!(self.selected <= last);
    }

    fn discard(&mut self) {
        if let Some(view) = self.stored.ready() {
            self.write = view.settings.write;
            self.wait = view.settings.wait;
            self.native = view.settings.native;
            self.size_cap = view.settings.size_cap;
        }
        assert!(!self.edited());
    }

    fn change(&mut self, forward: bool) {
        match self.rows()[self.selected] {
            Row::Write => {
                let options = words::WRITE_CHOICES.map(|(choice, _)| choice);
                self.write = turn(&options, &self.write, forward);
            }
            Row::Wait => {
                let options = words::WAIT_CHOICES.map(|(choice, _)| choice);
                self.wait = turn(&options, &self.wait, forward);
            }
            Row::Native => {
                let options = words::NATIVE_CHOICES.map(|(choice, _)| choice);
                self.native = turn(&options, &self.native, forward);
            }
            Row::Size => {
                let options = words::SIZE_CHOICES.map(|(choice, _)| choice);
                self.size_cap = turn(&options, &self.size_cap, forward);
            }
            Row::Connector(_) => {}
        }
    }

    fn enter(&mut self, ctx: &mut Ctx) {
        let Some(view) = self.stored.ready() else {
            return;
        };
        if self.edited() {
            let job = ConnectorsJob::SaveSettings {
                settings: ConnectorSettings {
                    write: self.write,
                    wait: self.wait,
                    native: self.native,
                    size_cap: self.size_cap,
                    ..view.settings
                },
                version: view.version,
            };
            self.ask(job, Pending::Settings, ctx);
            return;
        }
        if let Row::Connector(at) = self.rows()[self.selected]
            && !refused(&self.connectors()[at])
        {
            self.detail = Some(self.connectors()[at].name.clone());
            self.secret = 0;
            self.top = 0;
        }
    }

    fn review(&mut self, ctx: &mut Ctx) {
        let Some(connector) = self.opened() else {
            return;
        };
        if connector.status != PluginStatus::NeedsReview {
            return;
        }
        let name = connector.name.clone();
        self.ask(ConnectorsJob::Review { name }, Pending::Check, ctx);
    }

    fn remove(&mut self, ctx: &mut Ctx) {
        let Some(connector) = self.opened() else {
            return;
        };
        if refused(connector) {
            return;
        }
        if connector.origin == Origin::Bundled {
            ctx.say(Message::error(
                "Not removed.",
                &refusal("app_plugin_bundled"),
            ));
            return;
        }
        self.open = Open::Remove {
            name: connector.name.clone(),
            title: safe(&connector.title),
        };
    }

    fn key_form(&mut self, ctx: &mut Ctx) {
        let Some(connector) = self.opened() else {
            return;
        };
        if refused(connector) {
            return;
        }
        if connector.sign_in == SignIn::NotYet {
            ctx.say(Message::warning("No key here.", OAUTH_NOT_YET));
            return;
        }
        if chosen_way(connector) == Some(SignInWith::Browser) {
            ctx.say(Message::warning("No key here.", BROWSER_NO_KEY));
            return;
        }
        let Some(secret) = connector.secrets.get(self.secret) else {
            ctx.say(Message::warning(
                "No key needed.",
                "This connector declares no secrets.",
            ));
            return;
        };
        self.open = Open::Key(Box::new(KeyForm {
            name: connector.name.clone(),
            title: safe(&connector.title),
            hosts: connector.runs.as_ref().map(hosts).unwrap_or_default(),
            secret: secret.name.clone(),
            key: TextField::secret(KEY_FIELD, KEY_CHARS_MAX),
        }));
    }

    fn clear_key(&mut self, ctx: &mut Ctx) {
        let Some(connector) = self.opened() else {
            return;
        };
        let Some(secret) = secrets_shown(connector).get(self.secret).copied() else {
            return;
        };
        if secret.binding == Binding::None {
            ctx.say(Message::warning(
                "Nothing to clear.",
                "This secret has no key.",
            ));
            return;
        }
        let job = ConnectorsJob::ClearKey {
            name: connector.name.clone(),
            secret: secret.name.clone(),
        };
        self.done = Some(format!("{} no longer has a key.", safe(&secret.name)));
        self.ask(job, Pending::Change, ctx);
    }

    fn switch_way(&mut self, ctx: &mut Ctx) {
        let Some(connector) = self.opened() else {
            return;
        };
        let Some(now) = chosen_way(connector) else {
            return;
        };
        let way = other_way(now);
        let name = connector.name.clone();
        let done = signs_in_with(&connector.title, way);
        assert_ne!(way, now, "w always switches");
        self.done = Some(done);
        let job = ConnectorsJob::SetSignInWith { name, way };
        self.ask(job, Pending::Change, ctx);
    }

    fn waiting_on(&self, name: &str) -> bool {
        self.signing
            .as_ref()
            .is_some_and(|signing| signing.name == name)
    }

    fn sign_in_or_out(&mut self, ctx: &mut Ctx) {
        let Some(connector) = self.opened() else {
            return;
        };
        if refused(connector) {
            return;
        }
        let name = connector.name.clone();
        if signed_in(connector) {
            self.done = Some(signed_out_line(&connector.title));
            self.ask(ConnectorsJob::SignOut { name }, Pending::Change, ctx);
            return;
        }
        if !browser_now(connector) || self.waiting_on(&name) {
            return;
        }
        if let Some(other) = &self.signing {
            let first = sign_in_first(&other.title);
            ctx.say(Message::warning("Not yet.", &first));
            return;
        }
        self.ask(ConnectorsJob::SignIn { name }, Pending::SignIn, ctx);
    }

    fn cancel_sign_in(&mut self, ctx: &mut Ctx) {
        let Some(name) = self.detail.clone() else {
            return;
        };
        let open = |signing: &&mut Signing| signing.name == name && !signing.cancelled;
        let Some(signing) = self.signing.as_mut().filter(open) else {
            return;
        };
        signing.cancelled = true;
        signing.asked += 1;
        assert!(signing.asked <= 2, "one read and one cancel at most");
        let job = ConnectorsJob::CancelSignIn { name };
        send(ctx, SettingsJob::Connectors(Box::new(job)));
    }

    /// Reads where the page's sign-in stands when no read or cancel is on its way, at most
    /// `SIGN_IN_READS_MAX` times, after which the page reads itself again instead. Input:
    /// the context. Output: none; the read goes out on the context.
    fn read_sign_in(&mut self, ctx: &mut Ctx) {
        let Some(signing) = self.signing.as_mut() else {
            return;
        };
        if signing.asked > 0 {
            return;
        }
        if signing.reads >= SIGN_IN_READS_MAX {
            self.signing = None;
            self.read(ctx);
            return;
        }
        signing.reads += 1;
        signing.asked += 1;
        assert!(signing.reads <= SIGN_IN_READS_MAX);
        let job = ConnectorsJob::SignInState {
            name: signing.name.clone(),
        };
        send(ctx, SettingsJob::Connectors(Box::new(job)));
    }

    fn save_key(&mut self, ctx: &mut Ctx) {
        let Open::Key(form) = &mut self.open else {
            return;
        };
        if form.key.is_empty() {
            self.failure = Some(KEY_EMPTY.into());
            return;
        }
        let (name, secret) = (form.name.clone(), form.secret.clone());
        let key = AccountSecret::new(form.key.take_secret());
        assert!(form.key.is_empty(), "the typed key leaves the field once");
        let Ok(key) = key else {
            self.failure = Some(refusal("app_plugin_key_invalid"));
            return;
        };
        self.done = Some(format!("The key for {} was saved.", safe(&secret)));
        self.ask(
            ConnectorsJob::SetKey { name, secret, key },
            Pending::Change,
            ctx,
        );
    }

    fn check(&mut self, ctx: &mut Ctx) {
        let Open::Add(form) = &self.open else {
            return;
        };
        if form.on_path {
            let typed = form.path.value().trim().to_owned();
            let path = PathBuf::from(&typed);
            if typed.is_empty() || !path.is_absolute() {
                self.failure = Some(ABSOLUTE.into());
                return;
            }
            self.ask(ConnectorsJob::ReadFile { path }, Pending::Check, ctx);
            return;
        }
        let yaml = form.yaml.value().to_owned();
        if yaml.trim().is_empty() {
            self.failure = Some("Paste a manifest first.".into());
            return;
        }
        if yaml.len() > MANIFEST_BYTES_MAX {
            self.failure = Some(refusal("connector_request_bounds"));
            return;
        }
        self.sent = Some(yaml.clone());
        self.ask(ConnectorsJob::Preview { yaml }, Pending::Check, ctx);
    }

    fn confirm(&mut self, ctx: &mut Ctx) {
        let (job, done) = match &self.open {
            Open::Trust {
                yaml: Some(yaml),
                trust,
            } => (
                ConnectorsJob::Add {
                    yaml: yaml.clone(),
                    sha256: trust.sha256.clone(),
                    replace: trust.replaces,
                },
                format!("{} was added.", safe(&trust.title)),
            ),
            Open::Trust { yaml: None, trust } => (
                ConnectorsJob::Trust {
                    name: trust.name.clone(),
                    sha256: trust.sha256.clone(),
                },
                format!("{} is trusted.", safe(&trust.title)),
            ),
            Open::Remove { name, title } => (
                ConnectorsJob::Remove { name: name.clone() },
                format!("{title} was removed."),
            ),
            _ => return,
        };
        self.done = Some(done);
        self.ask(job, Pending::Change, ctx);
    }
}

impl ConnectorsPage {
    fn take(&mut self, event: &ConnectorsEvent, ctx: &mut Ctx) {
        match event {
            ConnectorsEvent::Page(result) => {
                let pending = match self.pending {
                    Some(pending @ (Pending::Read | Pending::Change)) => pending,
                    _ => return,
                };
                self.pending = None;
                self.page(result, pending, ctx);
            }
            ConnectorsEvent::Preview { read, result } => {
                if self.pending != Some(Pending::Check) {
                    return;
                }
                self.pending = None;
                self.preview(read.as_ref(), result, ctx);
            }
            ConnectorsEvent::Saved(result) => {
                if self.pending != Some(Pending::Settings) {
                    return;
                }
                self.pending = None;
                self.saved(result, ctx);
            }
            ConnectorsEvent::SignIn { name, result } => {
                if self.pending != Some(Pending::SignIn) {
                    return;
                }
                self.pending = None;
                self.started(name, result, ctx);
            }
            ConnectorsEvent::SignInState { name, result } => self.stands(name, result, ctx),
            ConnectorsEvent::Activity(_)
            | ConnectorsEvent::Approval { .. }
            | ConnectorsEvent::Writes { .. } => {}
        }
    }

    fn started(&mut self, name: &str, result: &Result<SignInStarted, String>, ctx: &mut Ctx) {
        assert!(self.pending.is_none());
        let started = match result {
            Ok(started) => started,
            Err(code) => {
                ctx.say(Message::error("Not signed in.", &refusal(code)));
                return;
            }
        };
        let title = self
            .connectors()
            .iter()
            .find(|connector| connector.name == name)
            .map_or_else(|| name.to_owned(), |connector| connector.title.clone());
        self.signing = Some(Signing {
            name: name.to_owned(),
            title,
            url: safe(&started.url),
            cancelled: false,
            asked: 0,
            reads: 0,
        });
        if !started.opened {
            ctx.say(Message::warning(
                NOT_OPENED,
                "Go to the address on its page.",
            ));
        }
    }

    /// Takes where the page's sign-in stands: waiting keeps it, and every end stops reading,
    /// says why and reads the page again; an answer for another connector changes nothing.
    /// Inputs: the connector, the answer and the context. Output: none.
    fn stands(&mut self, name: &str, result: &Result<SignInState, String>, ctx: &mut Ctx) {
        let Some(signing) = self.signing.as_mut().filter(|signing| signing.name == name) else {
            return;
        };
        signing.asked = signing.asked.saturating_sub(1);
        let title = signing.title.clone();
        match result {
            Ok(SignInState::Waiting) => return,
            Ok(SignInState::SignedIn) => {
                ctx.say(Message::success("Signed in.", &signed_in_line(&title)));
            }
            Ok(SignInState::Failed { code }) => {
                ctx.say(Message::error("Not signed in.", &refusal(code)));
            }
            Ok(SignInState::Cancelled) => {
                ctx.say(Message::warning("Not signed in.", SIGN_IN_CANCELLED));
            }
            Ok(SignInState::Idle) => {}
            Err(code) => ctx.say(Message::error(NOT_READ_SIGN_IN, &refusal(code))),
        }
        self.signing = None;
        self.read(ctx);
    }

    fn store(&mut self, view: &ConnectorsView) {
        if !self.edited() {
            self.write = view.settings.write;
            self.wait = view.settings.wait;
            self.native = view.settings.native;
            self.size_cap = view.settings.size_cap;
        }
        self.stored = Load::Ready(view.clone());
        self.moved(self.selected);
        self.secret = self.secret.min(self.secrets_listed().saturating_sub(1));
        assert!(self.secret < self.secrets_listed().max(1));
        assert!(self.stored.ready().is_some());
    }

    fn page(&mut self, result: &Result<ConnectorsView, String>, pending: Pending, ctx: &mut Ctx) {
        assert!(self.pending.is_none());
        match (result, pending) {
            (Ok(view), Pending::Read) => self.store(view),
            (Ok(view), _) => {
                let was = std::mem::replace(&mut self.open, Open::None);
                if matches!(was, Open::Remove { .. }) {
                    self.detail = None;
                    self.top = 0;
                }
                if let Some(done) = self.done.take() {
                    ctx.say(Message::success("Saved.", &done));
                }
                self.store(view);
            }
            (Err(code), Pending::Read) => match self.stored.ready() {
                Some(_) => ctx.say(Message::error(NOT_READ, &refusal(code))),
                None => self.stored = Load::Failed(code.clone()),
            },
            (Err(code), _) => {
                self.done = None;
                if let Open::Remove { .. } | Open::Trust { .. } = self.open {
                    self.open = Open::None;
                }
                if matches!(self.open, Open::None) {
                    ctx.say(Message::error("Not saved.", &refusal(code)));
                } else {
                    self.failure = Some(refusal(code));
                }
                if code == "app_plugin_not_found" {
                    self.read(ctx);
                }
            }
        }
    }

    fn preview(&mut self, read: Option<&String>, result: &Result<Preview, String>, ctx: &mut Ctx) {
        let yaml = match &self.open {
            Open::Add(_) => read.cloned().or_else(|| self.sent.take()),
            _ => None,
        };
        match result {
            Ok(Preview::Ready { trust }) if trust.connector.is_none() => {
                self.open = Open::Refused {
                    yaml,
                    problem: Problem {
                        code: NOT_CONNECTOR.into(),
                        line: None,
                        field: Some("kind".into()),
                        message: refusal(NOT_CONNECTOR),
                    },
                };
            }
            Ok(Preview::Ready { trust }) => {
                self.open = Open::Trust {
                    yaml,
                    trust: Box::new(trust.clone()),
                };
                self.trust_top = 0;
                self.trust_end = false;
            }
            Ok(Preview::Refused { problem }) => {
                self.open = Open::Refused {
                    yaml,
                    problem: problem.clone(),
                };
            }
            Err(code) if matches!(self.open, Open::Add(_)) => {
                self.failure = Some(refusal(code));
            }
            Err(code) => ctx.say(Message::error("Not reviewed.", &refusal(code))),
        }
    }

    fn saved(&mut self, result: &Result<ConnectorsSaved, String>, ctx: &mut Ctx) {
        assert!(self.pending.is_none());
        match result {
            Ok(ConnectorsSaved::Saved(view)) => {
                ctx.say(Message::success("Saved.", SAVED));
                self.store(view);
                self.discard();
            }
            Ok(ConnectorsSaved::Stale(_)) => {
                self.discard();
                ctx.say(Message::warning("Not saved.", STALE));
                self.read(ctx);
            }
            Err(code) => ctx.say(Message::error(NOT_SAVED, &refusal(code))),
        }
    }
}

impl ConnectorsPage {
    fn add_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Open::Add(form) = &mut self.open else {
            return Flow::Ignored;
        };
        match key {
            Key::Esc if self.pending.is_none() => {
                self.open = Open::None;
                self.failure = None;
            }
            Key::Esc => {}
            Key::Tab | Key::BackTab => form.on_path = !form.on_path,
            Key::Enter if self.pending.is_none() => self.check(ctx),
            other => {
                let field = if form.on_path {
                    &mut form.path
                } else {
                    &mut form.yaml
                };
                field.key(other);
            }
        }
        Flow::Handled
    }

    fn key_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Open::Key(form) = &mut self.open else {
            return Flow::Ignored;
        };
        match key {
            Key::Esc if self.pending.is_none() => {
                self.open = Open::None;
                self.failure = None;
            }
            Key::Esc => {}
            Key::Enter if self.pending.is_none() => self.save_key(ctx),
            Key::Enter => {}
            other => {
                form.key.key(other);
            }
        }
        Flow::Handled
    }

    /// Moves the trust screen's first line for a scroll key and notes once its end was on
    /// screen (rv-ds241 T5 F1). Inputs: the key, the trust screen's line count and the
    /// context. Output: none.
    fn scroll(&mut self, key: &Key, total: usize, ctx: &Ctx) {
        let rows = draw::dialog_rows(ctx);
        let page = rows.saturating_sub(1).max(1);
        let top = self.trust_top;
        let asked = match key {
            Key::Up => top.saturating_sub(1),
            Key::Down => top + 1,
            Key::PageUp => top.saturating_sub(page),
            Key::PageDown => top + page,
            Key::Home => 0,
            Key::End => total,
            _ => top,
        };
        let (top, _, below) = draw::window(asked, total, rows);
        self.trust_top = top;
        self.trust_end |= below == 0;
        assert!(self.trust_top <= total);
    }

    fn dialog_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let total = match &self.open {
            Open::Trust { trust, .. } => Some(self.trust_body(trust, ctx).len()),
            _ => None,
        };
        if let Some(total) = total {
            self.scroll(key, total, ctx);
        }
        let read = total.is_none() || self.trust_end;
        let confirms = matches!(self.open, Open::Trust { .. } | Open::Remove { .. });
        let edits = match &self.open {
            Open::Refused {
                yaml: Some(yaml), ..
            } => Some(yaml.clone()),
            _ => None,
        };
        match key {
            Key::Char('y') if confirms && read && self.pending.is_none() => self.confirm(ctx),
            Key::Char('y') if confirms && !read => {
                ctx.say(Message::warning("Not yet.", words::READ_TO_THE_END));
            }
            Key::Char('e') => {
                if let Some(yaml) = edits {
                    let mut form = add_form();
                    form.yaml.set(&yaml);
                    self.open = Open::Add(Box::new(form));
                }
            }
            Key::Esc if self.pending.is_none() => {
                self.open = Open::None;
                self.failure = None;
            }
            _ => {}
        }
        Flow::Handled
    }

    fn detail_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let secrets = self.secrets_listed();
        match key {
            Key::Esc => {
                self.detail = None;
                self.failure = None;
                self.top = 0;
            }
            Key::Up => self.secret = self.secret.saturating_sub(1),
            Key::Down => self.secret = (self.secret + 1).min(secrets.saturating_sub(1)),
            Key::Enter => self.review(ctx),
            Key::Char('w') => self.switch_way(ctx),
            Key::Char('s') => self.sign_in_or_out(ctx),
            Key::Char('c') => self.cancel_sign_in(ctx),
            Key::Char('k') => self.key_form(ctx),
            Key::Char('u') => self.clear_key(ctx),
            Key::Char('x') => self.remove(ctx),
            Key::Char('r') => self.read(ctx),
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn list_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let last = self.rows().len() - 1;
        match key {
            Key::Up => self.moved(self.selected.saturating_sub(1)),
            Key::Down => self.moved(self.selected + 1),
            Key::Home => self.moved(0),
            Key::End => self.moved(last),
            Key::Right => self.change(true),
            Key::Left => self.change(false),
            Key::Enter => self.enter(ctx),
            Key::Esc if self.edited() => self.discard(),
            Key::Char('a') if !self.edited() => {
                self.failure = None;
                self.open = Open::Add(Box::new(add_form()));
            }
            Key::Char('r') if !self.edited() => self.read(ctx),
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn list_hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let mut hints = vec![hint(
            &format!("{}{}", ctx.glyphs.up, ctx.glyphs.down),
            "Move",
        )];
        let row = self.rows()[self.selected];
        if matches!(row, Row::Write | Row::Wait | Row::Native | Row::Size) {
            hints.push(hint(
                &format!("{}{}", ctx.glyphs.left, ctx.glyphs.right),
                "Change",
            ));
        }
        if self.edited() {
            hints.push(hint("Enter", SAVE));
            hints.push(hint("Esc", "Discard changes"));
            return hints;
        }
        if let Row::Connector(at) = row
            && !refused(&self.connectors()[at])
        {
            hints.push(hint("Enter", "Open"));
        }
        hints.push(hint("a", words::ADD));
        hints.push(hint("r", "Read again"));
        hints.push(hint("Esc", "Back"));
        hints
    }

    fn detail_hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let mut hints = Vec::new();
        if let Some(connector) = self.opened() {
            let keys = keys_shown(connector);
            let listed = secrets_shown(connector);
            if listed.len() > 1 {
                hints.push(hint(
                    &format!("{}{}", ctx.glyphs.up, ctx.glyphs.down),
                    "Move",
                ));
            }
            if connector.status == PluginStatus::NeedsReview {
                hints.push(hint("Enter", "Review"));
            }
            if let Some(now) = chosen_way(connector) {
                let other = format!("{} {}", words::SIGN_IN_WITH, way_word(other_way(now)));
                hints.push(hint("w", &other));
            }
            let signing = self.signing.as_ref();
            let signing = signing.filter(|signing| signing.name == connector.name);
            if signed_in(connector) {
                hints.push(hint("s", SIGN_OUT));
            } else if let Some(signing) = signing {
                if !signing.cancelled {
                    hints.push(hint("c", "Cancel"));
                }
            } else if browser_now(connector) && !refused(connector) && self.signing.is_none() {
                hints.push(hint("s", SIGN_IN));
            }
            let secret = listed.get(self.secret);
            if keys && secret.is_some() {
                hints.push(hint("k", "Set key"));
            }
            if secret.is_some_and(|secret| secret.binding != Binding::None) {
                hints.push(hint("u", "Clear key"));
            }
            if connector.origin == Origin::User && !refused(connector) {
                hints.push(hint("x", "Remove"));
            }
        }
        hints.push(hint("r", "Read again"));
        hints.push(hint("Esc", BACK));
        hints
    }
}

impl Default for ConnectorsPage {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for ConnectorsPage {
    fn crumbs(&self) -> Vec<String> {
        let mut crumbs = vec!["Settings".to_owned(), TITLE.to_owned()];
        if let Some(name) = &self.detail {
            crumbs.push(
                self.opened()
                    .map_or_else(|| safe(name), |connector| safe(&connector.title)),
            );
        }
        crumbs
    }

    fn status(&self, ctx: &Ctx) -> Line<'static> {
        match self.pending {
            Some(Pending::Settings) => {
                Line::styled(format!("Saving{}", ctx.glyphs.ellipsis), ctx.theme.muted)
            }
            Some(Pending::Check) => Line::styled(CHECKING, ctx.theme.muted),
            Some(Pending::Read | Pending::Change | Pending::SignIn) | None => Line::default(),
        }
    }

    fn wraps_hints(&self) -> bool {
        true
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        match &self.open {
            Open::Add(_) | Open::Key(_) if self.pending.is_some() => return Vec::new(),
            Open::Add(_) => {
                return vec![
                    hint("Enter", CHECK),
                    hint("Ctrl-J", "New line"),
                    hint("Tab", "Paste or path"),
                    hint("Esc", "Cancel"),
                ];
            }
            Open::Key(_) => return vec![hint("Enter", "Save"), hint("Esc", "Cancel")],
            Open::None | Open::Trust { .. } | Open::Refused { .. } | Open::Remove { .. } => {}
        }
        match &self.stored {
            Load::Failed(_) => return vec![hint("r", "Try again"), hint("Esc", "Back")],
            Load::Ready(_) => {}
            Load::Idle | Load::Loading | Load::NotReady => return vec![hint("Esc", "Back")],
        }
        if self.pending.is_some() {
            return Vec::new();
        }
        if self.detail.is_some() {
            return self.detail_hints(ctx);
        }
        self.list_hints(ctx)
    }

    fn typing(&self) -> bool {
        matches!(self.open, Open::Add(_) | Open::Key(_))
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        match &self.open {
            Open::Add(_) => return self.add_key(key, ctx),
            Open::Key(_) => return self.key_key(key, ctx),
            Open::Trust { .. } | Open::Refused { .. } | Open::Remove { .. } => {
                return self.dialog_key(key, ctx);
            }
            Open::None => {}
        }
        if self.stored.ready().is_none() {
            if matches!(self.stored, Load::Failed(_)) && *key == Key::Char('r') {
                self.read(ctx);
                return Flow::Handled;
            }
            return Flow::Ignored;
        }
        let moving = matches!(key, Key::Up | Key::Down | Key::Home | Key::End | Key::Esc);
        if self.pending.is_some() && !moving {
            return Flow::Handled;
        }
        if self.detail.is_some() {
            return self.detail_key(key, ctx);
        }
        self.list_key(key, ctx)
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        if let Some(SettingsEvent::Connectors(event)) = settings_event(arrival) {
            self.take(event, ctx);
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if matches!(self.stored, Load::Idle) {
            self.read(ctx);
        }
        self.read_sign_in(ctx);
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let inner = Rect {
            x: area.x.saturating_add(GUTTER),
            width: area.width.saturating_sub(2 * GUTTER),
            ..area
        };
        let width = usize::from(inner.width).max(8);
        let room = usize::from(inner.height);
        let (lines, focus, cursor) = self.content(width, ctx);
        let lines = match focus {
            Some((first, last)) => {
                self.top = draw::scrolled(self.top, first, last, room, lines.len());
                lines.into_iter().skip(self.top).take(room).collect()
            }
            None => lines,
        };
        draw_lines(frame.buffer_mut(), inner, &lines);
        cursor.map(|at| Position::new(inner.x + at.x, inner.y + at.y))
    }

    fn modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        self.dialog(ctx)
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.pending.is_some()
    }

    fn draft(&self) -> bool {
        match &self.open {
            Open::Add(form) => !form.yaml.is_empty() || !form.path.is_empty(),
            Open::Key(form) => !form.key.is_empty(),
            Open::None | Open::Trust { .. } | Open::Refused { .. } | Open::Remove { .. } => false,
        }
    }

    fn loss(&self) -> &'static str {
        "What you typed will be lost."
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Connectors)
    }
}

/// A fresh Add form: a multi-line paste box one character past the engine's bound, so an
/// over-long paste stays whole and is refused, that draws hidden characters as escapes, and
/// a path field.
fn add_form() -> AddForm {
    AddForm {
        yaml: TextField::new(ADD_FIELD, MANIFEST_BYTES_MAX + 1)
            .multiline()
            .escaped(),
        path: TextField::new("Or the path of a .yaml file", PATH_CHARS_MAX),
        on_path: false,
    }
}
