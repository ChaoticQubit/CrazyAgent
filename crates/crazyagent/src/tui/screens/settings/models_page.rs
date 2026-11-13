use super::draw_page;
use super::harness_page::shown;
use crate::engine::owner::ModelAdded;
use crate::harness_settings::{Access, Source};
use crate::model_choice::rules::{FallbackRule, PickRule, Rule, RuleWritten, Rules};
use crate::model_choice::test_launch::Tested;
use crate::model_choice::{
    ALLOWED_MAX, Candidate, MODEL_BYTES_MAX, Models, NOT_SUPPORTED, Origin, Row, Slot,
    UNQUALIFIED_REASON_BYTES_MAX, Written, model_valid, shipped,
};
use crate::protocol::ProjectId;
use crate::tui::client::model_choice::{ModelChoiceEvent, ModelChoiceQuery, ModelChoiceReply};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::screens::process::Scope;
use crate::tui::screens::settings_accounts::{plain, send, settings_event};
use crate::tui::screens::{Flow, Place, Screen, SettingsSection};
use crate::tui::seam::{Arrival, Request};
use crate::tui::text::{
    cells, cut, indent, join, list_row, restyle, row, styled, thumb, with_bar, wrap,
};
use crate::tui::theme::Role;
use crate::tui::widgets::TextField;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use serde_json::Value;

#[cfg(all(test, unix))]
mod tests;

const DETAIL_ROWS: usize = 4;
const POOL_ROWS: usize = DETAIL_ROWS - 1;
const TESTING: &str = "Testing. This can take up to 2 minutes.";
/// Why the owner cannot add a model id on a harness the engine says has no test launch,
/// word for word on the desktop (MUG2-L1).
const NO_TEST_LAUNCH: &str = "An id you add needs a test launch, and this harness has none.";
const PAGE_STEP: isize = 10;
pub(in crate::tui) const ACCOUNT_MODEL: &str = "Account's own model";
const APP_VALUE: &str = "Use the app value";
const ID_RULE: &str = "Use letters, digits and - _ . : / @ +, starting with a letter or digit.";
const OUT_OF_SHAPE: &str = "The engine's answer does not fit this page. Press r to read again.";

const PICK_OPTIONS: [(PickRule, &str, &str); 3] = [
    (
        PickRule::Strict,
        "Only the allowed list",
        "The master picks from the allowed list or uses the agent default. Any other pick is refused.",
    ),
    (
        PickRule::Hint,
        "Any offered model",
        "The master may pick any model this harness offers. The allowed list is a suggestion.",
    ),
    (
        PickRule::Default,
        "Always the default",
        "Every agent runs the agent default. The master's pick is set aside.",
    ),
];

const FALLBACK_OPTIONS: [(FallbackRule, &str, &str); 3] = [
    (
        FallbackRule::Notice,
        "Tell me",
        "When no pool account can run the chosen model, it runs on an account's own model and you and the master are told once.",
    ),
    (
        FallbackRule::Stop,
        "Stop and say why",
        "When no pool account can run the chosen model, nothing starts and the reason shows.",
    ),
    (
        FallbackRule::Silent,
        "Quietly",
        "When no pool account can run the chosen model, it runs on an account's own model and no one is told.",
    ),
];

#[derive(Debug, PartialEq)]
enum State {
    Loading,
    Failed(String),
    Ready {
        models: Models,
        rules: Option<Rules>,
    },
}

/// One row of the page: a slot, one of the two app-wide rules, the field that adds an
/// owner id, or one owner id by its index in the candidates.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Item {
    Slot(Slot),
    Pick,
    Fallback,
    Add,
    Own(usize),
}

/// A change the owner is making to the selected row: a model for the master or agent slot
/// (index 0 is no model), the ticks of the allowed list, a rule option, a typed id, or the
/// pool that runs the test launch of the owner id at `index` in the candidates. `from` is
/// where the change started, so an unchanged choice sends nothing.
#[derive(Debug)]
enum Edit {
    Choice {
        at: usize,
        from: usize,
    },
    Ticks {
        at: usize,
        offset: usize,
        ticks: Vec<String>,
        from: Vec<String>,
    },
    Rule {
        at: usize,
        from: usize,
    },
    Field(TextField),
    Pool {
        at: usize,
        index: usize,
    },
}

enum Step {
    Stay,
    Close,
    Send(ModelChoiceQuery),
}

/// The plain sentence for a model choice problem code, the same words the desktop app
/// gives. Input: the code. Output: the sentence; other codes fall back to the shared
/// settings sentences.
fn message(code: &str) -> String {
    let words = match code {
        "model_choice_custom_full" => {
            "This harness already keeps the most model ids you can add. Remove one first."
        }
        "model_choice_custom_not_pending" => {
            "This model id isn't waiting for a test launch any more. The page shows its state."
        }
        "model_choice_harness_unknown" => "CrazyAgent doesn't know this harness.",
        "model_choice_not_supported" => return format!("{NOT_SUPPORTED}."),
        "model_choice_pool_needed" => {
            "Pick a pool with at least one account to run the test launch."
        }
        "model_choice_pool_unknown" => {
            "That pool no longer exists, so nothing was tested. Pick another one."
        }
        "model_choice_test_busy" => {
            "A test launch for this harness is already running. Try again when it ends."
        }
        "model_choice_outcome_unknown" => {
            "CrazyAgent couldn't confirm what happened. Press r to read the page before you try again."
        }
        "model_choice_project_unknown" => "This project is no longer in CrazyAgent.",
        "model_choice_reply_bounds" => {
            "The engine's answer was too large to read. Press r to read again."
        }
        "model_choice_request_bounds" => {
            "This change is too large for the engine. Choose fewer models."
        }
        "model_choice_request_invalid" => {
            "The engine couldn't read this change. Press r to read again."
        }
        "model_choice_unavailable" => "Model choice isn't ready in this engine yet.",
        "model_choice_value_invalid" => {
            "This harness doesn't offer that model, or the id has characters a model id can't have."
        }
        "owner_authentication_failed" => {
            "CrazyAgent couldn't confirm this app with the engine. Restart the app, then try again."
        }
        "owner_authority_unavailable" => {
            "The engine isn't ready for owner actions yet. Try again in a moment."
        }
        "owner_busy" => "The engine is busy with other owner requests. Try again in a moment.",
        "owner_grant_stale_or_revoked" => {
            "This app's access to the engine changed. Reconnect, then try again."
        }
        "owner_request_failed" => "The engine couldn't take this request. Try again.",
        "owner_root_replaced_or_unsafe" => {
            "The engine's data folder changed or isn't safe. Restart the engine, then try again."
        }
        "owner_root_unavailable" => {
            "The engine's data folder isn't available. Restart the engine, then try again."
        }
        "owner_socket_replaced" => "The engine was restarted. Reconnect, then try again.",
        "owner_socket_unsafe" => {
            "The engine connection isn't safe to use. Restart the engine, then try again."
        }
        "peer_owner_mismatch" => {
            "The engine runs as another user. Start it as yourself, then try again."
        }
        "versioned_item_invalid" => {
            "CrazyAgent couldn't record this model choice. Press r to read again."
        }
        "versioned_unavailable" => {
            "CrazyAgent couldn't record this right now. Try again in a moment."
        }
        other => plain(other),
    };
    words.to_owned()
}

fn sentence(reason: &str) -> String {
    if reason.ends_with('.') {
        reason.to_owned()
    } else {
        format!("{reason}.")
    }
}

fn printable(id: &str) -> String {
    shown(&Value::String(id.to_owned()))
}

fn slot_label(slot: Slot) -> &'static str {
    match slot {
        Slot::Master => "Master model",
        Slot::Agent => "Agent default model",
        Slot::Allowed => "Allowed models",
    }
}

fn slot_help(slot: Slot) -> &'static str {
    match slot {
        Slot::Master => "The model the master runs on with this harness.",
        Slot::Agent => "The model an agent runs on when the master picks none.",
        Slot::Allowed => "The models the master may pick for an agent, with a short reason.",
    }
}

fn origin_word(origin: Origin) -> &'static str {
    match origin {
        Origin::Catalog => "",
        Origin::Pending => "Waiting for its test launch",
        Origin::Verified => "Your id, tested",
        Origin::Failed => "Test launch failed",
    }
}

/// The state words beside an owner id: a pending id on a harness the engine says has no
/// test launch is not supported, as nothing will test it (MUG2-L1). Inputs: the id's
/// origin and whether the harness has a test launch. Output: the words.
fn own_word(origin: Origin, testable: bool) -> String {
    if origin == Origin::Pending && !testable {
        return format!("{NOT_SUPPORTED}.");
    }
    origin_word(origin).to_owned()
}

fn source_word(source: Source) -> &'static str {
    match source {
        Source::Default => "Default",
        Source::App => "App",
        Source::Project => "Project",
        Source::Engine => "Engine",
        Source::NotApplied => "Not applied",
    }
}

/// A slot value as the page shows it: the account's own model for none, "None ticked" for
/// an empty list, the ids joined otherwise. Input: the value. Output: one line of text.
fn value_text(value: &Value) -> String {
    match value {
        Value::Null => ACCOUNT_MODEL.into(),
        Value::Array(ids) if ids.is_empty() => "None ticked".into(),
        Value::Array(ids) => ids.iter().map(shown).collect::<Vec<_>>().join(", "),
        other => shown(other),
    }
}

/// Says where a slot's value comes from, in the desktop app's words. Inputs: the row and
/// whether the page is a project's. Output: the sentence.
fn source_line(row: &Row, project: bool) -> String {
    let empty = row.slot == Slot::Allowed && row.effective.as_array().is_some_and(Vec::is_empty);
    let none = "None ticked: only the agent default runs.";
    match row.source {
        Source::Project if empty => format!("Set for this project. {none}"),
        Source::Project => "Set for this project.".into(),
        Source::App if project => format!("Uses the app value: {}.", value_text(&row.effective)),
        Source::App if empty => format!("Set here for every project. {none}"),
        Source::App => "Set here for every project.".into(),
        Source::Default if row.slot == Slot::Allowed => none.into(),
        Source::Default => "Nothing set: the account's own model runs.".into(),
        Source::Engine => "Set by CrazyAgent.".into(),
        Source::NotApplied => format!("{NOT_SUPPORTED}."),
    }
}

fn usable(models: &Models) -> Vec<&str> {
    models
        .candidates
        .iter()
        .filter(|candidate| candidate.origin.usable())
        .map(|candidate| candidate.id.as_str())
        .collect()
}

/// Says what happens to a stored value the harness no longer offers. Inputs: the row and
/// the section. Output: the sentence, none when every stored id is in use.
fn passed_over(row: &Row, models: &Models) -> Option<String> {
    let here = row.here.as_ref()?;
    if row.access != Access::Editable {
        return Some(format!("Saved {} isn't used here.", value_text(here)));
    }
    let ids = usable(models);
    match here {
        Value::Array(list) => {
            let gone: Vec<String> = list
                .iter()
                .filter(|id| !id.as_str().is_some_and(|id| ids.contains(&id)))
                .map(shown)
                .collect();
            match gone.len() {
                0 => None,
                1 => Some(format!(
                    "Saved {} isn't offered now and is left out.",
                    gone[0]
                )),
                _ => Some(format!(
                    "Saved {} aren't offered now and are left out.",
                    gone.join(", ")
                )),
            }
        }
        Value::String(id) if ids.contains(&id.as_str()) => None,
        other => Some(format!(
            "Saved {} isn't offered now, so {} runs.",
            shown(other),
            value_text(&row.effective)
        )),
    }
}

/// The ids in candidate order, keeping only usable candidates. Inputs: the ids and the
/// section. Output: the ordered ids.
fn ordered(ids: &[String], models: &Models) -> Vec<String> {
    models
        .candidates
        .iter()
        .filter(|candidate| candidate.origin.usable() && ids.contains(&candidate.id))
        .map(|candidate| candidate.id.clone())
        .collect()
}

fn ticked(row: &Row, models: &Models) -> Vec<String> {
    let on: Vec<String> = row
        .effective
        .as_array()
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let mut ticks = ordered(&on, models);
    ticks.truncate(ALLOWED_MAX);
    ticks
}

fn choice_start(row: &Row, models: &Models) -> usize {
    match &row.here {
        Some(Value::String(id)) => usable(models)
            .iter()
            .position(|usable| usable == id)
            .map_or(0, |at| at + 1),
        _ => 0,
    }
}

/// Ticks or unticks the candidate under the cursor; pending and failed ids never tick and
/// the list holds at most `ALLOWED_MAX`. Inputs: the section, the cursor, the ticks and
/// the context for the reason. Output: the ticks change in place.
fn toggle(models: &Models, at: usize, ticks: &mut Vec<String>, ctx: &mut Ctx) {
    let Some(Candidate { id, origin }) = models.candidates.get(at) else {
        return;
    };
    let id_text = printable(id);
    match origin {
        Origin::Pending => ctx.say(Message::warning(
            &format!("{id_text} can't be ticked yet."),
            "It waits for its test launch.",
        )),
        Origin::Failed => ctx.say(Message::warning(
            &format!("{id_text} can't be ticked."),
            "Its test launch failed.",
        )),
        Origin::Catalog | Origin::Verified => {
            if let Some(index) = ticks.iter().position(|tick| tick == id) {
                ticks.remove(index);
            } else if ticks.len() >= ALLOWED_MAX {
                ctx.say(Message::warning(
                    &format!("At most {ALLOWED_MAX} models can be ticked."),
                    "Untick one first.",
                ));
            } else {
                ticks.push(id.clone());
                *ticks = ordered(ticks, models);
            }
        }
    }
    assert!(ticks.len() <= ALLOWED_MAX);
}

fn refuse(access: &Access, ctx: &mut Ctx) -> bool {
    match access {
        Access::Editable => false,
        Access::Locked { reason } | Access::ReadOnly { reason } => {
            ctx.say(Message::warning(
                &sentence(reason),
                "CrazyAgent does not choose this model here.",
            ));
            true
        }
    }
}

fn row_of(models: &Models, slot: Slot) -> Option<&Row> {
    models.rows.iter().find(|row| row.slot == slot)
}

/// One harness's Models page (DEC-150): the master model, the agent default and the
/// allowed list with where each value comes from, at app scope from Settings > Harnesses
/// or as one project's overrides. The app page also holds the two app-wide model rules
/// and the owner's own model ids. Every change names the version read; one request runs
/// at a time, and a harness whose model CrazyAgent does not choose shows the reason only.
pub(in crate::tui) struct ModelsPage {
    harness: String,
    name: String,
    scope: Scope,
    versions: Option<String>,
    state: State,
    selected: usize,
    offset: usize,
    edit: Option<Edit>,
    pending: Option<ModelChoiceQuery>,
}

impl ModelsPage {
    /// The page for one harness at one scope, loading until the engine answers. Inputs:
    /// the engine's harness id, its name and the scope. Output: the screen; `shown` reads.
    pub fn new(harness: String, name: String, scope: Scope) -> Self {
        assert!(!harness.is_empty());
        let versions = shipped(&harness).map(|catalog| catalog.qualified_versions.join(", "));
        Self {
            harness,
            name,
            scope,
            versions,
            state: State::Loading,
            selected: 0,
            offset: 0,
            edit: None,
            pending: None,
        }
    }

    fn project(&self) -> Option<ProjectId> {
        match &self.scope {
            Scope::App => None,
            Scope::Project { id, .. } => Some(id.clone()),
        }
    }

    fn ask(&mut self, query: ModelChoiceQuery, ctx: &mut Ctx) {
        if self.pending.is_some() {
            return;
        }
        self.pending = Some(query.clone());
        send(ctx, SettingsJob::ModelChoice(Box::new(query)));
    }

    fn read_again(&mut self, ctx: &mut Ctx) {
        if self.pending.is_some() {
            return;
        }
        if matches!(self.state, State::Failed(_)) {
            self.state = State::Loading;
        }
        let query = ModelChoiceQuery::Read {
            harness: self.harness.clone(),
            project: self.project(),
            fresh: true,
        };
        self.ask(query, ctx);
    }

    fn fits(&self, models: &Models) -> bool {
        models.harness == self.harness
            && models.rows.iter().map(|row| row.slot).eq(Slot::ALL)
            && models.candidates.len()
                <= crate::model_choice::CATALOG_MODELS_MAX + crate::model_choice::CUSTOM_MAX
            && models
                .reason
                .as_ref()
                .is_none_or(|reason| reason.len() <= UNQUALIFIED_REASON_BYTES_MAX)
            && models
                .live_note
                .as_ref()
                .is_none_or(|note| note.len() <= crate::model_choice::live::NOTE_BYTES_MAX)
    }

    fn items(&self) -> Vec<Item> {
        let State::Ready { models, rules } = &self.state else {
            return Vec::new();
        };
        let mut items: Vec<Item> = Slot::ALL.into_iter().map(Item::Slot).collect();
        if self.project().is_some() {
            return items;
        }
        if rules.is_some() {
            items.extend([Item::Pick, Item::Fallback]);
        }
        items.push(Item::Add);
        items.extend(
            models
                .candidates
                .iter()
                .enumerate()
                .filter(|(_, candidate)| candidate.origin != Origin::Catalog)
                .map(|(index, _)| Item::Own(index)),
        );
        items
    }

    fn current(&self) -> Option<Item> {
        self.items().get(self.selected).copied()
    }

    fn first_choice(&self) -> &'static str {
        match self.scope {
            Scope::App => ACCOUNT_MODEL,
            Scope::Project { .. } => APP_VALUE,
        }
    }

    fn enter(&mut self, ctx: &mut Ctx) {
        let Some(item) = self.current() else {
            return;
        };
        let State::Ready { models, rules } = &self.state else {
            return;
        };
        let edit = match item {
            Item::Slot(slot) => {
                let Some(row) = row_of(models, slot) else {
                    return;
                };
                if refuse(&row.access, ctx) {
                    return;
                }
                if slot != Slot::Allowed {
                    let from = choice_start(row, models);
                    Edit::Choice { at: from, from }
                } else if models.candidates.is_empty() {
                    ctx.say(Message::warning(
                        &format!("{} offers no models to tick.", self.name),
                        "",
                    ));
                    return;
                } else {
                    let from = ticked(row, models);
                    Edit::Ticks {
                        at: 0,
                        offset: 0,
                        ticks: from.clone(),
                        from,
                    }
                }
            }
            Item::Pick => {
                let at = rules
                    .as_ref()
                    .and_then(|rules| {
                        PICK_OPTIONS
                            .iter()
                            .position(|(pick, ..)| *pick == rules.pick)
                    })
                    .unwrap_or(0);
                Edit::Rule { at, from: at }
            }
            Item::Fallback => {
                let at = rules
                    .as_ref()
                    .and_then(|rules| {
                        FALLBACK_OPTIONS
                            .iter()
                            .position(|(fallback, ..)| *fallback == rules.fallback)
                    })
                    .unwrap_or(0);
                Edit::Rule { at, from: at }
            }
            Item::Add if !models.testable => {
                ctx.say(Message::warning(
                    &format!("{NOT_SUPPORTED}."),
                    NO_TEST_LAUNCH,
                ));
                return;
            }
            Item::Add => Edit::Field(TextField::new("Model id", MODEL_BYTES_MAX)),
            Item::Own(_) => return,
        };
        self.edit = Some(edit);
    }

    fn reset(&mut self, ctx: &mut Ctx) {
        let Some(item) = self.current() else {
            return;
        };
        let project = self.project();
        let State::Ready { models, .. } = &self.state else {
            return;
        };
        let query = match item {
            Item::Slot(slot) => {
                let Some(row) = row_of(models, slot) else {
                    return;
                };
                if row.here.is_none() {
                    ctx.say(Message::warning(
                        "Nothing to reset.",
                        &source_line(row, project.is_some()),
                    ));
                    return;
                }
                ModelChoiceQuery::Reset {
                    harness: self.harness.clone(),
                    project,
                    slot,
                    version: row.version,
                }
            }
            Item::Own(index) => {
                let Some(candidate) = models.candidates.get(index) else {
                    return;
                };
                ModelChoiceQuery::Remove {
                    harness: self.harness.clone(),
                    model: candidate.id.clone(),
                }
            }
            Item::Pick | Item::Fallback | Item::Add => return,
        };
        self.ask(query, ctx);
    }

    /// Tells whether the test launch of one owner id is running. Input: the id. Output:
    /// true while the page waits for its answer.
    fn testing(&self, id: &str) -> bool {
        matches!(&self.pending, Some(ModelChoiceQuery::Test { model, .. }) if model == id)
    }

    /// Tells whether an owner id can be tested now: it waits for its test launch, this
    /// harness has one and the id is not being tested. Input: the candidate. Output: true
    /// when t offers the test.
    fn testable(&self, candidate: &Candidate) -> bool {
        let State::Ready { models, .. } = &self.state else {
            return false;
        };
        candidate.origin == Origin::Pending && models.testable && !self.testing(&candidate.id)
    }

    /// Opens the pool choice of the selected owner id's test launch and reads the pools,
    /// when the id can be tested. Input: the context. Output: unit.
    fn test(&mut self, ctx: &mut Ctx) {
        let Some(Item::Own(index)) = self.current() else {
            return;
        };
        let State::Ready { models, .. } = &self.state else {
            return;
        };
        if !models
            .candidates
            .get(index)
            .is_some_and(|candidate| self.testable(candidate))
        {
            return;
        }
        ctx.request(Request::Pools);
        self.edit = Some(Edit::Pool { at: 0, index });
    }

    fn edit_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let item = self.current();
        let project = self.project();
        let Some(item) = item else {
            self.edit = None;
            return Flow::Handled;
        };
        let State::Ready { models, rules } = &self.state else {
            self.edit = None;
            return Flow::Handled;
        };
        let Some(edit) = self.edit.as_mut() else {
            return Flow::Handled;
        };
        let row = match item {
            Item::Slot(slot) => row_of(models, slot),
            _ => None,
        };
        let ids = usable(models);
        let choices = ids.len() + 1;
        let options = PICK_OPTIONS.len();
        let last = models.candidates.len().saturating_sub(1);
        let pools: Vec<_> = match &ctx.store.pools {
            Load::Ready(pools) => pools.iter().map(|pool| pool.id.clone()).collect(),
            _ => Vec::new(),
        };
        let step = match (edit, key) {
            (_, Key::Esc) => Step::Close,
            (Edit::Choice { at, .. }, Key::Left) => {
                *at = (*at + choices - 1) % choices;
                Step::Stay
            }
            (Edit::Choice { at, .. }, Key::Right) => {
                *at = (*at + 1) % choices;
                Step::Stay
            }
            (Edit::Choice { at, from }, Key::Enter) => match row {
                Some(row) if *at != *from => match (*at, ids.get(at.wrapping_sub(1))) {
                    (0, _) => Step::Send(ModelChoiceQuery::Reset {
                        harness: self.harness.clone(),
                        project,
                        slot: row.slot,
                        version: row.version,
                    }),
                    (_, Some(id)) => Step::Send(ModelChoiceQuery::Set {
                        harness: self.harness.clone(),
                        project,
                        slot: row.slot,
                        value: Value::String((*id).to_owned()),
                        version: row.version,
                    }),
                    (_, None) => Step::Close,
                },
                _ => Step::Close,
            },
            (Edit::Rule { at, .. }, Key::Left) => {
                *at = (*at + options - 1) % options;
                Step::Stay
            }
            (Edit::Rule { at, .. }, Key::Right) => {
                *at = (*at + 1) % options;
                Step::Stay
            }
            (Edit::Rule { at, from }, Key::Enter) => match (item, rules) {
                (Item::Pick, Some(rules)) if *at != *from => {
                    Step::Send(ModelChoiceQuery::SetRule {
                        rule: Rule::Pick(PICK_OPTIONS[*at].0),
                        version: rules.pick_version,
                    })
                }
                (Item::Fallback, Some(rules)) if *at != *from => {
                    Step::Send(ModelChoiceQuery::SetRule {
                        rule: Rule::Fallback(FALLBACK_OPTIONS[*at].0),
                        version: rules.fallback_version,
                    })
                }
                _ => Step::Close,
            },
            (Edit::Ticks { at, .. }, Key::Up) => {
                *at = at.saturating_sub(1);
                Step::Stay
            }
            (Edit::Ticks { at, .. }, Key::Down) => {
                *at = (*at + 1).min(last);
                Step::Stay
            }
            (Edit::Ticks { at, ticks, .. }, Key::Char(' ')) => {
                toggle(models, *at, ticks, ctx);
                Step::Stay
            }
            (Edit::Ticks { ticks, from, .. }, Key::Enter) => match row {
                Some(row) if ticks != from => Step::Send(ModelChoiceQuery::Set {
                    harness: self.harness.clone(),
                    project,
                    slot: Slot::Allowed,
                    value: Value::from(ticks.clone()),
                    version: row.version,
                }),
                _ => Step::Close,
            },
            (Edit::Field(field), Key::Enter) => {
                let id = field.value().trim().to_owned();
                if id.is_empty() {
                    field.error = Some("Type a model id.".into());
                    Step::Stay
                } else if !model_valid(&self.harness, &id) {
                    field.error = Some(ID_RULE.into());
                    Step::Stay
                } else {
                    Step::Send(ModelChoiceQuery::Add {
                        harness: self.harness.clone(),
                        model: id,
                    })
                }
            }
            (Edit::Field(field), key) => {
                if field.key(key) {
                    field.error = None;
                }
                Step::Stay
            }
            (Edit::Pool { at, .. }, Key::Up) => {
                *at = at.saturating_sub(1);
                Step::Stay
            }
            (Edit::Pool { at, .. }, Key::Down) => {
                *at = (*at + 1).min(pools.len().saturating_sub(1));
                Step::Stay
            }
            (Edit::Pool { at, index }, Key::Enter) => {
                match (models.candidates.get(*index), pools.get(*at)) {
                    (Some(candidate), Some(pool)) if candidate.origin == Origin::Pending => {
                        Step::Send(ModelChoiceQuery::Test {
                            harness: self.harness.clone(),
                            model: candidate.id.clone(),
                            pool: pool.clone(),
                        })
                    }
                    _ => Step::Stay,
                }
            }
            _ => Step::Stay,
        };
        match step {
            Step::Stay => {}
            Step::Close => self.edit = None,
            Step::Send(_) if self.pending.is_some() => {
                ctx.say(Message::warning(
                    "One change at a time.",
                    "Wait for the last one to finish, then try again.",
                ));
                if !matches!(self.edit, Some(Edit::Field(_))) {
                    self.edit = None;
                }
            }
            Step::Send(query) => {
                self.edit = None;
                self.ask(query, ctx);
            }
        }
        Flow::Handled
    }

    fn move_by(&mut self, delta: isize) {
        let count = self.items().len();
        if count == 0 {
            self.selected = 0;
            return;
        }
        let target = self.selected as isize + delta;
        self.selected = target.clamp(0, count as isize - 1) as usize;
    }

    /// Keeps the cursor on a row after the section changed and closes every open change
    /// but a typed id, whose options may have moved.
    fn settle(&mut self) {
        let count = self.items().len();
        self.selected = self.selected.min(count.saturating_sub(1));
        if !matches!(self.edit, Some(Edit::Field(_))) {
            self.edit = None;
        }
    }

    fn replace(&mut self, row: &Row) {
        if let State::Ready { models, .. } = &mut self.state
            && let Some(old) = models.rows.iter_mut().find(|old| old.slot == row.slot)
        {
            *old = row.clone();
        }
        self.settle();
    }

    fn take_models(&mut self, next: &Models, ctx: &mut Ctx) -> bool {
        if !self.fits(next) {
            ctx.say(Message::error(
                &format!("Could not read the {} models.", self.name),
                OUT_OF_SHAPE,
            ));
            return false;
        }
        if let State::Ready { models, .. } = &mut self.state {
            *models = next.clone();
        }
        self.settle();
        true
    }

    fn read_failed(&mut self, problem: String, ctx: &mut Ctx) {
        match self.state {
            State::Ready { .. } => ctx.say(Message::error(
                &format!("Could not read the {} models.", self.name),
                &problem,
            )),
            _ => self.state = State::Failed(problem),
        }
    }

    fn apply(&mut self, event: &ModelChoiceEvent, ctx: &mut Ctx) {
        if self.pending.as_ref() != Some(&event.query) {
            return;
        }
        self.pending = None;
        let name = self.name.clone();
        let changed = "This is the current value; change it again if you still want to.";
        match (&event.query, &event.result) {
            (ModelChoiceQuery::Read { .. }, Ok(ModelChoiceReply::Read { models, rules })) => {
                if !self.fits(models) {
                    self.read_failed(OUT_OF_SHAPE.into(), ctx);
                    return;
                }
                self.state = State::Ready {
                    models: models.clone(),
                    rules: rules.clone(),
                };
                self.settle();
            }
            (ModelChoiceQuery::Read { .. }, Err(code)) => self.read_failed(message(code), ctx),
            (_, Ok(ModelChoiceReply::Written(Written::Saved { row }))) => {
                self.replace(row);
                ctx.say(Message::success(
                    "Saved.",
                    &format!("The next {name} start uses it."),
                ));
            }
            (_, Ok(ModelChoiceReply::Written(Written::Stale { row }))) => {
                self.replace(row);
                ctx.say(Message::warning("Changed somewhere else.", changed));
            }
            (_, Ok(ModelChoiceReply::Written(Written::Refused { code, row }))) => {
                self.replace(row);
                let reason = match &row.access {
                    Access::Locked { reason } | Access::ReadOnly { reason } => sentence(reason),
                    Access::Editable => String::new(),
                };
                ctx.say(Message::warning(&message(code), &reason));
            }
            (ModelChoiceQuery::Add { model, .. }, Ok(ModelChoiceReply::Added(added, models))) => {
                if !self.take_models(models, ctx) {
                    return;
                }
                let id = printable(model);
                ctx.say(match added {
                    ModelAdded::Pending => Message::success(
                        &format!("Added {id}."),
                        "It waits for its test launch before it can be ticked.",
                    ),
                    ModelAdded::Listed => Message::warning(
                        &format!("{id} is already on the {name} model list."),
                        "Nothing was added.",
                    ),
                    ModelAdded::Verified => Message::warning(
                        &format!("{id} already passed its test launch."),
                        "Nothing was added.",
                    ),
                });
            }
            (
                ModelChoiceQuery::Remove { model, .. },
                Ok(ModelChoiceReply::Removed(removed, models)),
            ) => {
                if !self.take_models(models, ctx) {
                    return;
                }
                let id = printable(model);
                ctx.say(if *removed {
                    Message::success(&format!("Removed {id}."), "")
                } else {
                    Message::warning(&format!("{id} was already gone."), "")
                });
            }
            (
                ModelChoiceQuery::Test { model, .. },
                Ok(ModelChoiceReply::Tested(tested, reason, models)),
            ) => {
                if !self.take_models(models, ctx) {
                    return;
                }
                let id = printable(model);
                ctx.say(match tested {
                    Tested::Verified => Message::success(
                        &format!("{id} passed its test launch."),
                        "You can now choose it, tick it and launch it.",
                    ),
                    Tested::Failed => Message::warning(
                        &format!("{id} failed its test launch."),
                        "The provider refused this model. It stays listed and can't be used.",
                    ),
                    Tested::Pending => Message::warning(
                        &format!("{id} still waits for its test launch."),
                        &reason.as_deref().map(printable).unwrap_or_default(),
                    ),
                });
            }
            (ModelChoiceQuery::Test { model, .. }, Err(code)) => ctx.say(Message::error(
                &format!("Could not test {}.", printable(model)),
                &message(code),
            )),
            (_, Ok(ModelChoiceReply::RuleWritten(written))) => {
                let (rules, saved) = match written {
                    RuleWritten::Saved { rules } => (rules, true),
                    RuleWritten::Stale { rules } => (rules, false),
                };
                if let State::Ready { rules: current, .. } = &mut self.state {
                    *current = Some(rules.clone());
                }
                self.settle();
                ctx.say(if saved {
                    Message::success("Saved.", "The next agent start uses it.")
                } else {
                    Message::warning("Changed somewhere else.", changed)
                });
            }
            (_, Err(code)) => ctx.say(Message::error(
                "Could not change the model choice.",
                &message(code),
            )),
            (_, Ok(_)) => {}
        }
    }

    fn widths(limit: usize, wide: bool) -> [usize; 3] {
        let (first, third) = if wide { (28, 12) } else { (22, 11) };
        [
            first,
            limit.saturating_sub(2 + first + third + 2 * 2).max(1),
            third,
        ]
    }

    fn rule_value(&self, item: Item, rules: Option<&Rules>, selected: bool) -> String {
        let editing = match (&self.edit, selected) {
            (Some(Edit::Rule { at, .. }), true) => Some(*at),
            _ => None,
        };
        let label = match item {
            Item::Pick => editing
                .or_else(|| {
                    rules.and_then(|rules| {
                        PICK_OPTIONS
                            .iter()
                            .position(|(pick, ..)| *pick == rules.pick)
                    })
                })
                .map(|at| PICK_OPTIONS[at].1),
            _ => editing
                .or_else(|| {
                    rules.and_then(|rules| {
                        FALLBACK_OPTIONS
                            .iter()
                            .position(|(fallback, ..)| *fallback == rules.fallback)
                    })
                })
                .map(|at| FALLBACK_OPTIONS[at].1),
        };
        label.unwrap_or_default().to_owned()
    }

    /// The three cells of one row. Inputs: the row, the section, the rules, whether it is
    /// selected and the context. Output: the setting, value and from cells.
    fn row_cells(
        &self,
        item: Item,
        models: &Models,
        rules: Option<&Rules>,
        selected: bool,
        ctx: &Ctx,
    ) -> Vec<Line<'static>> {
        let glyphs = ctx.glyphs;
        let muted = |text: &str| styled(text.to_owned(), Role::Muted, &ctx.theme);
        let around = |text: &str| format!("{} {text} {}", glyphs.left, glyphs.right);
        match item {
            Item::Slot(slot) => {
                let Some(row) = row_of(models, slot) else {
                    return vec![
                        Line::raw(slot_label(slot)),
                        Line::default(),
                        Line::default(),
                    ];
                };
                let value = match (&row.access, &self.edit) {
                    (Access::Locked { reason } | Access::ReadOnly { reason }, _) => {
                        styled(reason.clone(), Role::Warning, &ctx.theme)
                    }
                    (Access::Editable, Some(Edit::Choice { at, .. })) if selected => {
                        let choice = match *at {
                            0 => self.first_choice().to_owned(),
                            at => usable(models)
                                .get(at - 1)
                                .map_or_else(String::new, |id| printable(id)),
                        };
                        Line::raw(around(&choice))
                    }
                    (Access::Editable, _) => Line::raw(value_text(&row.effective)),
                };
                vec![
                    Line::raw(slot_label(slot)),
                    value,
                    Line::raw(source_word(row.source)),
                ]
            }
            Item::Pick | Item::Fallback => {
                let label = if item == Item::Pick {
                    "Pick rule"
                } else {
                    "Fallback rule"
                };
                let value = self.rule_value(item, rules, selected);
                let value = match &self.edit {
                    Some(Edit::Rule { .. }) if selected => around(&value),
                    _ => value,
                };
                vec![Line::raw(label), Line::raw(value), Line::raw("App")]
            }
            Item::Add => vec![
                Line::raw("Add a model id"),
                if models.testable {
                    muted("Enter to type one")
                } else {
                    muted(&format!("{NOT_SUPPORTED}."))
                },
                Line::default(),
            ],
            Item::Own(index) => match models.candidates.get(index) {
                Some(candidate) => vec![
                    Line::raw(printable(&candidate.id)),
                    if self.testing(&candidate.id) {
                        styled("Testing".to_owned(), Role::Accent, &ctx.theme)
                    } else {
                        muted(&own_word(candidate.origin, models.testable))
                    },
                    Line::raw("Yours"),
                ],
                None => vec![Line::default(), Line::default(), Line::default()],
            },
        }
    }

    fn detail(
        &self,
        item: Item,
        models: &Models,
        rules: Option<&Rules>,
        limit: usize,
        ctx: &Ctx,
    ) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        let lines = |text: &str, role: Option<Role>| -> Vec<Line<'static>> {
            wrap(text, limit)
                .into_iter()
                .map(|part| match role {
                    Some(role) => styled(part, role, theme),
                    None => Line::raw(part),
                })
                .collect()
        };
        let mut out = Vec::new();
        let mut cursor = None;
        match item {
            Item::Slot(slot) => {
                out.extend(lines(slot_help(slot), None));
                if let Some(row) = row_of(models, slot) {
                    match &row.access {
                        Access::Locked { reason } | Access::ReadOnly { reason } => {
                            out.extend(lines(&sentence(reason), Some(Role::Warning)));
                        }
                        Access::Editable => out.extend(lines(
                            &source_line(row, self.project().is_some()),
                            Some(Role::Muted),
                        )),
                    }
                    if let Some(line) = passed_over(row, models) {
                        out.extend(lines(&line, Some(Role::Warning)));
                    }
                }
            }
            Item::Pick | Item::Fallback => {
                let question = if item == Item::Pick {
                    "What the master may pick."
                } else {
                    "When no account can run the model."
                };
                out.extend(lines(question, None));
                let value = self.rule_value(item, rules, true);
                let help = PICK_OPTIONS
                    .iter()
                    .map(|(_, label, help)| (label, help))
                    .chain(
                        FALLBACK_OPTIONS
                            .iter()
                            .map(|(_, label, help)| (label, help)),
                    )
                    .find(|(label, _)| **label == value)
                    .map_or("", |(_, help)| *help);
                out.extend(lines(help, Some(Role::Muted)));
            }
            Item::Add => match &self.edit {
                Some(Edit::Field(field)) => {
                    let (field_lines, at) = field.lines(limit.max(6), true, theme, ctx.glyphs);
                    cursor = at;
                    out.extend(field_lines);
                }
                _ if !models.testable => {
                    out.extend(lines(&format!("{NOT_SUPPORTED}."), None));
                    out.extend(lines(NO_TEST_LAUNCH, Some(Role::Muted)));
                }
                _ => out.extend(lines(
                    &format!(
                        "Type an id {} takes. It waits for a test launch before it can be ticked.",
                        self.name
                    ),
                    Some(Role::Muted),
                )),
            },
            Item::Own(index) => {
                let Some(candidate) = models.candidates.get(index) else {
                    return (out, cursor);
                };
                if let Some(Edit::Pool { at, .. }) = &self.edit {
                    out.extend(lines(
                        &format!(
                            "Which pool runs the test launch of {}?",
                            printable(&candidate.id)
                        ),
                        None,
                    ));
                    out.extend(self.pool_lines(*at, limit, ctx));
                    out.truncate(DETAIL_ROWS);
                    return (out, cursor);
                }
                if self.testing(&candidate.id) {
                    out.extend(lines(TESTING, Some(Role::Accent)));
                    return (out, cursor);
                }
                if candidate.origin == Origin::Pending && !models.testable {
                    out.extend(lines(&format!("{NOT_SUPPORTED}."), None));
                    out.extend(lines(NO_TEST_LAUNCH, Some(Role::Muted)));
                    return (out, cursor);
                }
                let state = match candidate.origin {
                    Origin::Pending => "It waits for its test launch before it can be ticked.",
                    Origin::Verified => "It passed its test launch and can be ticked.",
                    Origin::Failed => "Its test launch failed, so it can't be ticked.",
                    Origin::Catalog => "",
                };
                out.extend(lines(state, None));
                if self.testable(candidate) {
                    out.extend(lines("t tests it on a pool you pick.", Some(Role::Muted)));
                }
                out.extend(lines("x removes it.", Some(Role::Muted)));
            }
        }
        out.truncate(DETAIL_ROWS);
        (out, cursor)
    }

    /// The pools the owner picks from for a test launch, a window of at most `POOL_ROWS`
    /// around the one at `at`, or why there is none to pick. Inputs: the picked index, the
    /// width and the context. Output: the lines.
    fn pool_lines(&self, at: usize, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let say = |text: &str, role: Role| styled(text.to_owned(), role, theme);
        match &ctx.store.pools {
            Load::Ready(pools) if pools.is_empty() => vec![say(
                "No pool yet. Add one under Pools, then try again.",
                Role::Warning,
            )],
            Load::Ready(pools) => {
                let at = at.min(pools.len() - 1);
                let first = at.saturating_sub(POOL_ROWS - 1);
                pools
                    .iter()
                    .enumerate()
                    .skip(first)
                    .take(POOL_ROWS)
                    .map(|(index, pool)| {
                        let marker = if index == at { ">" } else { " " };
                        let role = if index == at {
                            Role::Accent
                        } else {
                            Role::Muted
                        };
                        let name = printable(&pool.name);
                        say(
                            &cut(&format!("{marker} {name}"), limit, ctx.glyphs.ellipsis),
                            role,
                        )
                    })
                    .collect()
            }
            Load::Failed(_) | Load::NotReady => vec![say(
                "The pools couldn't be read. Press Esc, then try again.",
                Role::Error,
            )],
            _ => vec![say("Reading the pools.", Role::Muted)],
        }
    }

    /// The tick list of the allowed models while it is open: every candidate with its tick
    /// and, for an owner id, its state. Inputs: the lines so far, the section, the width,
    /// the rows it may fill and the context. Output: the lines.
    fn tick_lines(
        &mut self,
        mut lines: Vec<Line<'static>>,
        models: &Models,
        limit: usize,
        height: usize,
        ctx: &Ctx,
    ) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let Some(Edit::Ticks {
            at, offset, ticks, ..
        }) = &mut self.edit
        else {
            return lines;
        };
        lines.push(Line::default());
        lines.push(styled(
            format!(
                "Allowed models: {} of {ALLOWED_MAX} ticked. Space ticks, Enter saves.",
                ticks.len()
            ),
            Role::Heading,
            theme,
        ));
        let count = models.candidates.len();
        if count == 0 {
            return lines;
        }
        let window = height.saturating_sub(lines.len()).max(1).min(count);
        let row_limit = if count > window { limit - 1 } else { limit };
        let selected = (*at).min(count - 1);
        let mut top = (*offset).min(count - window);
        if selected < top {
            top = selected;
        } else if selected >= top + window {
            top = selected + 1 - window;
        }
        let first = (row_limit / 2).max(1);
        let widths = [first, row_limit.saturating_sub(2 + first + 2).max(1)];
        let rows: Vec<Line<'static>> = models
            .candidates
            .iter()
            .enumerate()
            .skip(top)
            .take(window)
            .map(|(index, candidate)| {
                let mark = if ticks.contains(&candidate.id) {
                    "x"
                } else {
                    " "
                };
                let tick = format!("[{mark}] {}", printable(&candidate.id));
                let note = styled(origin_word(candidate.origin), Role::Muted, theme);
                list_row(
                    cells(&widths, vec![Line::raw(tick), note], 2, glyphs),
                    row_limit,
                    index == selected,
                    true,
                    theme,
                    glyphs,
                )
            })
            .collect();
        if count > window {
            let (bar, size) = thumb(window, top, count);
            lines.extend(with_bar(rows, limit, bar, size, theme, glyphs));
        } else {
            lines.extend(rows);
        }
        *at = selected;
        *offset = top;
        lines
    }

    /// The page's lines below the tab strip. Inputs: the content width, the rows it may
    /// fill and the context. Output: the lines and the cursor of an open field.
    fn body(
        &mut self,
        limit: usize,
        height: usize,
        ctx: &Ctx,
    ) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let name = self.name.clone();
        let title = match &self.scope {
            Scope::App => format!("{name} models"),
            Scope::Project { name: project, .. } => format!("{name} models for {project}"),
        };
        let checked = match &self.versions {
            Some(versions) => styled(format!("Checked against {versions}"), Role::Muted, theme),
            None => styled("No model list", Role::Warning, theme),
        };
        let mut lines = vec![row(
            limit,
            styled(title, Role::Accent, theme),
            checked,
            glyphs,
        )];
        let intro = match &self.scope {
            Scope::App => "Every project uses these unless it sets its own.".to_owned(),
            Scope::Project { name: project, .. } => {
                format!("Only {project} uses values set here; App rows follow Settings.")
            }
        };
        lines.extend(
            wrap(&intro, limit)
                .into_iter()
                .map(|part| styled(part, Role::Muted, theme)),
        );
        let (models, rules) = match &self.state {
            State::Loading => {
                lines.push(Line::default());
                lines.push(ctx.loading(&format!("Reading the {name} models")));
                return (lines, None);
            }
            State::Failed(problem) => {
                lines.push(Line::default());
                lines.extend(
                    wrap(
                        &format!("Could not read the {name} models. {problem}"),
                        limit,
                    )
                    .into_iter()
                    .map(|part| styled(part, Role::Error, theme)),
                );
                lines.push(styled("Press r to try again.", Role::Muted, theme));
                return (lines, None);
            }
            State::Ready { models, rules } => (models.clone(), rules.clone()),
        };
        if let (false, Some(reason)) = (models.qualified, &models.reason) {
            lines.extend(
                wrap(
                    &format!("Version not checked: {}.", printable(reason)),
                    limit,
                )
                .into_iter()
                .map(|part| styled(part, Role::Warning, theme)),
            );
        }
        if let Some(note) = &models.live_note {
            lines.extend(
                wrap(&format!("{}.", printable(note)), limit)
                    .into_iter()
                    .map(|part| styled(part, Role::Muted, theme)),
            );
        }
        if matches!(self.edit, Some(Edit::Ticks { .. })) {
            return (self.tick_lines(lines, &models, limit, height, ctx), None);
        }
        lines.push(Line::default());
        let items = self.items();
        let count = items.len();
        assert!(count >= Slot::ALL.len());
        let window = height
            .saturating_sub(lines.len() + 2 + DETAIL_ROWS)
            .max(1)
            .min(count);
        let row_limit = if count > window { limit - 1 } else { limit };
        let widths = Self::widths(row_limit, ctx.wide());
        lines.push(join([
            Line::raw("  "),
            restyle(
                cells(
                    &widths,
                    ["Setting", "Value", "From"]
                        .into_iter()
                        .map(Line::raw)
                        .collect(),
                    2,
                    glyphs,
                ),
                theme.muted,
            ),
        ]));
        let selected = self.selected.min(count - 1);
        let mut offset = self.offset.min(count - window);
        if selected < offset {
            offset = selected;
        } else if selected >= offset + window {
            offset = selected + 1 - window;
        }
        let rows: Vec<Line<'static>> = items
            .iter()
            .enumerate()
            .skip(offset)
            .take(window)
            .map(|(index, item)| {
                let values = self.row_cells(*item, &models, rules.as_ref(), index == selected, ctx);
                list_row(
                    cells(&widths, values, 2, glyphs),
                    row_limit,
                    index == selected,
                    true,
                    theme,
                    glyphs,
                )
            })
            .collect();
        if count > window {
            let (top, size) = thumb(window, offset, count);
            lines.extend(with_bar(rows, limit, top, size, theme, glyphs));
        } else {
            lines.extend(rows);
        }
        lines.push(Line::default());
        let start = lines.len() as u16;
        let (detail, cursor) = self.detail(items[selected], &models, rules.as_ref(), limit, ctx);
        lines.extend(detail);
        self.selected = selected;
        self.offset = offset;
        (lines, cursor.map(|at| Position::new(at.x, start + at.y)))
    }
}

impl Screen for ModelsPage {
    fn crumbs(&self) -> Vec<String> {
        let mut crumbs = match &self.scope {
            Scope::App => vec!["Settings".into(), "Harnesses".into()],
            Scope::Project { name, .. } => vec![name.clone(), "Harnesses".into()],
        };
        crumbs.extend([self.name.clone(), "Models".into()]);
        crumbs
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let sides = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
        let moves = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        match &self.edit {
            Some(Edit::Choice { .. } | Edit::Rule { .. }) => {
                return vec![
                    hint(&sides, "Pick"),
                    hint("Enter", "Save"),
                    hint("Esc", "Cancel"),
                ];
            }
            Some(Edit::Ticks { .. }) => {
                return vec![
                    hint(&moves, "Move"),
                    hint("Space", "Tick"),
                    hint("Enter", "Save"),
                    hint("Esc", "Cancel"),
                ];
            }
            Some(Edit::Field(_)) => return vec![hint("Enter", "Add"), hint("Esc", "Cancel")],
            Some(Edit::Pool { .. }) => {
                return vec![
                    hint(&moves, "Pick"),
                    hint("Enter", "Test"),
                    hint("Esc", "Cancel"),
                ];
            }
            None => {}
        }
        match self.state {
            State::Loading => return vec![hint("Esc", "Back")],
            State::Failed(_) => return vec![hint("r", "Try again"), hint("Esc", "Back")],
            State::Ready { .. } => {}
        }
        let mut hints = Vec::new();
        let State::Ready { models, .. } = &self.state else {
            return hints;
        };
        match self.current() {
            Some(Item::Slot(slot)) => {
                if let Some(row) = row_of(models, slot) {
                    if row.access == Access::Editable {
                        hints.push(hint("Enter", "Choose"));
                    }
                    if row.here.is_some() {
                        hints.push(hint("x", "Reset"));
                    }
                }
            }
            Some(Item::Pick | Item::Fallback) => hints.push(hint("Enter", "Choose")),
            Some(Item::Add) if models.testable => hints.push(hint("Enter", "Type an id")),
            Some(Item::Add) => {}
            Some(Item::Own(index)) => {
                if models
                    .candidates
                    .get(index)
                    .is_some_and(|candidate| self.testable(candidate))
                {
                    hints.push(hint("t", "Test"));
                }
                hints.push(hint("x", "Remove"));
            }
            None => {}
        }
        hints.push(hint(&moves, "Move"));
        hints.push(hint("r", "Read again"));
        hints.push(hint("Esc", "Back"));
        hints
    }

    fn typing(&self) -> bool {
        matches!(self.edit, Some(Edit::Field(_)))
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.edit.is_some() {
            return self.edit_key(key, ctx);
        }
        match key {
            Key::Char('r') => self.read_again(ctx),
            Key::Up => self.move_by(-1),
            Key::Down => self.move_by(1),
            Key::PageUp => self.move_by(-PAGE_STEP),
            Key::PageDown => self.move_by(PAGE_STEP),
            Key::Home => self.selected = 0,
            Key::End => self.move_by(isize::MAX / 2),
            Key::Enter => self.enter(ctx),
            Key::Char('x') => self.reset(ctx),
            Key::Char('t') => self.test(ctx),
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        if let Some(SettingsEvent::ModelChoice(event)) = settings_event(arrival) {
            self.apply(event, ctx);
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if self.state == State::Loading {
            self.read_again(ctx);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = (area.width as usize).saturating_sub(4).max(8);
        match self.scope {
            Scope::App => {
                let height = (area.height as usize).saturating_sub(2);
                let (content, cursor) = self.body(limit, height, ctx);
                draw_page(
                    frame,
                    area,
                    SettingsSection::Harnesses,
                    content,
                    cursor,
                    ctx,
                )
            }
            Scope::Project { .. } => {
                let (content, cursor) = self.body(limit, area.height as usize, ctx);
                draw_lines(frame.buffer_mut(), area, &indent(content, 2));
                cursor.map(|at| Position::new(area.x + 2 + at.x, area.y + at.y))
            }
        }
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.pending.is_some()
    }

    fn draft(&self) -> bool {
        matches!(self.edit, Some(Edit::Field(_)))
    }

    fn loss(&self) -> &'static str {
        "The model id you typed will be lost."
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Models {
            harness: self.harness.clone(),
            name: self.name.clone(),
            project: match &self.scope {
                Scope::App => None,
                Scope::Project { id, name } => Some((id.clone(), name.clone())),
            },
        })
    }
}
