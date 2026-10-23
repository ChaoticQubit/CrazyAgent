use super::*;

mod inputs;
use inputs::Input;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Account,
    Connection,
    Pool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Provider,
    Label,
    Endpoint,
    Model,
    Commit(Part),
    Pending(Part),
    ReadBack(Part),
    Review(Part),
    Inspect,
}

pub(super) struct Setup {
    input: Input,
    intent: Option<Intent>,
    stage: Stage,
}

struct Intent {
    connection: ModelConnection,
    pool: AccountPool,
}

impl Intent {
    fn new(input: &Input) -> Result<Self, String> {
        let nonce = crate::engine::owner::random_id().map_err(|error| error.to_string())?;
        let connection = input.connection(&nonce)?;
        let pool = AccountPool {
            id: inputs::generated_reference(input.provider, &input.label, &nonce, "pool")?,
            name: None,
            members: vec![connection.id.clone()],
            projects: Vec::new(),
            roles: Vec::new(),
            fallback_allowed: false,
            provider_quota_policy: ProviderQuotaPolicy::default(),
        };
        Ok(Self { connection, pool })
    }

    fn edit(&self, part: Part) -> ModelAccessEdit {
        match part {
            Part::Account => ModelAccessEdit::AddAccount {
                account_ref: self.connection.account_ref.clone(),
            },
            Part::Connection => ModelAccessEdit::CreateConnection {
                connection: self.connection.clone(),
            },
            Part::Pool => ModelAccessEdit::CreatePool {
                pool: self.pool.clone(),
            },
        }
    }

    fn record(&self, part: Part) -> ModelAccessRecord {
        match part {
            Part::Account => ModelAccessRecord::Account {
                account_ref: self.connection.account_ref.clone(),
            },
            Part::Connection => ModelAccessRecord::Connection {
                connection: self.connection.clone(),
            },
            Part::Pool => ModelAccessRecord::Pool {
                pool: self.pool.clone(),
            },
        }
    }

    fn query(&self, part: Part) -> OwnerRead {
        OwnerRead::ModelAccessRecord {
            collection: part.collection(),
            id: self.record(part).id().clone(),
        }
    }
}

impl Part {
    fn collection(self) -> ModelAccessCollection {
        match self {
            Self::Account => ModelAccessCollection::Accounts,
            Self::Connection => ModelAccessCollection::Connections,
            Self::Pool => ModelAccessCollection::Pools,
        }
    }
}

impl Setup {
    pub(super) fn render(&self, notice: &str, pending: Option<String>, compact: bool) -> String {
        let mut lines = vec!["Account setup | Sign in to a provider".into()];
        lines.extend(self.input.lines(self.stage));
        lines.push(
            notice
                .chars()
                .take(if compact { 96 } else { 240 })
                .collect(),
        );
        if let Some(pending) = pending {
            lines.push(pending);
        }
        lines.join("\n")
    }

    pub(super) fn assert_consistent(&self) {
        self.input.assert_consistent();
        assert_eq!(
            self.intent.is_none(),
            matches!(
                self.stage,
                Stage::Provider | Stage::Label | Stage::Endpoint | Stage::Model
            )
        );
    }
}

impl State {
    pub(super) fn begin_managed_setup(&mut self) -> Result<(), String> {
        self.writable()?;
        if self.loading() || self.has_pending() {
            return Err("Wait for the current owner metadata operation".into());
        }
        self.managed_setup = Some(Setup {
            input: Input::default(),
            intent: None,
            stage: Stage::Provider,
        });
        self.action = RecordAction::View;
        self.notice = "Choose how to connect your account".into();
        Ok(())
    }

    pub(super) fn reduce_managed_setup(&mut self, event: &UiEvent) -> Result<bool, String> {
        if matches!(
            event,
            UiEvent::Resize(..) | UiEvent::Scroll(_) | UiEvent::FocusNext | UiEvent::FocusPrevious
        ) {
            return Ok(false);
        }
        let result = self.edit_managed_setup(event);
        if let Err(problem) = &result {
            self.notice = problem.clone();
        }
        self.assert_consistent();
        result.map(|()| true)
    }

    fn edit_managed_setup(&mut self, event: &UiEvent) -> Result<(), String> {
        let stage = self.managed_setup.as_ref().unwrap().stage;
        if self.has_pending() || self.loading() {
            self.notice = "Setup retained; wait or Ctrl-R reconcile the original request".into();
            return Ok(());
        }
        match (stage, event) {
            (Stage::Provider | Stage::Label | Stage::Endpoint | Stage::Model, UiEvent::Cancel) => {
                self.managed_setup = None;
                self.notice = "Account setup cancelled; metadata unchanged".into();
            }
            (Stage::Provider, UiEvent::SelectionNext | UiEvent::SelectionPrevious) => {
                self.managed_setup
                    .as_mut()
                    .unwrap()
                    .input
                    .choose(matches!(event, UiEvent::SelectionNext));
            }
            (Stage::Provider, UiEvent::Submit) => {
                self.managed_setup.as_mut().unwrap().stage = Stage::Label;
                self.notice = "Choose a label or leave it blank".into();
            }
            (Stage::Label | Stage::Endpoint | Stage::Model, UiEvent::Character(character)) => {
                self.managed_setup
                    .as_mut()
                    .unwrap()
                    .input
                    .edit(stage, Some(*character))?;
            }
            (Stage::Label | Stage::Endpoint | Stage::Model, UiEvent::Backspace) => {
                self.managed_setup
                    .as_mut()
                    .unwrap()
                    .input
                    .edit(stage, None)?;
            }
            (Stage::Label | Stage::Endpoint | Stage::Model, UiEvent::Submit) => {
                self.confirm_setup_input(stage)?;
            }
            (Stage::Review(_), UiEvent::HistoryFirst) => {
                self.read(OwnerRead::ModelAccessPage {
                    collection: self.collection,
                    cursor: None,
                    limit: 16,
                })?;
            }
            (Stage::Review(part), UiEvent::Submit) => {
                self.writable()?;
                self.managed_setup.as_mut().unwrap().stage = Stage::Commit(part);
            }
            (Stage::Review(_), UiEvent::Cancel) => {
                self.managed_setup = None;
                self.needs_page = true;
                self.notice = "Setup closed; any already committed metadata remains visible".into();
            }
            _ => {}
        }
        Ok(())
    }

    fn confirm_setup_input(&mut self, stage: Stage) -> Result<(), String> {
        self.writable()?;
        let setup = self.managed_setup.as_mut().unwrap();
        if stage == Stage::Label && !setup.input.provider.native() {
            setup.stage = Stage::Endpoint;
        } else if stage == Stage::Endpoint {
            setup.input.validate_endpoint()?;
            setup.stage = Stage::Model;
        } else {
            setup.intent = Some(Intent::new(&setup.input)?);
            setup.stage = Stage::Commit(Part::Account);
            self.notice = "Saving your account setup".into();
        }
        Ok(())
    }

    pub(super) fn request_managed_setup(&mut self) -> Result<(), String> {
        if self.loading() || self.has_pending() {
            return Ok(());
        }
        let Some(setup) = &self.managed_setup else {
            return Ok(());
        };
        let stage = setup.stage;
        match stage {
            Stage::Commit(part) => {
                let edit = setup.intent.as_ref().unwrap().edit(part);
                let result = self.writable().and_then(|basis| self.submit(basis, edit));
                match result {
                    Ok(()) => self.managed_setup.as_mut().unwrap().stage = Stage::Pending(part),
                    Err(problem) => {
                        self.managed_setup.as_mut().unwrap().stage = Stage::Review(part);
                        self.notice =
                            format!("{problem}; setup retained; Ctrl-P refresh, Enter review");
                    }
                }
            }
            Stage::ReadBack(_) | Stage::Inspect => {
                let part = match stage {
                    Stage::ReadBack(part) => part,
                    _ => Part::Connection,
                };
                let query = setup.intent.as_ref().unwrap().query(part);
                self.collection = part.collection();
                self.items.clear();
                self.selected = 0;
                self.next = None;
                self.read(query)?;
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn settle_managed_setup(&mut self, applied: bool) {
        if let Some(setup) = &mut self.managed_setup
            && let Stage::Pending(part) = setup.stage
        {
            setup.stage = if applied {
                Stage::ReadBack(part)
            } else {
                Stage::Review(part)
            };
            if !applied {
                self.notice
                    .push_str("; generated setup retained; Ctrl-P refresh, Enter review");
            }
        }
    }

    pub(super) fn accept_managed_read(
        &mut self,
        query: &OwnerRead,
        data: &OwnerData,
        basis: &Basis,
    ) -> Result<bool, String> {
        let Some(setup) = &self.managed_setup else {
            return Ok(false);
        };
        let (part, inspect) = match setup.stage {
            Stage::ReadBack(part) => (part, false),
            Stage::Inspect => (Part::Connection, true),
            _ => return Ok(false),
        };
        let intent = setup.intent.as_ref().unwrap();
        let OwnerData::ModelAccessRecord { record, .. } = data else {
            return Err("Setup requires an actual owner record read-back".into());
        };
        if query != &intent.query(part) || record != &intent.record(part) || !basis.writable {
            self.managed_setup.as_mut().unwrap().stage = Stage::Review(part);
            self.basis = Some(basis.clone());
            self.notice =
                "Setup read-back changed or is read-only; retained for explicit review".into();
            return Ok(true);
        }
        self.basis = Some(basis.clone());
        self.record = Some(record.clone());
        if inspect {
            let connection = intent.connection.clone();
            self.finish_account_setup(basis, connection)?;
        } else {
            self.managed_setup.as_mut().unwrap().stage = match part {
                Part::Account => Stage::Commit(Part::Connection),
                Part::Connection => Stage::Commit(Part::Pool),
                Part::Pool => Stage::Inspect,
            };
        }
        Ok(true)
    }
    fn finish_account_setup(
        &mut self,
        basis: &Basis,
        connection: ModelConnection,
    ) -> Result<(), String> {
        if native_auth::supported(&connection) {
            self.native_auth.open(basis.clone(), connection)?;
            self.notice = "Account setup saved; choose your sign-in method".into();
        } else {
            let api_key = matches!(connection.auth, ModelAuthentication::ApiKey { .. });
            self.items = vec![ModelAccessRecord::Connection { connection }];
            self.selected = 0;
            self.collection = ModelAccessCollection::Connections;
            self.managed_setup = None;
            self.notice = match self.open_profile() {
                Ok(()) if api_key => "Account saved. Type the API key, then press Enter to finish setup".into(),
                Ok(()) => "Account saved. Press Enter to finish local endpoint setup".into(),
                Err(_) => concat!(
                    "Account and Unassigned pool saved. Enter reads the connection; ",
                    "k opens private profile setup. No authentication or model reply verified."
                )
                .into(),
            };
        }
        self.managed_setup = None;
        self.needs_page = true;
        Ok(())
    }
}
