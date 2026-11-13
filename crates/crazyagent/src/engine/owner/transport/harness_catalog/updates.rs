use super::*;
use crate::harnesses::update::{
    self, CHECK_TIME, Roots, UNKNOWN_HARNESS, UpdateSettings, Updates, VERSION_TIME,
};

pub(in crate::engine::owner::transport) const UPDATES_TIMEOUT: Duration =
    Duration::from_secs(VERSION_TIME.as_secs() + CHECK_TIME.as_secs() + 5);
pub(super) const REQUEST_BYTES_MAX: usize = 2 * finding::PATH_BYTES_MAX + 256;
const ID_BYTES_MAX: usize = 64;
const UNAVAILABLE: &str = "harness_update_unavailable";
const QUEUE_THREAD: &str = "harness-install-queue";
const QUEUE_PAUSE: Duration = Duration::from_millis(500);
const QUEUE_STOP: Duration = Duration::from_millis(1);
const QUEUE_WAIT_MAX: Duration =
    Duration::from_secs(update::UPDATE_TIME.as_secs() + VERSION_TIME.as_secs() + 5);

const _: () = assert!(UPDATES_TIMEOUT.as_secs() > VERSION_TIME.as_secs() + CHECK_TIME.as_secs());
const _: () = assert!(REQUEST_BYTES_MAX + 4 <= crate::protocol::FRAME_BYTES_MAX);

fn unknown() -> EngineError {
    failure("harness_update_outcome_unknown")
}

/// Tells whether a harness catalog request is one of the update actions, which may run a
/// harness or a package tool and are served past the owner exchange deadline within
/// `UPDATES_TIMEOUT`. Input: the request body. Output: true for those; false for List,
/// Check and an unreadable or oversized request, which the inline path answers.
pub(in crate::engine::owner::transport) fn slow(bytes: &[u8]) -> bool {
    bytes.len() <= REQUEST_BYTES_MAX
        && serde_json::from_slice::<Envelope>(bytes)
            .is_ok_and(|envelope| !matches!(envelope.harness_catalog, Action::List | Action::Check))
}

fn settings_reply(settings: UpdateSettings) -> WireReply {
    WireReply::Settings {
        mode: settings.mode,
        when: settings.when,
        online: settings.online,
        install: settings.install,
    }
}

impl OwnerClient {
    /// Reads one harness's versions row: how it was installed, its installed version, the
    /// exact update or install line and the last job. With `look` the engine checks for a
    /// newer version now, which can take as long as the installing tool does.
    /// Inputs: the catalog id and whether to check now. Output: the row, or the refusal code.
    pub async fn harness_versions(
        &self,
        id: &str,
        look: bool,
    ) -> Result<HarnessVersions, EngineError> {
        let action = Action::Versions {
            id: id.to_owned(),
            look,
        };
        match self
            .harness_exchange(action, Some("harness_catalog.versions"))
            .await?
        {
            WireReply::Versions { versions } => Ok(*versions),
            _ => Err(failure("harness_catalog_reply_invalid")),
        }
    }

    /// Starts the update of one installed harness; the engine picks the command.
    /// Input: the catalog id, the only thing the request carries. Output: Ok once the update
    /// runs in the background, or the refusal code.
    pub async fn harness_update(&self, id: &str) -> Result<(), EngineError> {
        let action = Action::Update { id: id.to_owned() };
        match self.harness_exchange(action, None).await? {
            WireReply::Started => Ok(()),
            _ => Err(unknown()),
        }
    }

    /// Starts the install of one harness that is not installed; the engine picks the tool
    /// and the package. Input: the catalog id, the only thing the request carries.
    /// Output: Ok once the install runs in the background, or the refusal code.
    pub async fn harness_install(&self, id: &str) -> Result<(), EngineError> {
        let action = Action::Install { id: id.to_owned() };
        match self.harness_exchange(action, None).await? {
            WireReply::Started => Ok(()),
            _ => Err(unknown()),
        }
    }

    /// Starts installing a list of harnesses one after another in the background, the
    /// onboarding picks. Input: the catalog ids, the only thing the request carries.
    /// Output: Ok once the list is queued, or the refusal code.
    pub async fn harness_install_many(&self, ids: &[String]) -> Result<(), EngineError> {
        let action = Action::InstallMany { ids: ids.to_vec() };
        match self.harness_exchange(action, None).await? {
            WireReply::Started => Ok(()),
            _ => Err(unknown()),
        }
    }

    /// Reads the harnesses still waiting in the install queue, in install order.
    /// Output: their catalog ids, or the refusal code.
    pub async fn harness_queued(&self) -> Result<Vec<String>, EngineError> {
        match self
            .harness_exchange(Action::Queued, Some("harness_catalog.queued"))
            .await?
        {
            WireReply::Queued { ids } => Ok(ids),
            _ => Err(failure("harness_catalog_reply_invalid")),
        }
    }

    /// Has the engine look for Codex or Claude Code again and pin it when the engine started
    /// without it: the chosen file, else the same search as engine start. Input: the
    /// catalog id. Output: what was found, or the refusal code.
    pub async fn harness_find(&self, id: &str) -> Result<Found, EngineError> {
        let action = Action::Find { id: id.to_owned() };
        match self.harness_exchange(action, None).await? {
            WireReply::Found { found } => Ok(found),
            _ => Err(unknown()),
        }
    }

    /// Saves the file the owner chose for Codex or Claude Code once it passes every check,
    /// then looks again with it; none clears the choice. Inputs: the catalog id and the
    /// absolute path, at most [`finding::PATH_BYTES_MAX`] bytes. Output: what was found.
    pub async fn harness_choose_path(
        &self,
        id: &str,
        path: Option<&Path>,
    ) -> Result<Found, EngineError> {
        let path = match path {
            Some(path) => Some(
                path.to_str()
                    .filter(|text| text.len() <= finding::PATH_BYTES_MAX)
                    .ok_or_else(|| failure(finding::PATH_INVALID))?
                    .to_owned(),
            ),
            None => None,
        };
        let action = Action::ChoosePath {
            id: id.to_owned(),
            path,
        };
        match self.harness_exchange(action, None).await? {
            WireReply::Found { found } => Ok(found),
            _ => Err(unknown()),
        }
    }

    /// Reads the owner's harness update settings. Output: the settings, or the refusal code.
    pub async fn harness_update_settings(&self) -> Result<UpdateSettings, EngineError> {
        match self
            .harness_exchange(Action::Settings, Some("harness_catalog.settings"))
            .await?
        {
            WireReply::Settings {
                mode,
                when,
                online,
                install,
            } => Ok(UpdateSettings {
                mode,
                when,
                online,
                install,
            }),
            _ => Err(failure("harness_catalog_reply_invalid")),
        }
    }

    /// Saves the owner's harness update settings. Input: the settings.
    /// Output: the settings as stored, or the refusal code.
    pub async fn harness_set_update_settings(
        &self,
        settings: UpdateSettings,
    ) -> Result<UpdateSettings, EngineError> {
        let action = Action::SetSettings {
            mode: settings.mode,
            when: settings.when,
            online: settings.online,
            install: settings.install,
        };
        match self.harness_exchange(action, None).await? {
            WireReply::Settings {
                mode,
                when,
                online,
                install,
            } => Ok(UpdateSettings {
                mode,
                when,
                online,
                install,
            }),
            _ => Err(unknown()),
        }
    }

    /// Sends one harness update action and reads its reply, waiting as long as a check may
    /// take. Inputs: the action and the read's name, None for a change.
    /// Output: the reply, or the engine's refusal code.
    async fn harness_exchange(
        &self,
        action: Action,
        read: Option<&'static str>,
    ) -> Result<WireReply, EngineError> {
        tokio::time::timeout(self.timeout.max(UPDATES_TIMEOUT), async {
            let frame = encode_frame(&Envelope {
                harness_catalog: action,
            })
            .map_err(|_| failure("harness_catalog_request_bounds"))?;
            assert!(frame.len() <= REQUEST_BYTES_MAX + 4);
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("harness_catalog_owner_unavailable"))?;
            verify_peer(&stream)?;
            let mut packet = [0; 37];
            packet[..4].copy_from_slice(b"CAO1");
            packet[4] = match self.kind {
                OwnerClientKind::Tui => 0,
                OwnerClientKind::Desktop => 1,
            };
            packet[5..].copy_from_slice(&self.token);
            stream
                .write_all(&packet)
                .await
                .map_err(|_| failure("harness_catalog_owner_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("harness_catalog_owner_unavailable"))?;
            let reply = read_frame(&mut stream).await.map_err(|_| unknown())?;
            match serde_json::from_slice(&reply[4..])
                .map_err(|error| self.skewed_reply(&error, read, unknown))?
            {
                WireReply::Rejected { code } => Err(EngineError(code)),
                reply => Ok(reply),
            }
        })
        .await
        .map_err(|_| unknown())?
    }
}

/// Opens the engine database with the harness update settings table in place.
/// Input: the engine folder. Output: the connection, or `harness_update_unavailable`.
fn open(root: &Path) -> Result<rusqlite::Connection, EngineError> {
    let db = crate::landing::open(root).map_err(|_| failure(UNAVAILABLE))?;
    update::initialize(&db)?;
    Ok(db)
}

/// Finds the catalog entry of a requested harness with the same search the catalog list
/// uses, so a registered executable wins exactly as in the list. The id only selects a
/// catalog row. Inputs: the engine, the search, the requested id and whether to probe now.
/// Output: the entry, or `harness_update_unknown_harness`.
async fn entry(
    engine: &Arc<Engine>,
    search: Search,
    id: &str,
    check: bool,
) -> Result<HarnessEntry, EngineError> {
    if id.len() > ID_BYTES_MAX {
        return Err(failure(UNKNOWN_HARNESS));
    }
    let spec = crate::harnesses::spec(id).ok_or_else(|| failure(UNKNOWN_HARNESS))?;
    let entries = engine
        .harness_probe
        .entries(search, check)
        .await
        .map_err(|_| failure("harness_catalog_unavailable"))?;
    assert!(!entries.is_empty());
    entries
        .into_iter()
        .find(|entry| entry.id == spec.id)
        .ok_or_else(|| failure(UNKNOWN_HARNESS))
}

/// Runs one blocking piece of harness update work off the request path.
/// Input: the work. Output: its answer, or `harness_update_unavailable` when it could not
/// finish.
async fn blocking(
    work: impl FnOnce() -> Result<WireReply, EngineError> + Send + 'static,
) -> Result<WireReply, EngineError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| failure(UNAVAILABLE))?
}

/// Pins the sign-in and master host of a harness the engine's own job just installed or
/// updated with success, when the engine started without it. It looks with the search set at
/// engine start and a refusal is logged by its code alone. Inputs: the owner runtime and the
/// harness id.
fn pin(owner: &OwnerRuntimeHandle, id: &str) {
    let pinned = owner
        .harness_search()
        .and_then(|search| owner.register_installed_harness(id, &search, None));
    if let Err(error) = pinned {
        eprintln!("{id} got no sign-in host after its install ({})", error.0);
    }
}

/// Checks the grant, then answers one update action. Update and Install probe the machine
/// first and start one background job; when it ends the catalog is probed again, which is
/// what makes the list and every later launch see the new install without a restart.
/// Inputs: the engine, authority, packet, admission, the catalog search and the action.
/// Output: the reply, or the refusal.
async fn answer(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    search: Search,
    action: Action,
) -> Result<WireReply, EngineError> {
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    let root = super::super::changes::engine_root(authority)?;
    let updates = engine.harness_updates.clone();
    let probe = engine.harness_probe.clone();
    let runtime = tokio::runtime::Handle::current();
    let owner = OwnerRuntimeHandle {
        engine: engine.clone(),
        authority: authority.clone(),
    };
    match action {
        Action::List | Action::Check => Err(failure("harness_catalog_request_invalid")),
        Action::Find { id } => {
            if !finding::CHOOSABLE.contains(&id.as_str()) {
                return Err(failure(finding::UNKNOWN_HARNESS));
            }
            let found = owner.look_again(&id)?;
            let _ = probe.entries(search, true).await;
            Ok(WireReply::Found { found })
        }
        Action::ChoosePath { id, path } => {
            if !finding::CHOOSABLE.contains(&id.as_str()) {
                return Err(failure(finding::UNKNOWN_HARNESS));
            }
            let chosen = id.clone();
            blocking(move || choose(&root, &chosen, path.as_deref())).await?;
            let found = owner.look_again(&id)?;
            let _ = probe.entries(search, true).await;
            Ok(WireReply::Found { found })
        }
        Action::Settings => {
            blocking(move || update::settings(&open(&root)?).map(settings_reply)).await
        }
        Action::SetSettings {
            mode,
            when,
            online,
            install,
        } => {
            let settings = UpdateSettings {
                mode,
                when,
                online,
                install,
            };
            blocking(move || {
                update::set_settings(&open(&root)?, settings)?;
                Ok(settings_reply(settings))
            })
            .await
        }
        Action::Versions { id, look } => {
            let entry = entry(engine, search, &id, false).await?;
            blocking(move || {
                let versions = updates.versions(&open(&root)?, &entry, &Roots::engine(&root), look);
                Ok(WireReply::Versions {
                    versions: Box::new(versions),
                })
            })
            .await
        }
        Action::Update { id } => {
            let entry = entry(engine, search.clone(), &id, true).await?;
            blocking(move || {
                let id = entry.id.clone();
                let after = move |updated: bool| {
                    if updated {
                        pin(&owner, &id);
                    }
                    runtime.spawn(async move {
                        let _ = probe.entries(search, true).await;
                    });
                };
                updates
                    .start(&open(&root)?, &entry, &Roots::engine(&root), after)
                    .map(|()| WireReply::Started)
                    .map_err(failure)
            })
            .await
        }
        Action::Install { id } => {
            let entry = entry(engine, search.clone(), &id, true).await?;
            blocking(move || {
                let db = open(&root)?;
                let job = Job {
                    owner,
                    runtime,
                    probe,
                    search,
                    updates,
                };
                job.install(&db, &root, &entry)
                    .map(|()| WireReply::Started)
                    .map_err(failure)
            })
            .await
        }
        Action::InstallMany { ids } => {
            let queue = updates.clone();
            let folder = root.clone();
            blocking(move || {
                queue
                    .queue(&open(&folder)?, &ids)
                    .map(|_| WireReply::Started)
                    .map_err(failure)
            })
            .await?;
            let engine = engine.clone();
            let job = Job {
                owner,
                runtime,
                probe,
                search,
                updates: updates.clone(),
            };
            let left = updates.clone();
            let started = std::thread::Builder::new()
                .name(QUEUE_THREAD.into())
                .spawn(move || {
                    updates.drain(
                        |id| job.install_next(&engine, &root, id),
                        QUEUE_PAUSE,
                        QUEUE_WAIT_MAX,
                    );
                });
            if started.is_err() {
                left.drain(|_| Err(update::START_FAILED), QUEUE_STOP, QUEUE_STOP);
                return Err(failure(update::START_FAILED));
            }
            Ok(WireReply::Started)
        }
        Action::Queued => Ok(WireReply::Queued {
            ids: engine.harness_updates.queued(),
        }),
    }
}

/// What one install job needs past the request: the owner runtime that pins the new
/// harness, the runtime and probe that look again once it ends, the catalog search and the
/// update state that holds the one job slot.
struct Job {
    owner: OwnerRuntimeHandle,
    runtime: tokio::runtime::Handle,
    probe: Arc<crate::harnesses::ProbeCache>,
    search: Search,
    updates: Arc<Updates>,
}

impl Job {
    /// Starts the install of one catalog entry and, once it ends, pins the harness and
    /// probes the catalog again so every later launch sees it. Inputs: the engine database,
    /// the engine folder and the entry. Output: Ok once the install runs, or the refusal code.
    fn install(
        &self,
        db: &rusqlite::Connection,
        root: &Path,
        entry: &HarnessEntry,
    ) -> Result<(), &'static str> {
        let (owner, runtime, probe, search) = (
            self.owner.clone(),
            self.runtime.clone(),
            self.probe.clone(),
            self.search.clone(),
        );
        let id = entry.id.clone();
        let after = move || {
            pin(&owner, &id);
            let entries = runtime.block_on(probe.entries(search, true)).ok()?;
            let found = entries.into_iter().find(|entry| entry.id == id)?;
            found.executable
        };
        self.updates.install(db, entry, &Roots::engine(root), after)
    }

    /// Starts the install of the next queued harness from a fresh catalog probe, on the
    /// queue's own thread. Inputs: the engine, the engine folder and the catalog id.
    /// Output: Ok once the install runs, or the refusal code.
    fn install_next(
        &self,
        engine: &Arc<Engine>,
        root: &Path,
        id: &'static str,
    ) -> Result<(), &'static str> {
        let entry = self
            .runtime
            .block_on(entry(engine, self.search.clone(), id, true))
            .map_err(|_| UNKNOWN_HARNESS)?;
        let db = open(root).map_err(|_| UNAVAILABLE)?;
        assert!(entry.id == id);
        self.install(&db, root, &entry)
    }
}

/// Stores or clears the file the owner chose for one harness. A path is stored only once it
/// passes [`finding::check`] and a capture with every check of an engine-start registration,
/// so a file the engine would refuse is never kept. Inputs: the engine folder, the id and
/// the path text, none to clear. Output: Ok once stored, or the plain refusal code.
fn choose(root: &Path, id: &str, path: Option<&str>) -> Result<WireReply, EngineError> {
    let db = open(root)?;
    finding::initialize(&db)?;
    let Some(text) = path else {
        finding::set_chosen(&db, id, None)?;
        return Ok(WireReply::Started);
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let space = finding::AgentSpace::read(root, &db)?;
    let checked = finding::check(id, Path::new(text), &space, home.as_deref())?;
    let host = crate::engine::RegisteredNativeAuthHost::capture_entry(&checked)?;
    host.verified_path()?;
    drop(host);
    finding::set_chosen(&db, id, Some(&checked))?;
    Ok(WireReply::Started)
}

/// Serves one harness update action: the work is done first, then under the owner barrier
/// the grant is checked again and the reply is written. Nothing but a catalog id and the
/// four setting choices is ever read from the request.
/// Inputs: the connection, engine, authority, packet, admission, catalog search and action.
/// Output: done once the reply is written.
pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    search: Search,
    action: Action,
) -> Result<(), EngineError> {
    let answered = answer(engine, authority, packet, admission, search, action).await;
    let _barrier = engine.owner_egress.write().await;
    let reply = match answered {
        Ok(reply) if &authority.authenticate(packet)? == admission => reply,
        Ok(_) => WireReply::Rejected {
            code: "owner_grant_stale_or_revoked".into(),
        },
        Err(error) => WireReply::Rejected { code: error.0 },
    };
    let frame = encode_frame(&reply).map_err(|_| failure("harness_catalog_response_bounds"))?;
    stream.write_all(&frame).await.map_err(io_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(action: &Action) -> Vec<u8> {
        serde_json::to_vec(&Envelope {
            harness_catalog: action.clone(),
        })
        .unwrap()
    }

    #[test]
    fn list_and_check_keep_their_wire_form_and_stay_on_the_inline_path() {
        assert_eq!(body(&Action::List), br#"{"harness_catalog":"list"}"#);
        assert_eq!(body(&Action::Check), br#"{"harness_catalog":"check"}"#);
        assert!(!slow(br#"{"harness_catalog":"list"}"#));
        assert!(!slow(br#"{"harness_catalog":"check"}"#));
        assert!(!slow(b"not json"));
    }

    #[test]
    fn every_update_action_fits_the_request_bound_and_is_served_past_the_deadline() {
        let id = "x".repeat(ID_BYTES_MAX);
        let actions = [
            Action::Versions {
                id: id.clone(),
                look: true,
            },
            Action::Update { id: id.clone() },
            Action::Install { id },
            Action::InstallMany {
                ids: vec!["x".repeat(ID_BYTES_MAX); crate::harnesses::specs().len()],
            },
            Action::Queued,
            Action::Settings,
            Action::SetSettings {
                mode: update::UpdateMode::CheckAndUpdate,
                when: update::CheckWhen::OnButton,
                online: update::OnlineLookup::Off,
                install: update::InstallFromApp::Off,
            },
            Action::Find {
                id: "claude-code".into(),
            },
            Action::ChoosePath {
                id: "claude-code".into(),
                path: Some("\"".repeat(finding::PATH_BYTES_MAX)),
            },
            Action::ChoosePath {
                id: "codex".into(),
                path: None,
            },
        ];
        for action in &actions {
            let bytes = body(action);
            assert!(bytes.len() <= REQUEST_BYTES_MAX, "{}", bytes.len());
            assert!(slow(&bytes));
        }
    }

    #[test]
    fn choose_path_carries_an_id_and_a_path_and_nothing_else() {
        let bytes = br#"{"harness_catalog":{"choose_path":{"id":"codex","path":"/opt/codex"}}}"#;
        assert!(slow(bytes));
        for extra in ["command", "args", "sha256", "entry"] {
            let bytes = format!(
                r#"{{"harness_catalog":{{"choose_path":{{"id":"codex","path":"/opt/codex","{extra}":"x"}}}}}}"#
            );
            assert!(
                serde_json::from_slice::<Envelope>(bytes.as_bytes()).is_err(),
                "{extra}"
            );
        }
        let find = br#"{"harness_catalog":{"find":{"id":"codex","path":"/x"}}}"#;
        assert!(serde_json::from_slice::<Envelope>(find).is_err());
    }

    #[test]
    fn a_request_carries_a_catalog_id_and_nothing_a_command_could_be_built_from() {
        for extra in ["path", "command", "args", "url", "version", "package"] {
            let bytes =
                format!(r#"{{"harness_catalog":{{"update":{{"id":"codex","{extra}":"x"}}}}}}"#);
            assert!(
                serde_json::from_slice::<Envelope>(bytes.as_bytes()).is_err(),
                "{extra}"
            );
            assert!(!slow(bytes.as_bytes()));
        }
        let oversized = format!(
            r#"{{"harness_catalog":{{"update":{{"id":"{}"}}}}}}"#,
            "x".repeat(REQUEST_BYTES_MAX)
        );
        assert!(!slow(oversized.as_bytes()));
    }
}
