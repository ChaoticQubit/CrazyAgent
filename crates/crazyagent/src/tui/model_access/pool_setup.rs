use super::*;
use std::collections::BTreeSet;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Label,
    Load,
    Members,
    Review,
}

pub(super) struct Setup {
    basis: Basis,
    pool: Option<AccountPool>,
    replace: bool,
    label: String,
    members: BTreeSet<CredentialRef>,
    stage: Stage,
    page: Vec<ModelAccessRecord>,
    cursor: Option<ModelAccessCursor>,
    next: Option<ModelAccessCursor>,
    selected: usize,
}

impl Setup {
    fn new(basis: Basis, pool: Option<AccountPool>) -> Self {
        Self {
            basis,
            replace: pool.is_some(),
            members: pool
                .as_ref()
                .map(|pool| pool.members.iter().cloned().collect())
                .unwrap_or_default(),
            stage: if pool.is_some() {
                Stage::Load
            } else {
                Stage::Label
            },
            pool,
            label: String::new(),
            page: Vec::new(),
            cursor: None,
            next: None,
            selected: 0,
        }
    }

    fn create(&mut self) -> Result<(), String> {
        assert!(self.pool.is_none());
        let nonce = crate::engine::owner::random_id().map_err(|error| error.to_string())?;
        let stem: String = self
            .label
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(24)
            .flat_map(char::to_lowercase)
            .collect();
        self.pool = Some(AccountPool {
            id: CredentialRef::new(format!("pool-{stem}-{nonce}"))
                .map_err(|error| error.to_string())?,
            members: Vec::new(),
            projects: Vec::new(),
            roles: Vec::new(),
            fallback_allowed: false,
            provider_quota_policy: ProviderQuotaPolicy::default(),
        });
        self.stage = Stage::Load;
        Ok(())
    }

    fn edit(&self) -> Result<ModelAccessEdit, String> {
        if self.members.is_empty() || self.members.len() > 64 {
            return Err("Choose between 1 and 64 saved connections".into());
        }
        let mut pool = self.pool.clone().ok_or("Confirm a pool label first")?;
        pool.members = self.members.iter().cloned().collect();
        Ok(if self.replace {
            ModelAccessEdit::ReplacePool { pool }
        } else {
            ModelAccessEdit::CreatePool { pool }
        })
    }

    pub(super) fn render(&self, notice: &str, pending: Option<String>) -> String {
        let mut lines = vec!["Account pool".into()];
        if self.stage == Stage::Label {
            lines.extend([
                "Pool label (optional)".into(),
                self.label.clone(),
                "Enter chooses saved connections | Esc cancels".into(),
            ]);
        } else {
            lines.push("Choose connections".into());
            if let Some(pool) = &self.pool {
                lines.push(pool.id.as_str().to_owned());
            }
            lines.push(format!("{} selected across all pages", self.members.len()));
            for (index, record) in self.page.iter().enumerate() {
                lines.push(format!(
                    "{} [{}] {}",
                    if index == self.selected { ">" } else { " " },
                    if self.members.contains(record.id()) {
                        "x"
                    } else {
                        " "
                    },
                    record.id().as_str()
                ));
            }
            lines.push("Space selects | Up/Down moves | Enter saves | Esc closes".into());
            lines.push("Ctrl-N next page | Ctrl-P first page | Ctrl-R checks pending save".into());
            if self.stage == Stage::Review {
                lines.push("Source changed; close and reopen to review the current pool".into());
            }
        }
        lines.push(notice.chars().take(240).collect());
        if let Some(pending) = pending {
            lines.push(pending);
        }
        lines.join("\n")
    }

    pub(super) fn assert_consistent(&self) {
        assert!(self.label.len() <= 128);
        assert!(!self.label.chars().any(char::is_control));
        assert!(self.members.len() <= 64);
        assert!(self.page.len() <= 16);
        assert!(self.page.is_empty() || self.selected < self.page.len());
        assert_eq!(self.pool.is_none(), self.stage == Stage::Label);
    }
}

impl State {
    pub(super) fn begin_pool_setup(
        &mut self,
        basis: Basis,
        pool: Option<AccountPool>,
    ) -> Result<(), String> {
        if !basis.writable || self.loading() || self.has_pending() {
            return Err("Wait for a current writable owner pool read".into());
        }
        self.pool_setup = Some(Setup::new(basis, pool));
        self.action = RecordAction::View;
        self.notice = "Select saved connections; account authentication remains separate".into();
        Ok(())
    }

    pub(super) fn request_pool_setup(&mut self) -> Result<(), String> {
        if self.loading() || self.has_pending() {
            return Ok(());
        }
        if let Some(setup) = &self.pool_setup
            && setup.stage == Stage::Load
        {
            self.read(OwnerRead::ModelAccessPage {
                collection: ModelAccessCollection::Connections,
                cursor: setup.cursor.clone(),
                limit: 16,
            })?;
        }
        Ok(())
    }

    pub(super) fn reduce_pool_setup(&mut self, event: &UiEvent) -> Result<bool, String> {
        if matches!(
            event,
            UiEvent::Resize(..) | UiEvent::Scroll(_) | UiEvent::FocusNext | UiEvent::FocusPrevious
        ) {
            return Ok(false);
        }
        if self.loading() || self.has_pending() {
            self.notice = "Pool save pending; Ctrl-R checks the original request".into();
            return Ok(true);
        }
        if matches!(event, UiEvent::Cancel) {
            self.pool_setup = None;
            self.needs_page = true;
            self.notice = "Pool editor closed; no new save requested".into();
            return Ok(true);
        }
        let result = self.edit_pool_setup(event);
        if let Err(problem) = &result {
            self.notice = problem.clone();
        }
        self.assert_consistent();
        result.map(|()| true)
    }

    fn edit_pool_setup(&mut self, event: &UiEvent) -> Result<(), String> {
        let setup = self.pool_setup.as_mut().unwrap();
        if setup.stage == Stage::Review {
            return Ok(());
        }
        match (setup.stage, event) {
            (Stage::Label, UiEvent::Character(character)) => {
                if character.is_control() || setup.label.len() + character.len_utf8() > 128 {
                    return Err("Pool label accepts at most 128 literal bytes".into());
                }
                setup.label.push(*character);
            }
            (Stage::Label, UiEvent::Backspace) => {
                setup.label.pop();
            }
            (Stage::Label, UiEvent::Submit) => setup.create()?,
            (Stage::Members, UiEvent::SelectionNext | UiEvent::SelectionPrevious) => {
                if !setup.page.is_empty() {
                    let delta = if matches!(event, UiEvent::SelectionNext) {
                        1
                    } else {
                        setup.page.len() - 1
                    };
                    setup.selected = (setup.selected + delta) % setup.page.len();
                }
            }
            (Stage::Members, UiEvent::Character(' ')) => {
                if let Some(record) = setup.page.get(setup.selected) {
                    if setup.members.contains(record.id()) {
                        setup.members.remove(record.id());
                    } else if setup.members.len() < 64 {
                        setup.members.insert(record.id().clone());
                    } else {
                        return Err("A pool admits at most 64 connections".into());
                    }
                }
            }
            (Stage::Members, UiEvent::HistoryFirst | UiEvent::HistoryNext) => {
                setup.cursor = if matches!(event, UiEvent::HistoryNext) {
                    Some(setup.next.clone().ok_or("No further connection page")?)
                } else {
                    None
                };
                setup.stage = Stage::Load;
            }
            (Stage::Members, UiEvent::Submit) => {
                let edit = setup.edit()?;
                let basis = setup.basis.clone();
                self.submit(basis, edit)?;
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn accept_pool_read(
        &mut self,
        query: &OwnerRead,
        data: &OwnerData,
        basis: &Basis,
    ) -> Result<bool, String> {
        if self.pool_setup.is_none() {
            return Ok(false);
        }
        let setup = self.pool_setup.as_mut().unwrap();
        if setup.stage != Stage::Load {
            return Ok(false);
        }
        let OwnerData::ModelAccessPage {
            collection,
            items,
            next,
            ..
        } = data
        else {
            return Err("Pool choices require an actual owner connection page".into());
        };
        if query
            != &(OwnerRead::ModelAccessPage {
                collection: ModelAccessCollection::Connections,
                cursor: setup.cursor.clone(),
                limit: 16,
            })
            || *collection != ModelAccessCollection::Connections
            || items.len() > 16
            || !items
                .iter()
                .all(|record| matches!(record, ModelAccessRecord::Connection { .. }))
            || next.as_ref().is_some_and(|cursor| {
                cursor.collection != *collection
                    || cursor.source_hash != basis.source_hash
                    || cursor.config_id != basis.config_id
                    || cursor.at_revision != basis.revision
                    || items
                        .last()
                        .is_none_or(|last| last.id() != &cursor.after_id)
            })
        {
            return Err("Invalid bounded owner connection choices".into());
        }
        self.basis = Some(basis.clone());
        if basis != &setup.basis {
            setup.stage = Stage::Review;
            self.notice = "Pool source changed; selections retained without saving".into();
        } else {
            setup.page = items.clone();
            setup.next = next.clone();
            setup.selected = 0;
            setup.stage = Stage::Members;
        }
        Ok(true)
    }

    pub(super) fn settle_pool_setup(&mut self, applied: bool) {
        if self.pool_setup.is_some() {
            if applied {
                self.pool_setup = None;
                self.collection = ModelAccessCollection::Pools;
                self.notice = "Account pool saved; reading the current owner page".into();
            } else {
                self.pool_setup.as_mut().unwrap().stage = Stage::Review;
            }
        }
    }
}
