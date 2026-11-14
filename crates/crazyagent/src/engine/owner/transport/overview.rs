use super::*;
use crate::project_overview::{self, Apply, Overview, Readme, TAB_BYTES_MAX, Tab};
use crate::protocol::{FRAME_BYTES_MAX, ProjectId, encode_frame};
use crate::roadmap_status::Refresh;
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = 2 * TAB_BYTES_MAX + 1024;
const TEXT_WIRE_BYTES_MAX: usize = FRAME_BYTES_MAX - 4096;
const READ_PASSES_MAX: usize = 2;
pub(super) const OVERVIEW_TIMEOUT: Duration = Duration::from_secs(30);

const _: () = assert!(REQUEST_BYTES_MAX <= FRAME_BYTES_MAX);
const _: () = assert!(2 * TAB_BYTES_MAX <= TEXT_WIRE_BYTES_MAX);

/// Every code an Overview request can answer an owner client with: the tab store's, this
/// protocol's own, and the owner connection's it travels on.
pub const OVERVIEW_CODES: &[&str] = &[
    "overview_needs_project_home",
    "overview_no_proposal",
    "overview_off",
    "overview_project_unknown",
    "overview_proposal_changed",
    "overview_reply_bounds",
    "overview_request_bounds",
    "overview_request_invalid",
    "overview_tab_blocked",
    "overview_tab_changed",
    "overview_tab_empty",
    "overview_tab_too_large",
    "overview_unavailable",
    "owner_authentication_failed",
    "owner_authority_unavailable",
    "owner_busy",
    "owner_grant_stale_or_revoked",
    "owner_request_failed",
    "owner_root_replaced_or_unsafe",
    "owner_root_unavailable",
    "owner_socket_replaced",
    "owner_socket_unsafe",
    "peer_owner_mismatch",
];

/// One text of an Overview, sent in a frame of its own: the README, a tab's file or a tab's
/// proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "part", rename_all = "snake_case", deny_unknown_fields)]
enum Part {
    Readme,
    Tab { tab: Tab },
    Proposal { tab: Tab },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Read {
        project: ProjectId,
    },
    Text {
        project: ProjectId,
        part: Part,
    },
    Refresh {
        project: ProjectId,
    },
    Save {
        project: ProjectId,
        tab: Tab,
        text: String,
        version: String,
    },
    Accept {
        project: ProjectId,
        tab: Tab,
        version: String,
        proposal_version: String,
    },
    Reject {
        project: ProjectId,
        tab: Tab,
    },
    SetRefresh {
        project: ProjectId,
        refresh: Refresh,
    },
    SetApply {
        project: ProjectId,
        apply: Apply,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    overview: Action,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Overview {
        overview: Box<Overview>,
        proposals: Vec<Tab>,
    },
    Text {
        text: Option<String>,
        version: String,
        path: Option<String>,
        cut: bool,
    },
    Done,
    Rejected {
        code: String,
    },
}

/// Names the requests that only read, for the log of replies this client could not read.
/// Input: the request. Output: its name, None for a request that changes something.
fn read_kind(action: &Action) -> Option<&'static str> {
    match action {
        Action::Read { .. } => Some("overview.read"),
        Action::Text { .. } => Some("overview.text"),
        _ => None,
    }
}

fn unavailable() -> EngineError {
    failure("overview_unavailable")
}

impl OwnerClient {
    /// Reads a project's Overview: the README, the four tabs with their text, version and
    /// proposal, the competitor state, the run state and the settings. The engine sends the
    /// state and each text in a frame of its own; a tab whose file changed between two
    /// frames is read again once. Input: the project. Output: the Overview, or an
    /// `OVERVIEW_CODES` refusal.
    pub async fn overview_read(&self, project: ProjectId) -> Result<Overview, EngineError> {
        for _ in 0..READ_PASSES_MAX {
            if let Some(overview) = self.overview_pass(&project).await? {
                return Ok(overview);
            }
        }
        Err(unavailable())
    }

    /// One pass over the state frame and the text frames. Input: the project. Output: the
    /// Overview, or none when a tab's file changed between its state and its text.
    async fn overview_pass(&self, project: &ProjectId) -> Result<Option<Overview>, EngineError> {
        let read = Action::Read {
            project: project.clone(),
        };
        let Reply::Overview {
            mut overview,
            proposals,
        } = self.overview_exchange(read).await?
        else {
            return Err(unavailable());
        };
        if overview.tabs.len() != Tab::ALL.len() || proposals.len() > Tab::ALL.len() {
            return Err(unavailable());
        }
        if let (Some(text), _, Some(path), cut) = self.overview_text(project, Part::Readme).await? {
            overview.readme = Some(Readme { path, text, cut });
        }
        for state in &mut overview.tabs {
            let tab = state.tab;
            if !state.version.is_empty() {
                let (text, version, _, _) = self.overview_text(project, Part::Tab { tab }).await?;
                if text.is_none() || version != state.version {
                    return Ok(None);
                }
                state.text = text;
            }
            if proposals.contains(&tab) {
                let (text, version, _, _) =
                    self.overview_text(project, Part::Proposal { tab }).await?;
                if text.is_none() || version != state.proposal_version {
                    return Ok(None);
                }
                state.proposal = text;
            }
        }
        Ok(Some(*overview))
    }

    /// Reads one text of a project's Overview. Inputs: the project and the part. Output:
    /// the text when there is one, its version, its path (the README's) and whether it
    /// was cut.
    async fn overview_text(
        &self,
        project: &ProjectId,
        part: Part,
    ) -> Result<(Option<String>, String, Option<String>, bool), EngineError> {
        let action = Action::Text {
            project: project.clone(),
            part,
        };
        match self.overview_exchange(action).await? {
            Reply::Text {
                text,
                version,
                path,
                cut,
            } => Ok((text, version, path, cut)),
            _ => Err(unavailable()),
        }
    }

    /// Asks for a writing run of the project's tabs at once (the Refresh button). Input:
    /// the project. Output: unit, or the refusal (`overview_off` when the tabs are off).
    pub async fn overview_refresh(&self, project: ProjectId) -> Result<(), EngineError> {
        self.overview_done(Action::Refresh { project }).await
    }

    /// Saves the owner's edit of a tab at the version the owner saw. Inputs: the project,
    /// the tab, the text and the version. Output: unit, or the refusal
    /// (`overview_tab_changed` at another version).
    pub async fn overview_save(
        &self,
        project: ProjectId,
        tab: Tab,
        text: String,
        version: String,
    ) -> Result<(), EngineError> {
        self.overview_done(Action::Save {
            project,
            tab,
            text,
            version,
        })
        .await
    }

    /// Writes a tab's proposal as its file, when it is still the proposal the owner
    /// reviewed and the file is at the version its diff was made from. Inputs: the project,
    /// the tab, the file's version and the proposal's version, both as read. Output: unit,
    /// or the refusal (`overview_proposal_changed` for a newer proposal).
    pub async fn overview_accept(
        &self,
        project: ProjectId,
        tab: Tab,
        version: String,
        proposal_version: String,
    ) -> Result<(), EngineError> {
        self.overview_done(Action::Accept {
            project,
            tab,
            version,
            proposal_version,
        })
        .await
    }

    /// Drops a tab's proposal; the file is untouched. Inputs: the project and the tab.
    /// Output: unit, or the refusal.
    pub async fn overview_reject(&self, project: ProjectId, tab: Tab) -> Result<(), EngineError> {
        self.overview_done(Action::Reject { project, tab }).await
    }

    /// Stores when the project's tabs are refreshed. Inputs: the project and the choice.
    /// Output: unit, or the refusal.
    pub async fn overview_set_refresh(
        &self,
        project: ProjectId,
        refresh: Refresh,
    ) -> Result<(), EngineError> {
        self.overview_done(Action::SetRefresh { project, refresh })
            .await
    }

    /// Stores what happens to a refreshed tab. Inputs: the project and the choice. Output:
    /// unit, or the refusal.
    pub async fn overview_set_apply(
        &self,
        project: ProjectId,
        apply: Apply,
    ) -> Result<(), EngineError> {
        self.overview_done(Action::SetApply { project, apply })
            .await
    }

    async fn overview_done(&self, action: Action) -> Result<(), EngineError> {
        match self.overview_exchange(action).await? {
            Reply::Done => Ok(()),
            _ => Err(unavailable()),
        }
    }

    async fn overview_exchange(&self, action: Action) -> Result<Reply, EngineError> {
        tokio::time::timeout(self.timeout.max(OVERVIEW_TIMEOUT), async {
            let read = read_kind(&action);
            let frame = encode_frame(&Envelope { overview: action })
                .map_err(|_| failure("overview_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| unavailable())?;
            verify_peer(&stream)?;
            let mut packet = [0; 37];
            packet[..4].copy_from_slice(b"CAO1");
            packet[4] = match self.kind {
                OwnerClientKind::Tui => 0,
                OwnerClientKind::Desktop => 1,
            };
            packet[5..].copy_from_slice(&self.token);
            stream.write_all(&packet).await.map_err(|_| unavailable())?;
            stream.write_all(&frame).await.map_err(|_| unavailable())?;
            let frame = read_frame(&mut stream).await.map_err(|_| unavailable())?;
            match serde_json::from_slice(&frame[4..])
                .map_err(|error| self.skewed_reply(&error, read, unavailable))?
            {
                Reply::Rejected { code } => Err(EngineError(code)),
                reply => Ok(reply),
            }
        })
        .await
        .map_err(|_| unavailable())?
    }
}

/// Builds the refusal frame for an Overview request that was not served. Input: the code.
/// Output: the frame.
pub(super) fn refusal(code: &str) -> Result<Vec<u8>, EngineError> {
    encode_frame(&Reply::Rejected { code: code.into() }).map_err(|_| unavailable())
}

/// Reads an Overview request from its frame body. Input: the bytes. Output: the request,
/// `overview_request_bounds` for a body over the bound or `overview_request_invalid` for
/// one that is not a request, an unknown field included.
fn parse(bytes: &[u8]) -> Result<Action, EngineError> {
    if bytes.len() > REQUEST_BYTES_MAX {
        return Err(failure("overview_request_bounds"));
    }
    let Envelope { overview } =
        serde_json::from_slice(bytes).map_err(|_| failure("overview_request_invalid"))?;
    Ok(overview)
}

/// Whether a text fits one reply frame as JSON. Input: the text. Output: the answer.
fn fits(text: &str) -> bool {
    serde_json::to_string(text).is_ok_and(|wire| wire.len() <= TEXT_WIRE_BYTES_MAX)
}

/// The state frame of an Overview: the texts are taken out, the tabs holding a proposal
/// are listed, and a tab whose text cannot travel in one frame reads as blocked. Input:
/// the Overview as read. Output: the reply.
fn state_reply(mut overview: Overview) -> Reply {
    let mut proposals = Vec::with_capacity(Tab::ALL.len());
    overview.readme = None;
    for tab in &mut overview.tabs {
        if tab.text.take().is_some_and(|text| !fits(&text)) {
            tab.version = String::new();
            tab.edited = false;
            tab.blocked = Some(project_overview::BLOCKED_TOO_LARGE.to_owned());
        }
        if tab.proposal.take().is_some_and(|text| fits(&text)) {
            proposals.push(tab.tab);
        } else {
            tab.proposal_version = String::new();
        }
    }
    assert!(proposals.len() <= Tab::ALL.len());
    assert!(
        overview
            .tabs
            .iter()
            .all(|tab| tab.text.is_none() && tab.proposal.is_none())
    );
    Reply::Overview {
        overview: Box::new(overview),
        proposals,
    }
}

/// The text frame of one part of an Overview as read. The README is cut further, at a
/// line, until it fits the frame. Inputs: the Overview and the part. Output: the reply.
fn text_reply(overview: Overview, part: Part) -> Reply {
    let none = Reply::Text {
        text: None,
        version: String::new(),
        path: None,
        cut: false,
    };
    match part {
        Part::Readme => {
            let Some(mut readme) = overview.readme else {
                return none;
            };
            while !fits(&readme.text) {
                let half = project_overview::cut_at_line(&readme.text, readme.text.len() / 2);
                readme.text.truncate(half);
                readme.cut = true;
            }
            Reply::Text {
                text: Some(readme.text),
                version: String::new(),
                path: Some(readme.path),
                cut: readme.cut,
            }
        }
        Part::Tab { tab } | Part::Proposal { tab } => {
            let Some(found) = overview.tabs.into_iter().find(|state| state.tab == tab) else {
                return none;
            };
            let (text, version) = match part {
                Part::Proposal { .. } => (found.proposal, found.proposal_version),
                _ => (found.text, found.version),
            };
            match text {
                Some(text) if fits(&text) => Reply::Text {
                    text: Some(text),
                    version,
                    path: None,
                    cut: false,
                },
                _ => none,
            }
        }
    }
}

/// Opens the engine database with the Overview tables in place. Input: the engine root.
/// Output: the connection, or `overview_unavailable`.
fn open(root: &Path) -> Result<rusqlite::Connection, EngineError> {
    let db = crate::landing::open(root).map_err(|_| unavailable())?;
    project_overview::initialize(&db)?;
    Ok(db)
}

/// The folder of a project as its stored setup names it. Inputs: the connection and the
/// project. Output: the absolute folder, `overview_project_unknown` without one.
fn folder(db: &rusqlite::Connection, project: &ProjectId) -> Result<PathBuf, EngineError> {
    let folder = crate::project_setup::load_setup(db, project)
        .map_err(|_| unavailable())?
        .ok_or_else(|| failure("overview_project_unknown"))?
        .project_folder();
    if !folder.is_absolute() {
        return Err(failure("overview_project_unknown"));
    }
    Ok(folder)
}

/// Does what one request asks, on a blocking thread. Inputs: the engine root and the
/// request. Output: the reply, or the refusal.
fn act(root: &Path, action: Action) -> Result<Reply, EngineError> {
    let db = open(root)?;
    let now = crate::team::now_ms();
    match action {
        Action::Read { project } => {
            let folder = folder(&db, &project)?;
            project_overview::read(&db, &project, &folder, now).map(state_reply)
        }
        Action::Text { project, part } => {
            let folder = folder(&db, &project)?;
            project_overview::read(&db, &project, &folder, now).map(|read| text_reply(read, part))
        }
        Action::Refresh { project } => {
            folder(&db, &project)?;
            project_overview::refresh(&db, &project).map(|()| Reply::Done)
        }
        Action::Save {
            project,
            tab,
            text,
            version,
        } => {
            let folder = folder(&db, &project)?;
            project_overview::save(&db, &project, &folder, tab, &text, &version)
                .map(|()| Reply::Done)
        }
        Action::Accept {
            project,
            tab,
            version,
            proposal_version,
        } => {
            let folder = folder(&db, &project)?;
            project_overview::accept(&db, &project, &folder, tab, &version, &proposal_version)
                .map(|()| Reply::Done)
        }
        Action::Reject { project, tab } => {
            folder(&db, &project)?;
            project_overview::reject(&db, &project, tab).map(|()| Reply::Done)
        }
        Action::SetRefresh { project, refresh } => {
            folder(&db, &project)?;
            project_overview::set_refresh(&db, &project, refresh).map(|()| Reply::Done)
        }
        Action::SetApply { project, apply } => {
            folder(&db, &project)?;
            project_overview::set_apply(&db, &project, apply).map(|()| Reply::Done)
        }
    }
}

fn checked(
    authority: &Authority,
    packet: &[u8; 37],
    admission: &Admission,
) -> Result<(), EngineError> {
    if &authority.authenticate(packet)? != admission {
        return Err(failure("owner_grant_stale_or_revoked"));
    }
    Ok(())
}

/// Serves one Overview request: it is parsed and bounded first, the grant is checked
/// inside the owner egress barrier, where the work starts on a blocking thread, and the
/// grant is checked again before the reply leaves. Inputs: the stream, the engine, the
/// authority, the handshake packet, the admission and the request bytes. Output: Ok once
/// a reply or a refusal was written.
pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    let answered = match parse(bytes) {
        Ok(action) => {
            let work = {
                let _barrier = engine.owner_egress.write().await;
                checked(authority, packet, admission).and_then(|()| {
                    let root = super::changes::engine_root(authority)?;
                    Ok(tokio::task::spawn_blocking(move || act(&root, action)))
                })
            };
            match work {
                Ok(work) => work.await.unwrap_or_else(|_| Err(unavailable())),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    };
    let _barrier = engine.owner_egress.write().await;
    let reply =
        match answered.and_then(|reply| checked(authority, packet, admission).map(|()| reply)) {
            Ok(reply) => reply,
            Err(error) => Reply::Rejected { code: error.0 },
        };
    let frame = match encode_frame(&reply) {
        Ok(frame) => frame,
        Err(_) => encode_frame(&Reply::Rejected {
            code: "overview_reply_bounds".into(),
        })
        .map_err(|_| unavailable())?,
    };
    write_reply(stream, &frame).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_overview::TabState;

    fn overview(text: Option<&str>, proposal: Option<&str>) -> Overview {
        let tabs = Tab::ALL
            .into_iter()
            .map(|tab| TabState {
                tab,
                text: text.map(str::to_owned),
                version: text.map(|_| "v1".to_owned()).unwrap_or_default(),
                edited: false,
                proposal: proposal.map(str::to_owned),
                proposal_version: proposal.map(|_| "p1".to_owned()).unwrap_or_default(),
                blocked: None,
            })
            .collect();
        Overview {
            readme: Some(Readme {
                path: "README.md".into(),
                text: "# Alpha\n".into(),
                cut: false,
            }),
            tabs,
            competitors: project_overview::Competitors::Missing,
            running: false,
            stale: false,
            checked_ms: None,
            failure: None,
            waiting_for_account: false,
            refresh: Refresh::AfterLanding,
            apply: Apply::AlwaysAsk,
            needs_home: false,
        }
    }

    #[test]
    fn an_overview_request_over_its_bound_or_with_an_unknown_field_is_refused_before_any_write() {
        let frame = format!(
            r#"{{"overview":{{"kind":"save","project":"alpha","tab":"features","text":"{}","version":""}}}}"#,
            "x".repeat(REQUEST_BYTES_MAX)
        );
        assert_eq!(
            parse(frame.as_bytes()).unwrap_err().0,
            "overview_request_bounds"
        );
        assert_eq!(
            parse(br#"{"overview":{"kind":"read","project":"alpha","extra":1}}"#)
                .unwrap_err()
                .0,
            "overview_request_invalid"
        );
        assert!(matches!(
            parse(br#"{"overview":{"kind":"text","project":"alpha","part":{"part":"proposal","tab":"quick_info"}}}"#),
            Ok(Action::Text {
                part: Part::Proposal {
                    tab: Tab::QuickInfo
                },
                ..
            })
        ));
    }

    #[test]
    fn the_largest_overview_goes_out_as_a_state_frame_and_one_frame_for_each_text() {
        let full = "\"".repeat(TAB_BYTES_MAX);
        let read = overview(Some(&full), Some(&full));
        assert!(encode_frame(&read).is_err());
        let Reply::Overview {
            overview: sent,
            proposals,
        } = state_reply(read.clone())
        else {
            panic!("a read answers with the state");
        };
        assert_eq!(proposals, Tab::ALL);
        assert!(sent.readme.is_none());
        assert!(sent.tabs.iter().all(|tab| tab.version == "v1"));
        assert!(encode_frame(&state_reply(read.clone())).is_ok());
        for tab in Tab::ALL {
            for part in [Part::Tab { tab }, Part::Proposal { tab }] {
                let reply = text_reply(read.clone(), part);
                assert!(encode_frame(&reply).is_ok());
                let Reply::Text { text, version, .. } = reply else {
                    panic!("a text request answers with a text");
                };
                assert_eq!(text.as_deref(), Some(full.as_str()));
                let wanted = match part {
                    Part::Proposal { .. } => "p1",
                    _ => "v1",
                };
                assert_eq!(version, wanted);
            }
        }
        let Reply::Text {
            text, path, cut, ..
        } = text_reply(read, Part::Readme)
        else {
            panic!("a text request answers with a text");
        };
        assert_eq!(text.as_deref(), Some("# Alpha\n"));
        assert_eq!(path.as_deref(), Some("README.md"));
        assert!(!cut);
    }

    #[test]
    fn a_text_that_cannot_travel_in_one_frame_reads_as_blocked_and_a_readme_is_cut_to_fit() {
        let control = "\u{1}".repeat(TAB_BYTES_MAX);
        let mut read = overview(Some(&control), Some(&control));
        read.readme = Some(Readme {
            path: "README.md".into(),
            text: "\u{1}\u{1}\u{1}\n".repeat(TAB_BYTES_MAX / 4),
            cut: false,
        });
        let Reply::Overview {
            overview: sent,
            proposals,
        } = state_reply(read.clone())
        else {
            panic!("a read answers with the state");
        };
        assert!(proposals.is_empty());
        assert!(sent.tabs.iter().all(|tab| {
            tab.version.is_empty()
                && tab.proposal_version.is_empty()
                && tab.blocked.as_deref() == Some("too_large")
        }));
        let Reply::Text { text, cut, .. } = text_reply(read, Part::Readme) else {
            panic!("a text request answers with a text");
        };
        let text = text.unwrap();
        assert!(cut);
        assert!(!text.is_empty() && text.ends_with('\n'));
        assert!(fits(&text));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_overview_is_read_and_changed_over_the_owner_socket_one_text_a_frame() {
        use super::super::usage_tests::Usage;
        let owner = Usage::start().await;
        let project = ProjectId::new("alpha").unwrap();
        assert_eq!(
            owner
                .client
                .overview_read(project.clone())
                .await
                .unwrap_err()
                .0,
            "overview_project_unknown"
        );
        let folder = owner.root.path().join("alpha");
        std::fs::create_dir_all(folder.join(".crazyagent")).unwrap();
        std::fs::write(folder.join("README.md"), "# Alpha\n").unwrap();
        {
            let db = crate::landing::open(&owner.engine_root).unwrap();
            crate::ide::workspace::tests::add_project(&db, "alpha", &folder);
        }
        let full = format!("{}\n", "\"".repeat(TAB_BYTES_MAX - 1));
        owner
            .client
            .overview_save(project.clone(), Tab::Features, full.clone(), String::new())
            .await
            .unwrap();
        let read = owner.client.overview_read(project.clone()).await.unwrap();
        assert_eq!(read.readme.as_ref().unwrap().text, "# Alpha\n");
        assert!(!read.needs_home);
        let features = read
            .tabs
            .iter()
            .find(|tab| tab.tab == Tab::Features)
            .unwrap();
        assert_eq!(features.text.as_deref(), Some(full.as_str()));
        assert!(features.edited && !features.version.is_empty());
        assert!(
            read.tabs
                .iter()
                .filter(|tab| tab.tab != Tab::Features)
                .all(|tab| tab.text.is_none() && tab.version.is_empty())
        );
        assert_eq!(
            owner
                .client
                .overview_save(
                    project.clone(),
                    Tab::Features,
                    "- late\n".into(),
                    "old".into()
                )
                .await
                .unwrap_err()
                .0,
            "overview_tab_changed"
        );
        assert_eq!(
            owner
                .client
                .overview_accept(
                    project.clone(),
                    Tab::Features,
                    features.version.clone(),
                    String::new()
                )
                .await
                .unwrap_err()
                .0,
            "overview_no_proposal"
        );
        {
            let db = crate::landing::open(&owner.engine_root).unwrap();
            project_overview::offer(&db, &project, &folder, Tab::Features, "- B\n").unwrap();
        }
        let shown = owner.client.overview_read(project.clone()).await.unwrap();
        let reviewed = shown
            .tabs
            .iter()
            .find(|tab| tab.tab == Tab::Features)
            .unwrap();
        assert_eq!(reviewed.proposal.as_deref(), Some("- B\n"));
        assert!(!reviewed.proposal_version.is_empty());
        {
            let db = crate::landing::open(&owner.engine_root).unwrap();
            project_overview::offer(&db, &project, &folder, Tab::Features, "- C\n").unwrap();
        }
        assert_eq!(
            owner
                .client
                .overview_accept(
                    project.clone(),
                    Tab::Features,
                    reviewed.version.clone(),
                    reviewed.proposal_version.clone()
                )
                .await
                .unwrap_err()
                .0,
            "overview_proposal_changed"
        );
        owner
            .client
            .overview_reject(project.clone(), Tab::Features)
            .await
            .unwrap();
        owner
            .client
            .overview_set_apply(project.clone(), Apply::WhenUnedited)
            .await
            .unwrap();
        owner
            .client
            .overview_set_refresh(project.clone(), Refresh::Off)
            .await
            .unwrap();
        assert_eq!(
            owner
                .client
                .overview_refresh(project.clone())
                .await
                .unwrap_err()
                .0,
            "overview_off"
        );
        let read = owner.client.overview_read(project.clone()).await.unwrap();
        assert_eq!(
            (read.apply, read.refresh),
            (Apply::WhenUnedited, Refresh::Off)
        );
        assert_eq!(
            std::fs::read_to_string(folder.join(".crazyagent/overview/features.md")).unwrap(),
            full
        );
    }

    #[test]
    fn overview_codes_cover_every_refusal_of_the_tab_store_and_this_protocol() {
        let sources = [
            include_str!("../../../project_overview.rs"),
            include_str!("overview.rs")
                .split("mod tests {")
                .next()
                .unwrap(),
        ];
        let mut found = 0;
        for code in sources {
            for (at, _) in code.match_indices("\"overview_") {
                let rest = &code[at + 1..];
                let name = &rest[..rest.find('"').unwrap()];
                assert!(
                    OVERVIEW_CODES.contains(&name),
                    "{name} is not in OVERVIEW_CODES"
                );
                found += 1;
            }
        }
        assert!(found >= 12);
        let mut sorted = OVERVIEW_CODES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, OVERVIEW_CODES);
    }
}
