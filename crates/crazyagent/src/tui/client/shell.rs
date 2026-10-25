use super::*;
use crate::config::{AccountPool, ModelAccessEdit};
use crate::engine::EngineError;
use crate::engine::owner::{ModelAccessCollection, ModelAccessRecord, OwnerResult};
use crate::memory::KnowledgeStatus;
use crate::protocol::CredentialRef;
use crate::protocol::{Outcome, StoredProject};
use crate::team::AgentStatus;
use crate::tui::seam::{Activity, PoolChange, PoolScope, ProjectCard, ShellEvent, ShellRequest};

const PROJECTS_MAX: usize = 200;
const POOLS_MAX: usize = 64;
const POOL_PAGE: u8 = 16;
const POOL_PAGES_MAX: usize = POOLS_MAX / POOL_PAGE as usize + 1;
const POOL_PROJECTS_MAX: usize = 64;
const LEADER_ROLE: &str = "project_leader";

/// Every saved pool with the settings version it was read at.
/// Input: read page by page from the engine. Output: the pools and the
/// expected config id and source hash for the first edit.
struct PoolBasis {
    pools: Vec<AccountPool>,
    config_id: ConfigId,
    source_hash: String,
    writable: bool,
}

impl Connection {
    /// Queues one shell request on the paired owner worker.
    /// Input: the request. Output: queued, or why not while another request
    /// is pending.
    pub(in crate::tui) fn shell(&mut self, request: ShellRequest) -> Result<(), String> {
        self.owner_job(Job::Shell(Box::new(request)))
    }
}

impl NativeOwner {
    /// Runs one shell request through the owner controller; an engine
    /// transport failure detaches the trusted connection so the shell
    /// reconnects.
    /// Input: the request. Output: the event with data or a bounded plain
    /// problem code.
    pub(super) async fn shell(&mut self, request: ShellRequest) -> ShellEvent {
        let mut lost = false;
        let event = self.shell_event(request, &mut lost).await;
        if lost {
            self.detach();
        }
        event
    }

    async fn shell_event(&mut self, request: ShellRequest, lost: &mut bool) -> ShellEvent {
        match request {
            ShellRequest::Profile => ShellEvent::Profile(match self.ready_controller() {
                Ok(controller) => controller
                    .owner_profile()
                    .await
                    .map_err(|error| transport(&error, lost)),
                Err(error) => Err(error),
            }),
            ShellRequest::SaveProfile {
                profile,
                generation,
            } => ShellEvent::ProfileSaved(match self.ready_controller() {
                Ok(controller) => match controller.save_owner_profile(profile, generation).await {
                    Ok(response) => settled(response, "profile_outcome_unknown"),
                    Err(error) => Err(transport(&error, lost)),
                },
                Err(error) => Err(error),
            }),
            ShellRequest::Pools => ShellEvent::Pools(match self.ready_controller() {
                Ok(controller) => controller
                    .pool_summaries()
                    .await
                    .map_err(|error| transport(&error, lost)),
                Err(error) => Err(error),
            }),
            ShellRequest::PoolProjects => ShellEvent::PoolProjects(match self.ready_controller() {
                Ok(controller) => pool_basis(controller, lost).await.map(|basis| {
                    basis
                        .pools
                        .into_iter()
                        .map(|pool| PoolScope {
                            pool: pool.id,
                            projects: pool.projects,
                        })
                        .collect()
                }),
                Err(error) => Err(error),
            }),
            ShellRequest::Projects => ShellEvent::Projects(self.project_cards(lost).await),
            ShellRequest::EditPool { change, generation } => {
                ShellEvent::PoolEdited(self.edit_pool(change, generation, lost).await)
            }
        }
    }

    async fn edit_pool(
        &mut self,
        change: PoolChange,
        generation: u64,
        lost: &mut bool,
    ) -> Result<(), String> {
        let controller = self.ready_controller()?;
        if controller.pending().is_some() {
            return Err("controller_pending_intent".into());
        }
        let basis = pool_basis(controller, lost).await?;
        if !basis.writable {
            return Err("model_access_read_only".into());
        }
        let edits = pool_edits(change, &basis.pools)?;
        for (index, edit) in edits.into_iter().enumerate() {
            let (config_id, source_hash) = if index == 0 {
                (basis.config_id.clone(), basis.source_hash.clone())
            } else {
                unchanged(controller, &edit, &basis.pools, lost).await?
            };
            controller
                .begin_mutation(
                    OwnerMutation::ModelAccess {
                        expected_config_id: config_id,
                        expected_source_hash: source_hash,
                        edit,
                    },
                    generation,
                )
                .map_err(|error| bounded_problem(&error.0))?;
            match controller.send_pending().await {
                Ok(response) => settled(response, "pool_outcome_unknown")?,
                Err(error) => return Err(transport(&error, lost)),
            }
        }
        Ok(())
    }

    fn ready_controller(&mut self) -> Result<&mut OwnerController, String> {
        if !self.ready {
            return Err("trusted_connection_unavailable".into());
        }
        self.controller
            .as_mut()
            .ok_or_else(|| "trusted_connection_unavailable".into())
    }

    /// Reads one project's working team agents and suggested memories for its
    /// Home card; a read that fails leaves its part unknown or zero.
    /// Inputs: the owner controller and the project. Output: the activity.
    async fn activity(controller: &mut OwnerController, project: &ProjectId) -> Activity {
        let working = controller
            .team_status(project.clone())
            .await
            .ok()
            .map(|team| {
                team.agents
                    .iter()
                    .filter(|agent| agent.status == AgentStatus::Working)
                    .count()
            });
        let suggestions = controller
            .project_memories(project.clone(), None)
            .await
            .map_or(0, |entries| {
                entries
                    .iter()
                    .filter(|entry| {
                        entry.status == KnowledgeStatus::Provisional && !entry.human_reviewed
                    })
                    .count()
            });
        Activity {
            working,
            suggestions,
        }
    }

    async fn project_cards(&mut self, lost: &mut bool) -> Result<Vec<ProjectCard>, String> {
        let reply = self
            .ordinary()
            .request(WireRequest {
                protocol_version: PROTOCOL_VERSION,
                client_id: ClientId::new("tui-home").map_err(|error| error.to_string())?,
                request_id: RequestId::new(
                    crate::engine::owner::random_id().map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?,
                expected_revision: Cursor::new(0),
                command: Command::Snapshot,
            })
            .await
            .map_err(|error| {
                *lost = true;
                bounded_problem(&error.to_string())
            })?;
        let Outcome::Snapshot { projects, .. } = reply.outcome else {
            return Err("projects_read_failed".into());
        };
        let projects: Vec<StoredProject> = projects.into_iter().take(PROJECTS_MAX).collect();
        let controller = self.ready_controller()?;
        let pools = controller
            .pool_summaries()
            .await
            .map_err(|error| transport(&error, lost))?;
        let mut cards = Vec::with_capacity(projects.len());
        for project in &projects {
            let setup = controller
                .project_setup(project.project_id.clone())
                .await
                .map_err(|error| transport(&error, lost))?;
            let jobs = if setup.is_some() {
                controller
                    .research_jobs(project.project_id.clone())
                    .await
                    .map_err(|error| transport(&error, lost))?
            } else {
                Vec::new()
            };
            let mut card = ProjectCard::build(project, setup.as_ref(), &jobs, &pools);
            if setup.is_some() {
                card.activity = Self::activity(controller, &project.project_id).await;
            }
            cards.push(card);
        }
        cards.sort_by(|left, right| {
            right
                .created_ms
                .cmp(&left.created_ms)
                .then_with(|| left.name.cmp(&right.name))
        });
        assert!(cards.len() <= PROJECTS_MAX);
        Ok(cards)
    }
}

/// A fresh pool id from its name: `pool-<letters>-<random hex>`.
/// Input: the pool name. Output: the id, or the plain problem code.
fn new_pool_id(name: &str) -> Result<CredentialRef, String> {
    let stem: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(24)
        .flat_map(|character| character.to_lowercase())
        .collect();
    let nonce = crate::engine::owner::random_id().map_err(|error| bounded_problem(&error.0))?;
    CredentialRef::new(format!("pool-{stem}-{nonce}"))
        .map_err(|error| bounded_problem(&error.to_string()))
}

/// Reads every saved pool, following engine pages.
/// Inputs: the owner controller and the lost-connection flag. Output: the
/// pools with the settings version of the last page, or the problem code.
async fn pool_basis(
    controller: &mut OwnerController,
    lost: &mut bool,
) -> Result<PoolBasis, String> {
    let mut pools = Vec::new();
    let mut cursor = None;
    for _ in 0..POOL_PAGES_MAX {
        let reply = controller
            .read(OwnerRead::ModelAccessPage {
                collection: ModelAccessCollection::Pools,
                cursor: cursor.take(),
                limit: POOL_PAGE,
            })
            .await
            .map_err(|error| transport(&error, lost))?;
        let OwnerData::ModelAccessPage {
            source_hash,
            writable,
            items,
            next,
            ..
        } = reply.data
        else {
            return Err("pool_read_failed".into());
        };
        for item in items {
            let ModelAccessRecord::Pool { pool } = item else {
                return Err("pool_read_failed".into());
            };
            pools.push(pool);
        }
        if pools.len() > POOLS_MAX {
            return Err("pool_read_failed".into());
        }
        match next {
            Some(next) => cursor = Some(next),
            None => {
                return Ok(PoolBasis {
                    pools,
                    config_id: reply.config_id,
                    source_hash,
                    writable,
                });
            }
        }
    }
    Err("pool_read_failed".into())
}

/// Re-reads the pool a later edit replaces and checks it still matches the
/// first read, so a change made elsewhere meanwhile is never overwritten.
/// Inputs: the controller, the edit, the first read and the lost flag.
/// Output: the config id and source hash to expect, or the problem code.
async fn unchanged(
    controller: &mut OwnerController,
    edit: &ModelAccessEdit,
    pools: &[AccountPool],
    lost: &mut bool,
) -> Result<(ConfigId, String), String> {
    let ModelAccessEdit::ReplacePool { pool } = edit else {
        return Err("pool_edit_order".into());
    };
    let reply = controller
        .read(OwnerRead::ModelAccessRecord {
            collection: ModelAccessCollection::Pools,
            id: pool.id.clone(),
        })
        .await
        .map_err(|error| transport(&error, lost))?;
    let OwnerData::ModelAccessRecord {
        source_hash,
        writable,
        record: ModelAccessRecord::Pool { pool: current },
    } = reply.data
    else {
        return Err("pool_not_found".into());
    };
    if !writable {
        return Err("model_access_read_only".into());
    }
    if pools.iter().find(|before| before.id == pool.id) != Some(&current) {
        return Err("pool_changed".into());
    }
    Ok((reply.config_id, source_hash))
}

/// Builds the model access edits for one pool change.
/// Inputs: the change and every saved pool. Output: the edits in the order
/// they apply, or the plain problem code.
fn pool_edits(change: PoolChange, pools: &[AccountPool]) -> Result<Vec<ModelAccessEdit>, String> {
    Ok(match change {
        PoolChange::Create {
            name,
            members,
            quota,
        } => vec![ModelAccessEdit::CreatePool {
            pool: AccountPool {
                id: new_pool_id(&name)?,
                name: Some(name),
                members,
                projects: Vec::new(),
                roles: Vec::new(),
                fallback_allowed: false,
                provider_quota_policy: quota,
            },
        }],
        PoolChange::Update {
            id,
            name,
            members,
            quota,
        } => {
            let mut pool = pools
                .iter()
                .find(|pool| pool.id == id)
                .cloned()
                .ok_or_else(|| "pool_not_found".to_string())?;
            pool.name = Some(name);
            pool.members = members;
            pool.provider_quota_policy = quota;
            vec![ModelAccessEdit::ReplacePool { pool }]
        }
        PoolChange::Remove { id } => vec![ModelAccessEdit::RemovePool { id }],
        PoolChange::Assign { pool, project } => assign_edits(pools, &pool, &project)?,
    })
}

/// Builds the edits that give a project exactly one pool. The project leaves
/// every other pool first, so a failed step leaves it with no pool, never two.
/// Inputs: every saved pool, the chosen pool and the project. Output: replace
/// edits, the chosen pool last with the project and the leader role; empty
/// when the project already has only this pool.
fn assign_edits(
    pools: &[AccountPool],
    target: &CredentialRef,
    project: &ProjectId,
) -> Result<Vec<ModelAccessEdit>, String> {
    let project =
        CredentialRef::new(project.as_str()).map_err(|_| "project_ref_invalid".to_string())?;
    let leader = CredentialRef::new(LEADER_ROLE).expect("the leader role is a valid reference");
    let mut chosen = pools
        .iter()
        .find(|pool| &pool.id == target)
        .cloned()
        .ok_or_else(|| "pool_not_found".to_string())?;
    if !chosen.projects.contains(&project) && chosen.projects.len() >= POOL_PROJECTS_MAX {
        return Err("pool_project_limit".into());
    }
    let mut replaced: Vec<AccountPool> = pools
        .iter()
        .filter(|pool| &pool.id != target && pool.projects.contains(&project))
        .map(|pool| {
            let mut pool = pool.clone();
            pool.projects.retain(|scope| scope != &project);
            pool
        })
        .collect();
    let before = chosen.clone();
    if !chosen.projects.contains(&project) {
        chosen.projects.push(project);
    }
    if !chosen.roles.contains(&leader) {
        chosen.roles.push(leader);
    }
    if chosen != before {
        replaced.push(chosen);
    }
    let ids: std::collections::BTreeSet<&CredentialRef> =
        replaced.iter().map(|pool| &pool.id).collect();
    assert_eq!(
        ids.len(),
        replaced.len(),
        "each pool is replaced at most once"
    );
    Ok(replaced
        .into_iter()
        .map(|pool| ModelAccessEdit::ReplacePool { pool })
        .collect())
}

/// Reads an owner reply to a mutation; a receipt can still carry a refusal.
/// Inputs: the reply and the code for an outcome that is not known. Output:
/// Ok only when the engine applied the change, else the plain problem code.
fn settled(response: OwnerResponse, unknown: &str) -> Result<(), String> {
    match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            match operation.result {
                OwnerResult::Applied => Ok(()),
                OwnerResult::Rejected { code } => Err(bounded_problem(&code)),
                _ => Err(unknown.into()),
            }
        }
        OwnerResponse::Rejected { code } => Err(bounded_problem(&code)),
        _ => Err(unknown.into()),
    }
}

fn transport(error: &EngineError, lost: &mut bool) -> String {
    *lost = true;
    bounded_problem(&error.0)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::config::ProviderQuotaPolicy;
    use crate::engine::bootstrap::EngineService;
    use crate::tui::ctx::Ctx;
    use crate::tui::glyphs::UNICODE;
    use crate::tui::seam::Arrival;
    use crate::tui::shell::Shell;
    use crate::tui::theme::{Appearance, Colors, Depth};
    use std::os::unix::fs::PermissionsExt;

    const SETTINGS: &str = r#"version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["local-1"]
connections = [
  { id = "local-a", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-1", endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }, model = { id = "qwen2.5:0.5b", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [
  { id = "pool-a", name = "Main", members = ["local-a"], projects = [], roles = [], fallback_allowed = false },
  { id = "pool-b", name = "Spare", members = ["local-a"], projects = [], roles = [], fallback_allowed = false },
]
"#;

    struct Live {
        root: tempfile::TempDir,
        service: EngineService,
        native: NativeOwner,
        _stop: tokio::sync::watch::Sender<bool>,
    }

    impl Live {
        async fn start() -> (Self, OwnerEvent) {
            let root = tempfile::Builder::new()
                .prefix("ca-tui-pools-")
                .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
                .unwrap();
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let global = root.path().join("global.toml");
            std::fs::write(&global, SETTINGS).unwrap();
            std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
            let (stop, stopped) = tokio::sync::watch::channel(false);
            let service = EngineService::start_with_settings_sources(
                root.path(),
                "engine.sock",
                &global,
                &root.path().join("project.toml"),
                &stopped,
            )
            .await
            .unwrap();
            let mut native =
                NativeOwner::new(root.path().join("engine.sock"), Duration::from_secs(5));
            let (_, access) = native.attach().await;
            assert!(matches!(access, OwnerEvent::Access { ready: true, .. }));
            (
                Self {
                    root,
                    service,
                    native,
                    _stop: stop,
                },
                access,
            )
        }

        async fn edit(&mut self, change: PoolChange) -> Result<(), String> {
            match self
                .native
                .shell(ShellRequest::EditPool {
                    change,
                    generation: 1,
                })
                .await
            {
                ShellEvent::PoolEdited(result) => result,
                _ => panic!("a pool edit answers with PoolEdited"),
            }
        }

        async fn scopes(&mut self) -> Vec<(String, Vec<String>)> {
            match self.native.shell(ShellRequest::PoolProjects).await {
                ShellEvent::PoolProjects(Ok(scopes)) => scopes
                    .into_iter()
                    .map(|scope| {
                        (
                            scope.pool.as_str().to_owned(),
                            scope
                                .projects
                                .iter()
                                .map(|p| p.as_str().to_owned())
                                .collect(),
                        )
                    })
                    .collect(),
                _ => panic!("the engine must answer with pool scopes"),
            }
        }

        async fn roles(&mut self, id: &str) -> Vec<String> {
            let controller = self.native.controller.as_mut().unwrap();
            let basis = pool_basis(controller, &mut false).await.unwrap();
            let pool = basis
                .pools
                .iter()
                .find(|pool| pool.id.as_str() == id)
                .unwrap();
            pool.roles
                .iter()
                .map(|role| role.as_str().to_owned())
                .collect()
        }

        async fn create_project(&mut self, id: &str) -> ProjectId {
            let project = ProjectId::new(id).unwrap();
            let controller = self.native.controller.as_mut().unwrap();
            controller.read(OwnerRead::Capabilities).await.unwrap();
            controller
                .begin_mutation(
                    OwnerMutation::CreateProject {
                        project_id: project.clone(),
                        spec: ProjectSpec {
                            name: format!("Project {id}"),
                            repositories: vec![],
                        },
                    },
                    1,
                )
                .unwrap();
            let response = controller.send_pending().await.unwrap();
            assert_eq!(settled(response, "project_outcome_unknown"), Ok(()));
            project
        }

        async fn close(mut self) {
            drop(self.native);
            self.service.shutdown().await.unwrap();
            drop(self.root);
        }
    }

    fn reference(value: &str) -> CredentialRef {
        CredentialRef::new(value).unwrap()
    }

    fn scope(pool: &str, projects: &[&str]) -> (String, Vec<String>) {
        (
            pool.into(),
            projects.iter().map(|p| (*p).to_owned()).collect(),
        )
    }

    fn pool(id: &str, projects: &[&str], roles: &[&str]) -> AccountPool {
        AccountPool {
            id: reference(id),
            name: None,
            members: vec![reference("local-a")],
            projects: projects.iter().map(|p| reference(p)).collect(),
            roles: roles.iter().map(|r| reference(r)).collect(),
            fallback_allowed: false,
            provider_quota_policy: ProviderQuotaPolicy::default(),
        }
    }

    fn replaced(edits: &[ModelAccessEdit]) -> Vec<(String, Vec<String>, Vec<String>)> {
        edits
            .iter()
            .map(|edit| {
                let ModelAccessEdit::ReplacePool { pool } = edit else {
                    panic!("assignment builds only replace edits");
                };
                (
                    pool.id.as_str().to_owned(),
                    pool.projects
                        .iter()
                        .map(|p| p.as_str().to_owned())
                        .collect(),
                    pool.roles.iter().map(|r| r.as_str().to_owned()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn an_assignment_takes_the_project_out_of_other_pools_before_adding_it() {
        let pools = vec![
            pool("pool-a", &["demo", "other"], &["project_leader"]),
            pool("pool-b", &[], &[]),
            pool("pool-c", &["other"], &["project_leader"]),
        ];
        let project = ProjectId::new("demo").unwrap();
        let edits = assign_edits(&pools, &reference("pool-b"), &project).unwrap();
        assert_eq!(
            replaced(&edits),
            vec![
                (
                    "pool-a".into(),
                    vec!["other".into()],
                    vec!["project_leader".into()]
                ),
                (
                    "pool-b".into(),
                    vec!["demo".into()],
                    vec!["project_leader".into()]
                ),
            ]
        );
        let again = vec![
            pool("pool-a", &["other"], &["project_leader"]),
            pool("pool-b", &["demo"], &["project_leader"]),
        ];
        assert!(
            assign_edits(&again, &reference("pool-b"), &project)
                .unwrap()
                .is_empty()
        );
        let doubled = vec![
            pool("pool-a", &["demo"], &["project_leader"]),
            pool("pool-b", &["demo"], &["project_leader"]),
        ];
        assert_eq!(
            replaced(&assign_edits(&doubled, &reference("pool-b"), &project).unwrap()),
            vec![("pool-a".into(), vec![], vec!["project_leader".into()])]
        );
    }

    #[test]
    fn an_assignment_refuses_a_missing_pool_and_a_full_pool() {
        let project = ProjectId::new("demo").unwrap();
        let pools = vec![pool("pool-a", &[], &[])];
        assert_eq!(
            assign_edits(&pools, &reference("pool-x"), &project),
            Err("pool_not_found".into())
        );
        let names: Vec<String> = (0..POOL_PROJECTS_MAX)
            .map(|index| format!("p{index}"))
            .collect();
        let full: Vec<&str> = names.iter().map(String::as_str).collect();
        let pools = vec![pool("pool-a", &full, &["project_leader"])];
        assert_eq!(
            assign_edits(&pools, &reference("pool-a"), &project),
            Err("pool_project_limit".into())
        );
        let inside = ProjectId::new("p3").unwrap();
        assert!(
            assign_edits(&pools, &reference("pool-a"), &inside)
                .unwrap()
                .is_empty(),
            "a project already in a full pool needs no change"
        );
    }

    #[test]
    #[should_panic(expected = "each pool is replaced at most once")]
    fn an_assignment_never_replaces_one_pool_twice() {
        let pools = vec![
            pool("pool-a", &["demo"], &[]),
            pool("pool-a", &["demo"], &[]),
            pool("pool-b", &[], &[]),
        ];
        let _ = assign_edits(
            &pools,
            &reference("pool-b"),
            &ProjectId::new("demo").unwrap(),
        );
    }

    #[test]
    fn an_update_keeps_the_pool_projects_and_roles() {
        let pools = vec![pool("pool-a", &["demo"], &["project_leader"])];
        let edits = pool_edits(
            PoolChange::Update {
                id: reference("pool-a"),
                name: "Renamed".into(),
                members: vec![reference("local-b")],
                quota: ProviderQuotaPolicy::Strict,
            },
            &pools,
        )
        .unwrap();
        let [ModelAccessEdit::ReplacePool { pool }] = edits.as_slice() else {
            panic!("an update builds one replace edit");
        };
        assert_eq!(pool.name.as_deref(), Some("Renamed"));
        assert_eq!(pool.members, vec![reference("local-b")]);
        assert_eq!(pool.projects, vec![reference("demo")]);
        assert_eq!(pool.roles, vec![reference("project_leader")]);
        assert_eq!(pool.provider_quota_policy, ProviderQuotaPolicy::Strict);
        assert_eq!(
            pool_edits(
                PoolChange::Update {
                    id: reference("pool-x"),
                    name: "Gone".into(),
                    members: vec![],
                    quota: ProviderQuotaPolicy::BoundedAttempt,
                },
                &pools,
            ),
            Err("pool_not_found".into())
        );
    }

    #[tokio::test]
    async fn the_shell_keeps_the_owner_identity_while_the_trusted_connection_lasts() {
        let (live, access) = Live::start().await;
        let expected = live.native.controller.as_ref().unwrap().identity().clone();
        let mut shell = Shell::new(Ctx::new(
            Depth::NoColor,
            Colors::Dark,
            Appearance::Dark,
            &UNICODE,
            String::new(),
        ));
        assert_eq!(shell.ctx.identity, None);
        shell.arrive(crate::tui::run::owner_arrival(access));
        assert_eq!(shell.ctx.identity, Some(expected));
        shell.arrive(Arrival::Disconnected("trusted_connection_lost".into()));
        assert_eq!(shell.ctx.identity, None);
        live.close().await;
    }

    #[tokio::test]
    async fn assigning_a_pool_moves_the_project_and_engine_refusals_are_reported() {
        let (mut live, _) = Live::start().await;
        let project = live.create_project("assign-demo").await;
        assert_eq!(
            live.scopes().await,
            vec![scope("pool-a", &[]), scope("pool-b", &[])]
        );
        let assign = |pool: &str| PoolChange::Assign {
            pool: reference(pool),
            project: project.clone(),
        };
        assert_eq!(live.edit(assign("pool-a")).await, Ok(()));
        assert_eq!(
            live.scopes().await,
            vec![scope("pool-a", &["assign-demo"]), scope("pool-b", &[])]
        );
        assert_eq!(
            live.roles("pool-a").await,
            vec!["project_leader".to_owned()]
        );
        assert_eq!(live.edit(assign("pool-b")).await, Ok(()));
        assert_eq!(
            live.scopes().await,
            vec![scope("pool-a", &[]), scope("pool-b", &["assign-demo"])]
        );
        assert_eq!(
            live.roles("pool-b").await,
            vec!["project_leader".to_owned()]
        );
        assert_eq!(
            live.edit(assign("pool-b")).await,
            Ok(()),
            "the same pool again changes nothing"
        );
        assert_eq!(
            live.edit(PoolChange::Remove {
                id: reference("pool-b")
            })
            .await,
            Err("pool_in_use".into()),
            "a refusal inside an owner receipt is not a success"
        );
        assert_eq!(
            live.edit(assign("pool-x")).await,
            Err("pool_not_found".into())
        );
        assert_eq!(
            live.scopes().await,
            vec![scope("pool-a", &[]), scope("pool-b", &["assign-demo"])]
        );
        live.close().await;
    }
}
