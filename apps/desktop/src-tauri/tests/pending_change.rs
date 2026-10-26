use crazyagent::accounts::{AccountDraft, Provider, SignInMethod};
use crazyagent::config::{AccountPool, ModelAccessEdit, ProviderQuotaPolicy};
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::ModelAccessCollection;
use crazyagent::owner_profile::OwnerProfile;
use crazyagent::protocol::{CredentialRef, ProjectSpec};
use crazyagent_desktop::bridge::{ActionOutcome, IntentReceipt, NativeBridge};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

struct Case {
    root: tempfile::TempDir,
    service: EngineService,
    bridge: NativeBridge,
    member: CredentialRef,
}

impl Case {
    async fn start() -> TestResult<Self> {
        let root = tempfile::Builder::new()
            .prefix("ca-dpc-")
            .tempdir_in("/private/tmp")?;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
        let global = root.path().join("global.toml");
        std::fs::write(&global, "version = 1\ntimeout_ms = 1000\n")?;
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600))?;
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await?;
        let mut bridge = NativeBridge::new(Some(root.path().join("e.sock")));
        bridge.connect().await?;
        let member = bridge
            .add_account(
                AccountDraft {
                    provider: Provider::Openrouter,
                    method: SignInMethod::ApiKey,
                    name: "Pool member".into(),
                    url: None,
                    model: Some("openai/gpt-4o".into()),
                },
                Some(format!("sk-dpc-{}", std::process::id())),
            )
            .await?
            .id;
        Ok(Self {
            root,
            service,
            bridge,
            member,
        })
    }

    fn sqlite(&self, sql: &str) -> TestResult<String> {
        let result = crazyagent::spawn_gate::output(
            std::process::Command::new("/usr/bin/sqlite3")
                .arg(self.root.path().join("engine.db"))
                .arg(sql)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()),
        )?;
        if !result.status.success() {
            return Err("actual SQLite command failed".into());
        }
        Ok(String::from_utf8(result.stdout)?.trim().into())
    }

    async fn create_pool(&mut self, id: &str, generation: u64) -> TestResult<IntentReceipt> {
        let page = self
            .bridge
            .model_access_page(ModelAccessCollection::Pools, false)
            .await?;
        let pool = AccountPool {
            id: CredentialRef::new(id)?,
            name: None,
            members: vec![self.member.clone()],
            projects: Vec::new(),
            roles: Vec::new(),
            fallback_allowed: false,
            provider_quota_policy: ProviderQuotaPolicy::BoundedAttempt,
        };
        Ok(self
            .bridge
            .edit_model_access(
                &page.ticket,
                ModelAccessEdit::CreatePool { pool },
                generation,
            )
            .await?)
    }

    async fn close(mut self) {
        let cleanup = tokio::time::timeout(Duration::from_secs(5), self.service.shutdown()).await;
        assert!(matches!(cleanup, Ok(Ok(()))), "actual engine cleanup");
    }
}

fn profile(name: &str) -> OwnerProfile {
    OwnerProfile {
        name: name.into(),
        role: String::new(),
        bio: String::new(),
        preferred_stack: Vec::new(),
        working_style: Default::default(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_change_the_engine_never_recorded_is_kept_for_retry_and_the_next_change_saves() {
    let mut case = Case::start().await.unwrap();
    let result: TestResult<()> = async {
        case.sqlite(
            "CREATE TRIGGER lb9_abort BEFORE INSERT ON owner_operations \
             BEGIN SELECT RAISE(ABORT, 'lb9_abort'); END;",
        )?;
        let sent = case
            .bridge
            .save_owner_profile(profile("Sent again"), 1)
            .await?;
        assert_eq!(sent.outcome, ActionOutcome::Unknown);
        assert!(sent.pending);
        case.sqlite("DROP TRIGGER lb9_abort;")?;
        let checked = case.bridge.reconcile_intent().await?;
        assert_eq!(checked.outcome, ActionOutcome::NotRecordedAtBarrier);
        assert!(
            checked.pending,
            "a change not recorded at the barrier is kept for an exact retry"
        );
        assert_eq!(
            case.bridge.pending_intent()?.map(|intent| intent.kind),
            Some("owner_profile")
        );
        let blocked = case.create_pool("while-kept", 2).await.err().unwrap();
        assert!(blocked.to_string().contains("pending_intent"));
        let retried = case.bridge.retry_intent().await?;
        assert_eq!(retried.outcome, ActionOutcome::Applied);
        assert!(!retried.pending);
        let pool = case.create_pool("after-retry", 3).await?;
        assert_eq!(pool.outcome, ActionOutcome::Applied);
        assert!(!pool.pending);
        assert_eq!(
            case.bridge.owner_profile().await?.map(|saved| saved.name),
            Some("Sent again".to_owned())
        );
        Ok(())
    }
    .await;
    case.close().await;
    result.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_change_the_engine_refused_on_its_first_send_is_dropped_and_the_next_change_saves()
{
    let mut case = Case::start().await.unwrap();
    let result: TestResult<()> = async {
        let page = case
            .bridge
            .model_access_page(ModelAccessCollection::Pools, false)
            .await?;
        case.sqlite("UPDATE owner_meta SET runtime_state='offline' WHERE singleton=1;")?;
        let refused = case
            .bridge
            .edit_model_access(
                &page.ticket,
                ModelAccessEdit::CreatePool {
                    pool: AccountPool {
                        id: CredentialRef::new("refused")?,
                        name: None,
                        members: vec![case.member.clone()],
                        projects: Vec::new(),
                        roles: Vec::new(),
                        fallback_allowed: false,
                        provider_quota_policy: ProviderQuotaPolicy::BoundedAttempt,
                    },
                },
                1,
            )
            .await?;
        case.sqlite("UPDATE owner_meta SET runtime_state='active' WHERE singleton=1;")?;
        assert_eq!(refused.outcome, ActionOutcome::Rejected);
        assert!(
            !refused.pending,
            "a change the engine refused on its only send is not kept as pending"
        );
        assert!(case.bridge.pending_intent()?.is_none());
        let pool = case.create_pool("after-refusal", 2).await?;
        assert_eq!(pool.outcome, ActionOutcome::Applied);
        assert!(!pool.pending);
        let pools = case
            .bridge
            .model_access_page(ModelAccessCollection::Pools, false)
            .await?;
        let ids: Vec<String> = pools
            .items
            .iter()
            .map(|item| item.id().as_str().to_owned())
            .collect();
        assert_eq!(ids, vec!["after-refusal".to_owned()]);
        Ok(())
    }
    .await;
    case.close().await;
    result.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_change_saved_after_its_reply_was_lost_settles_on_the_next_check() {
    let mut case = Case::start().await.unwrap();
    let result: TestResult<()> = async {
        case.sqlite(
            "CREATE TABLE lb9_rows(x INTEGER); \
             WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 800) \
             INSERT INTO lb9_rows SELECT x FROM n; \
             CREATE TABLE lb9_once(x INTEGER); \
             CREATE TRIGGER lb9_slow AFTER INSERT ON owner_operations \
             WHEN NOT EXISTS (SELECT 1 FROM lb9_once) \
             BEGIN INSERT INTO lb9_once VALUES (1); \
             SELECT count(*) FROM lb9_rows a, lb9_rows b, lb9_rows c; END;",
        )?;
        let sent = case
            .bridge
            .save_owner_profile(profile("Saved late"), 1)
            .await?;
        assert!(
            matches!(
                sent.outcome,
                ActionOutcome::Unknown | ActionOutcome::Uncertain
            ),
            "the reply was lost while the engine kept saving: {:?}",
            sent.outcome
        );
        assert!(sent.pending);
        let mut committed = false;
        for _ in 0..300 {
            if case
                .sqlite("SELECT COUNT(*) FROM lb9_once;")
                .ok()
                .as_deref()
                == Some("1")
            {
                committed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert!(committed, "the engine finished the slow save");
        if !case.bridge.status().connected {
            assert!(case.bridge.connect().await?.pending);
        }
        let checked = case.bridge.reconcile_intent().await?;
        assert_eq!(checked.outcome, ActionOutcome::Applied);
        assert!(!checked.pending);
        let pool = case.create_pool("after-lost-reply", 2).await?;
        assert_eq!(pool.outcome, ActionOutcome::Applied);
        assert_eq!(
            case.bridge.owner_profile().await?.map(|saved| saved.name),
            Some("Saved late".to_owned())
        );
        Ok(())
    }
    .await;
    case.close().await;
    result.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_refused_creation_frees_the_next_creation_and_master_change() {
    let mut case = Case::start().await.unwrap();
    let result: TestResult<()> = async {
        let spec = ProjectSpec {
            name: "Refused first".into(),
            repositories: vec![
                case.root.path().join("absent-one").display().to_string(),
                case.root.path().join("absent-two").display().to_string(),
            ],
        };
        case.bridge.projects(false).await?;
        case.sqlite("UPDATE owner_meta SET runtime_state='offline' WHERE singleton=1;")?;
        let refused = case.bridge.create_project(spec.clone(), 1).await;
        case.sqlite("UPDATE owner_meta SET runtime_state='active' WHERE singleton=1;")?;
        let refused = refused?;
        assert_eq!(refused.outcome, ActionOutcome::Rejected);
        assert!(
            !refused.pending,
            "a creation the engine refused on its only send is not kept as pending"
        );
        assert!(case.bridge.pending_intent()?.is_none());

        case.bridge.projects(false).await?;
        let created = case.bridge.create_project(spec, 2).await?;
        assert_eq!(created.outcome, ActionOutcome::Applied);
        assert!(!created.pending);
        let project_id = created
            .intent
            .project_id
            .ok_or("actual project receipt required")?;
        let detail = case.bridge.open_project(&project_id).await?;
        let master = case
            .bridge
            .close_master(&detail.ticket, "absent-session", 3)
            .await?;
        assert!(
            !master.pending,
            "the master change reaches the engine instead of waiting on the refused creation"
        );
        Ok(())
    }
    .await;
    case.close().await;
    result.unwrap();
}
